use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
    response::Response,
};
use futures_util::StreamExt;
use metiva::{AppState, access, app, open};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

struct Dashboard {
    app: Router,
    state: AppState,
    _dir: TempDir,
    cookie: String,
    csrf: String,
}
impl Dashboard {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = open(dir.path(), "https://mirror.example").await.unwrap();
        access::initialize(&state.db, dir.path()).await.unwrap();
        let secret = std::fs::read_to_string(dir.path().join("bootstrap-secret")).unwrap();
        let app = app(state.clone());
        let request=Request::builder().method(Method::POST).uri("/api/admin/setup")
            .header("content-type","application/json").body(Body::from(json!({
                "bootstrapSecret":secret.trim(),"username":"operator","password":"example-local-password"
            }).to_string())).unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let (_, session) = reply(response).await;
        let csrf = session["csrfToken"].as_str().unwrap().to_owned();
        Self {
            app,
            state,
            _dir: dir,
            cookie,
            csrf,
        }
    }

    async fn raw(&self, method: Method, path: &str, body: Option<Value>) -> Response {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/admin/{path}"))
            .header("cookie", &self.cookie)
            .header("x-csrf-token", &self.csrf);
        let body = if let Some(value) = body {
            request = request.header("content-type", "application/json");
            Body::from(value.to_string())
        } else {
            Body::empty()
        };
        self.app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>, status: u16) -> Value {
        let (actual, value) = reply(self.raw(method, path, body).await).await;
        assert_eq!(actual, status, "{path}: {value}");
        value
    }

    async fn create_runtime(&self) -> i64 {
        let p = self
            .call(
                Method::POST,
                "projects",
                Some(json!({"name":"Tools","slug":"tools"})),
                200,
            )
            .await;
        self.call(
            Method::POST,
            &format!("projects/{}/runtimes", p["id"]),
            Some(json!({"name":"Stable","slug":"stable"})),
            200,
        )
        .await["id"]
            .as_i64()
            .unwrap()
    }
}

async fn reply(response: Response) -> (StatusCode, Value) {
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 1 << 20).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)))
    };
    (parts.status, value)
}

fn upload(version: &str) -> Value {
    json!({"upload":json!({"entries":[{"tag":version,"files":[{"name":"kit.bin","href":"https://downloads.example.org/kit.bin"}]}]}).to_string(),
        "mapping":{"releases_path":"entries","version_path":"tag"}})
}

#[tokio::test]
async fn validate_requests_replace_sources_and_retain_originals() {
    let d = Dashboard::new().await;
    d.call(
        Method::POST,
        "projects",
        Some(json!({"name":"test","slug":"bad/slug"})),
        422,
    )
    .await;
    d.call(
        Method::POST,
        "projects",
        Some(json!({"name":"test","slug":"test","visibility":"inherit"})),
        422,
    )
    .await;
    d.call(Method::GET, "projects/987/runtimes", None, 404)
        .await;
    d.call(Method::POST, "runtimes/987/sync", None, 404).await;
    assert!(d.call(Method::GET, "runtimes/nonnumeric", None, 400).await["error"].is_string());
    let id = d.create_runtime().await;
    let rt = format!("runtimes/{id}");
    d.call(Method::PATCH, &rt, Some(json!({"sync_interval":30})), 422)
        .await;
    d.call(Method::POST, &format!("{rt}/sync"), None, 422).await;
    for bad in [
        json!({}),
        json!({"upload":"{}","url":"https://downloads.example.org/feed"}),
        json!({"upload":"garbage"}),
        json!({"url":"file:///unavailable"}),
    ] {
        d.call(Method::POST, &format!("{rt}/sources"), Some(bad), 422)
            .await;
    }
    let preview = d
        .call(
            Method::POST,
            &format!("{rt}/preview"),
            Some(upload("1.0")),
            200,
        )
        .await;
    assert_eq!(preview["releases"][0]["version"], "1.0");
    assert!(
        d.call(Method::GET, &rt, None, 200).await["sources"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let old = d
        .call(
            Method::POST,
            &format!("{rt}/sources"),
            Some(upload("1.0")),
            200,
        )
        .await["id"]
        .as_i64()
        .unwrap();
    let job = d.call(Method::POST, &format!("{rt}/sync"), None, 200).await;
    assert_eq!(
        job,
        d.call(Method::POST, &format!("{rt}/sync"), None, 200).await
    );
    assert_eq!(
        d.call(Method::GET, "projects", None, 200).await[0]["status"],
        "queued"
    );
    let job = job["id"].as_i64().unwrap();
    sqlx::query("UPDATE jobs SET state='running' WHERE id=?")
        .bind(job)
        .execute(&d.state.db)
        .await
        .unwrap();
    assert_eq!(
        d.call(Method::GET, "projects", None, 200).await[0]["status"],
        "running"
    );
    let source = format!("sources/{old}");
    d.call(Method::PATCH, &source, Some(upload("1.1")), 409)
        .await;
    d.call(Method::DELETE, &source, None, 409).await;
    sqlx::query("INSERT INTO raw_documents(digest,body) VALUES('original',?)")
        .bind(b"original bytes".as_slice())
        .execute(&d.state.db)
        .await
        .unwrap();
    let observation:i64=sqlx::query_scalar("INSERT INTO source_observations(source_id,job_id,page,digest) VALUES(?,?,0,'original') RETURNING id")
        .bind(old).bind(job).fetch_one(&d.state.db).await.unwrap();
    sqlx::query("UPDATE jobs SET state='failed' WHERE id=?")
        .bind(job)
        .execute(&d.state.db)
        .await
        .unwrap();
    d.call(
        Method::PATCH,
        &source,
        Some(json!({"upload":"invalid"})),
        422,
    )
    .await;
    let new = d
        .call(Method::PATCH, &source, Some(upload("1.1")), 200)
        .await["id"]
        .as_i64()
        .unwrap();
    assert_ne!(old, new);
    let info = d.call(Method::GET, &rt, None, 200).await;
    assert_eq!(info["sources"].as_array().unwrap().len(), 1);
    assert_eq!(info["sources"][0]["id"], new);
    assert_eq!(
        d.call(Method::GET, &format!("originals/{observation}"), None, 200)
            .await,
        "original bytes"
    );
    d.call(Method::POST, &format!("{rt}/sync"), None, 200).await;
    d.call(Method::DELETE, &format!("sources/{new}"), None, 204)
        .await;
    d.call(Method::POST, &format!("{rt}/sync"), None, 422).await;
    assert_eq!(
        d.call(Method::GET, &rt, None, 200).await["history"][0]["state"],
        "cancelled"
    );
    d.call(Method::PATCH, &source, Some(upload("old")), 404)
        .await;
}

#[tokio::test]
async fn stream_controls_and_session_revocation() {
    let d = Dashboard::new().await;
    let id = d.create_runtime().await;
    d.call(
        Method::POST,
        &format!("runtimes/{id}/sources"),
        Some(upload("1")),
        200,
    )
    .await;
    let job: i64 =
        sqlx::query_scalar("INSERT INTO jobs(runtime_id,state) VALUES(?,'running') RETURNING id")
            .bind(id)
            .fetch_one(&d.state.db)
            .await
            .unwrap();
    let transfer:i64=sqlx::query_scalar("INSERT INTO transfers(job_id,runtime_id,filename,host,backend,state,source_url) VALUES(?,?,'kit.bin','downloads.example.org','native','downloading','private upstream URL') RETURNING id")
        .bind(job).bind(id).fetch_one(&d.state.db).await.unwrap();
    let response = d.raw(Method::GET, "events", None).await;
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut stream = response.into_body().into_data_stream();
    let first = stream.next().await.unwrap().unwrap();
    let event = std::str::from_utf8(&first).unwrap();
    assert!(
        event.contains("event: transfers")
            && event.contains("kit.bin")
            && !event.contains("private upstream URL")
    );
    let path = format!("transfers/{transfer}");
    d.call(Method::POST, &format!("{path}/resume"), None, 422)
        .await;
    for _ in 0..2 {
        d.call(Method::POST, &format!("{path}/pause"), None, 200)
            .await;
    }
    d.call(Method::POST, &format!("{path}/cancel"), None, 409)
        .await;
    sqlx::query("UPDATE transfers SET state='paused',command=NULL WHERE id=?")
        .bind(transfer)
        .execute(&d.state.db)
        .await
        .unwrap();
    d.call(Method::POST, &format!("{path}/resume"), None, 200)
        .await;
    sqlx::query("UPDATE transfers SET state='failed',command=NULL WHERE id=?")
        .bind(transfer)
        .execute(&d.state.db)
        .await
        .unwrap();
    d.call(Method::POST, &format!("{path}/retry"), None, 409)
        .await;
    sqlx::query("UPDATE jobs SET state='failed' WHERE id=?")
        .bind(job)
        .execute(&d.state.db)
        .await
        .unwrap();
    for _ in 0..2 {
        d.call(Method::POST, &format!("{path}/retry"), None, 200)
            .await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM jobs WHERE state='queued'")
            .fetch_one(&d.state.db)
            .await
            .unwrap(),
        1
    );
    sqlx::query("UPDATE transfers SET bytes=bytes+1,state=state WHERE id=?")
        .bind(transfer)
        .execute(&d.state.db)
        .await
        .unwrap();
    let history = d.call(Method::GET, &path, None, 200).await;
    let states: Vec<_> = history["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["state"].as_str().unwrap())
        .collect();
    assert_eq!(states, ["failed", "paused", "downloading"]);
    d.call(Method::GET, "transfers/987", None, 404).await;
    d.call(Method::POST, "logout", None, 204).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn storage_counts_physical_objects_and_settings_validate_limits() {
    let d = Dashboard::new().await;
    let dir = d.state.data.join("artifacts");
    let published = "a".repeat(64);
    let unpublished = "b".repeat(64);
    std::fs::write(dir.join(&published), [0; 37]).unwrap();
    std::fs::write(dir.join(&unpublished), [0; 9]).unwrap();
    std::fs::write(dir.join("extra.txt"), [0; 10]).unwrap();
    std::os::unix::fs::symlink(dir.join(&published), dir.join("c".repeat(64))).unwrap();
    for path in ["settings", "overview"] {
        assert_eq!(
            d.call(Method::GET, path, None, 200).await["storage_bytes"],
            46
        );
    }
    d.call(
        Method::PATCH,
        "settings",
        Some(
            json!({"engine":"native","concurrency":4,"per_host":5,"retries":0,"sync_interval":300}),
        ),
        422,
    )
    .await;
    let settings=d.call(Method::PATCH,"settings",Some(json!({"engine":"native","concurrency":4,"per_host":2,"retries":0,"sync_interval":300})),200).await;
    assert_eq!(settings["per_host"], 2);
    assert_eq!(settings["sync_interval"], 300);
    let runtime = d.create_runtime().await;
    assert_eq!(
        d.call(Method::GET, &format!("runtimes/{runtime}"), None, 200)
            .await["sync_interval"],
        300
    );
    d.call(Method::GET, "archive", None, 200).await;
}

#[tokio::test]
async fn parking_cancels_work_and_restores_only_explicit_targets() {
    let d = Dashboard::new().await;
    let runtime = d.create_runtime().await;
    let rt = format!("runtimes/{runtime}");
    let project = d.call(Method::GET, &rt, None, 200).await["project_id"]
        .as_i64()
        .unwrap();
    let p = format!("projects/{project}");
    d.call(
        Method::POST,
        &format!("{rt}/sources"),
        Some(upload("v1")),
        200,
    )
    .await;
    d.call(Method::POST, &format!("{rt}/sync"), None, 200).await;
    d.call(Method::POST, &format!("{rt}/park"), None, 200).await;
    d.call(Method::POST, &format!("{rt}/sync"), None, 409).await;
    let view = d.call(Method::GET, &rt, None, 200).await;
    assert_eq!(view["history"][0]["state"], "cancelled");
    assert!(view["parked_at"].is_number());
    assert_eq!(
        d.call(Method::GET, "overview", None, 200).await["runtimes"],
        0
    );
    d.call(Method::POST, &format!("{p}/park"), None, 200).await;
    assert_eq!(d.call(Method::GET, "projects", None, 200).await, json!([]));
    let archive = d.call(Method::GET, "archive", None, 200).await;
    assert_eq!(archive.as_array().unwrap().len(), 2);
    assert!(
        archive
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "runtime" && v["parent_parked"] == true)
    );
    d.call(Method::POST, &format!("{rt}/restore"), None, 409)
        .await;
    d.call(Method::POST, &format!("{p}/restore"), None, 200)
        .await;
    assert_eq!(
        d.call(Method::GET, &format!("{p}/runtimes"), None, 200)
            .await,
        json!([])
    );
    d.call(Method::POST, &format!("{rt}/restore"), None, 200)
        .await;
    assert_eq!(d.call(Method::GET, "archive", None, 200).await, json!([]));
    d.call(Method::POST, &format!("{rt}/sync"), None, 200).await;
    d.call(Method::POST, &format!("{p}/park"), None, 200).await;
    assert!(d.call(Method::GET, &rt, None, 200).await["project_parked_at"].is_number());
    d.call(Method::POST, &format!("{p}/restore"), None, 200)
        .await;
    assert_eq!(
        d.call(Method::GET, "overview", None, 200).await["runtimes"],
        1
    );
}
