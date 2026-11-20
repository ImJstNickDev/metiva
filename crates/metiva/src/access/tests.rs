use super::*;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;
use tower::ServiceExt;

struct Fixture {
    _dir: TempDir,
    state: AppState,
    app: Router,
}
struct Admin {
    cookie: String,
    csrf: String,
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::open(dir.path(), "https://mirror.test")
            .await
            .unwrap();
        initialize(&state.db, dir.path()).await.unwrap();
        let app = router(state.clone()).with_state(state.clone());
        Self {
            _dir: dir,
            state,
            app,
        }
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut req = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let body = match body {
            Some(value) => {
                req = req.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, 1 << 20).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        Reply {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    async fn setup(&self) -> Admin {
        let path = self.state.data.join("bootstrap-secret");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let raw = fs::read_to_string(&path).unwrap();
        let response=self.request(Method::POST,"/api/admin/setup",&[],Some(json!({"username":"owner","password":"example-local-password","bootstrapSecret":raw.trim()}))).await;
        assert_eq!(response.status, 200, "{}", response.body);
        let cookie = response.headers["set-cookie"].to_str().unwrap();
        assert!(
            cookie.contains("HttpOnly")
                && cookie.contains("Secure")
                && cookie.contains("SameSite=Strict")
        );
        assert!(!path.exists());
        assert_eq!(response.body["username"], "owner");
        Admin {
            cookie: cookie.split(';').next().unwrap().into(),
            csrf: response.body["csrfToken"].as_str().unwrap().into(),
        }
    }

    async fn seed(&self) {
        sqlx::raw_sql("INSERT INTO projects(id,name,slug,visibility) VALUES(1,'Public','pub','public'),(2,'Restricted','restricted','private'),(3,'Other','other','private');
            INSERT INTO runtimes(id,project_id,name,slug,visibility) VALUES(1,1,'Stable','stable','inherit'),(2,1,'Internal','internal','private'),(3,2,'Stable','stable','public'),(4,2,'Nightly','nightly','inherit'),(5,3,'Stable','stable','inherit');")
            .execute(&self.state.db).await.unwrap();
        let sha = digest("contents");
        for (id, p, r) in [
            (1, "pub", "stable"),
            (2, "pub", "internal"),
            (3, "restricted", "stable"),
            (4, "restricted", "nightly"),
            (5, "other", "stable"),
        ] {
            let url = format!("https://mirror.test/p/{p}/r/{r}/files/1.0/tool.bin");
            let manifest = json!({"schema_version":1,"project":p,"runtime":r,"releases":[{"version":"1.0","artifacts":[{"filename":"tool.bin","size":8,"sha256":sha,"url":url}]}]});
            sqlx::query(
                "INSERT INTO publications(id,runtime_id,manifest,upstream) VALUES(?,?,?,?)",
            )
            .bind(id)
            .bind(id)
            .bind(manifest.to_string())
            .bind(json!({"file":url}).to_string())
            .execute(&self.state.db)
            .await
            .unwrap();
            sqlx::query("UPDATE runtimes SET active_publication=? WHERE id=?")
                .bind(id)
                .bind(id)
                .execute(&self.state.db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO files(id,publication_id,release,filename,digest,size,source_url) VALUES(?,?,'1.0','tool.bin',?,8,'https://upstream.test/file')").bind(id).bind(id).bind(&sha).execute(&self.state.db).await.unwrap();
        }
    }

    async fn issue(&self, admin: &Admin, scope: Value) -> (String, i64) {
        let response = self
            .request(
                Method::POST,
                "/api/admin/keys",
                &admin.headers(),
                Some(scope),
            )
            .await;
        assert_eq!(response.status, 201, "{}", response.body);
        (
            format!("Bearer {}", response.body["token"].as_str().unwrap()),
            response.body["id"].as_i64().unwrap(),
        )
    }
}
impl Admin {
    fn headers(&self) -> [(&str, &str); 2] {
        [("cookie", &self.cookie), ("x-csrf-token", &self.csrf)]
    }
}

#[tokio::test]
async fn bootstrap_change_recovery_and_rate_limits() {
    let f = Fixture::new().await;
    let initial = f
        .request(Method::GET, "/api/admin/session", &[], None)
        .await;
    assert_eq!(
        initial.body,
        json!({"authenticated":false,"setupRequired":true})
    );
    let foreign = f
        .request(
            Method::POST,
            "/api/admin/login",
            &[("origin", "https://elsewhere.test")],
            Some(json!({"username":"owner","password":"test"})),
        )
        .await;
    assert_eq!(foreign.status, 403);
    let wrong=f.request(Method::POST,"/api/admin/setup",&[],Some(json!({"username":"owner","password":"example-local-password","bootstrapSecret":"incorrect"}))).await;
    assert_eq!(wrong.status, 401);
    let a = f.setup().await;
    let stored: String = sqlx::query_scalar("SELECT password_hash FROM admin_credential")
        .fetch_one(&f.state.db)
        .await
        .unwrap();
    assert!(stored.starts_with("$argon2id$"));
    initialize(&f.state.db, &f.state.data).await.unwrap();
    assert!(!f.state.data.join("bootstrap-secret").exists());
    let replay=f.request(Method::POST,"/api/admin/setup",&[],Some(json!({"username":"owner","password":"another-password","bootstrapSecret":"anything"}))).await;
    assert_eq!(replay.status, 409);
    let no_csrf = f
        .request(
            Method::POST,
            "/api/admin/logout",
            &[("cookie", &a.cookie)],
            None,
        )
        .await;
    assert_eq!(no_csrf.status, 403);
    let login = |password: &str| json!({"username":"owner","password":password});
    let second = f
        .request(
            Method::POST,
            "/api/admin/login",
            &[],
            Some(login("example-local-password")),
        )
        .await;
    assert_eq!(second.status, 200);
    let cookie = second.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let change=f.request(Method::POST,"/api/admin/password",&a.headers(),Some(json!({"currentPassword":"example-local-password","newPassword":"second-local-password"}))).await;
    assert_eq!(change.status, 204);
    for old in [&a.cookie[..], cookie] {
        assert_eq!(
            f.request(Method::GET, "/api/admin/keys", &[("cookie", old)], None)
                .await
                .status,
            401
        );
    }
    let logged = f
        .request(
            Method::POST,
            "/api/admin/login",
            &[],
            Some(login("second-local-password")),
        )
        .await;
    assert_eq!(logged.status, 200);
    let old = logged.headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    recover(&f.state.db, "recovered-local-password")
        .await
        .unwrap();
    assert_eq!(
        f.request(Method::GET, "/api/admin/keys", &[("cookie", old)], None)
            .await
            .status,
        401
    );
    assert_eq!(
        f.request(
            Method::POST,
            "/api/admin/login",
            &[],
            Some(login("recovered-local-password"))
        )
        .await
        .status,
        200
    );
    for _ in 0..8 {
        assert_eq!(
            f.request(
                Method::POST,
                "/api/admin/login",
                &[],
                Some(login(&"z".repeat(257)))
            )
            .await
            .status,
            401
        );
    }
    assert_eq!(
        f.request(
            Method::POST,
            "/api/admin/login",
            &[],
            Some(login("recovered-local-password"))
        )
        .await
        .status,
        429
    );
}

#[tokio::test]
async fn scope_discovery_publication_and_file_handoffs() {
    let f = Fixture::new().await;
    f.seed().await;
    let a = f.setup().await;
    let (key, _) = f
        .issue(
            &a,
            json!({"name":"Stable client","scope":"runtime","runtimeId":3}),
        )
        .await;
    let (project_key, _) = f
        .issue(
            &a,
            json!({"name":"Project client","scope":"project","projectId":2}),
        )
        .await;
    let (global_key, _) = f
        .issue(&a, json!({"name":"All clients","scope":"global"}))
        .await;
    assert_eq!(
        f.request(
            Method::POST,
            "/api/admin/keys",
            &a.headers(),
            Some(json!({"name":"Invalid","scope":"runtime","projectId":1,"runtimeId":3}))
        )
        .await
        .status,
        400
    );
    for (grant, expected) in [
        ("", vec!["pub"]),
        (&key, vec!["pub", "restricted"]),
        (&global_key, vec!["other", "pub", "restricted"]),
    ] {
        let auth = [("authorization", grant)];
        let headers = if grant.is_empty() { &[][..] } else { &auth[..] };
        let reply = f
            .request(Method::GET, "/api/v1/projects", headers, None)
            .await;
        assert_eq!(reply.status, 200);
        let slugs: Vec<_> = reply
            .body
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["slug"].as_str().unwrap())
            .collect();
        assert_eq!(slugs, expected);
    }
    for collection in [
        "/api/v1/projects/restricted/runtimes",
        "/api/v1/projects/unknown/runtimes",
        "/p/restricted/r/stable/manifest.json",
        "/p/unknown/r/unknown/manifest.json",
    ] {
        let reply = f.request(Method::GET, collection, &[], None).await;
        assert_eq!(reply.status, 401);
        assert_eq!(reply.body["error"], "Bearer credential required");
    }
    let grant = [("authorization", key.as_str())];
    let list = f
        .request(
            Method::GET,
            "/api/v1/projects/restricted/runtimes",
            &grant,
            None,
        )
        .await;
    assert_eq!(list.body.as_array().unwrap().len(), 1);
    assert_eq!(list.body[0]["slug"], "stable");
    for endpoint in [
        "/p/{p}/r/{r}/manifest.json",
        "/p/{p}/r/{r}/upstream.json",
        "/p/{p}/r/{r}/files/1.0/tool.bin",
        "/api/v1/projects/{p}/runtimes/{r}/availability",
        "/api/v1/projects/{p}/runtimes/{r}/releases",
        "/api/v1/projects/{p}/runtimes/{r}/releases/1.0/files",
    ] {
        for (p, r, anon, scoped, project) in [
            ("pub", "stable", 200, 200, 200),
            ("pub", "internal", 401, 403, 403),
            ("restricted", "stable", 401, 200, 200),
            ("restricted", "nightly", 401, 403, 200),
            ("other", "stable", 401, 403, 403),
        ] {
            let path = endpoint.replace("{p}", p).replace("{r}", r);
            for (headers, expected) in [
                (&[][..], anon),
                (&grant[..], scoped),
                (&[("authorization", project_key.as_str())][..], project),
                (&[("authorization", global_key.as_str())][..], 200),
            ] {
                let reply = f.request(Method::GET, &path, headers, None).await;
                assert_eq!(reply.status, expected, "{path}: {}", reply.body);
            }
        }
    }
    let path = "/p/restricted/r/stable/files/1.0/tool.bin";
    assert_eq!(
        f.request(Method::GET, path, &a.headers(), None)
            .await
            .status,
        401
    );
    assert_eq!(
        f.request(
            Method::GET,
            &format!("{path}?api_key={}", key.trim_start_matches("Bearer ")),
            &[],
            None
        )
        .await
        .status,
        401
    );
    assert_eq!(
        f.request(Method::GET, "/api/admin/archive/files/3", &grant, None)
            .await
            .status,
        401
    );
    assert_eq!(
        f.request(
            Method::GET,
            "/api/admin/archive/files/3",
            &a.headers(),
            None
        )
        .await
        .status,
        200
    );
    for method in [Method::HEAD, Method::GET] {
        let reply = f
            .request(
                method,
                path,
                &[("authorization", &key), ("range", "bytes=0-1")],
                None,
            )
            .await;
        assert_eq!(reply.status, 200);
        assert_eq!(
            reply.headers["x-accel-redirect"].to_str().unwrap(),
            format!("/_objects/{}", digest("contents"))
        );
        assert!(reply.body.is_null());
    }
    assert_eq!(
        f.request(Method::GET, "/api/admin/keys", &grant, None)
            .await
            .status,
        401
    );
    assert_eq!(
        f.request(
            Method::GET,
            "/p/pub/r/stable/manifest.json",
            &[("authorization", "Bearer malformed")],
            None
        )
        .await
        .status,
        401
    );
}

#[tokio::test]
async fn rotate_rename_and_revoke_without_exposing_tokens() {
    let f = Fixture::new().await;
    let a = f.setup().await;
    let (old, id) = f
        .issue(&a, json!({"name":"Downloads","scope":"global"}))
        .await;
    let key_path = format!("/api/admin/keys/{id}");
    let renamed = f
        .request(
            Method::PATCH,
            &key_path,
            &a.headers(),
            Some(json!({"name":"Builders"})),
        )
        .await;
    assert_eq!(renamed.body["name"], "Builders");
    let rotated = f
        .request(
            Method::POST,
            &format!("{key_path}/rotate"),
            &a.headers(),
            None,
        )
        .await;
    assert_eq!(rotated.status, 201);
    let new = format!("Bearer {}", rotated.body["token"].as_str().unwrap());
    assert_ne!(old, new);
    for token in [&old, &new] {
        assert_eq!(
            f.request(
                Method::GET,
                "/api/v1/projects",
                &[("authorization", token)],
                None
            )
            .await
            .status,
            200
        );
    }
    let listed = f
        .request(Method::GET, "/api/admin/keys", &a.headers(), None)
        .await;
    for row in listed.body.as_array().unwrap() {
        assert!(row.get("token").is_none() && row.get("digest").is_none());
        assert!(row["lastUsedAt"].is_i64());
    }
    assert_eq!(
        f.request(Method::DELETE, &key_path, &a.headers(), None)
            .await
            .status,
        204
    );
    assert_eq!(
        f.request(
            Method::GET,
            "/api/v1/projects",
            &[("authorization", &old)],
            None
        )
        .await
        .status,
        401
    );
    assert_eq!(
        f.request(
            Method::GET,
            "/api/v1/projects",
            &[("authorization", &new)],
            None
        )
        .await
        .status,
        200
    );
    let logout = f
        .request(Method::POST, "/api/admin/logout", &a.headers(), None)
        .await;
    assert_eq!(logout.status, 204);
    assert_eq!(
        f.request(Method::GET, "/api/admin/keys", &a.headers(), None)
            .await
            .status,
        401
    );
}

#[tokio::test]
async fn contract_matches_routes_and_only_active_files_are_public() {
    let f = Fixture::new().await;
    f.seed().await;
    let admin = f.setup().await;
    let spec = f
        .request(Method::GET, "/api/v1/openapi.json", &[], None)
        .await;
    assert_eq!(spec.status, 200);
    for (route, op) in spec.body["paths"].as_object().unwrap() {
        let path = route
            .replace("{project}", "pub")
            .replace("{runtime}", "stable")
            .replace("{release}", "1.0")
            .replace("{artifact}", "tool.bin");
        assert_eq!(
            f.request(Method::GET, &path, &[], None).await.status,
            200,
            "{path}"
        );
        assert_eq!(
            f.request(Method::HEAD, &path, &[], None).await.status,
            200,
            "{path}"
        );
        assert!(op["get"]["responses"]["200"].is_object());
    }
    sqlx::raw_sql("INSERT INTO publications(id,runtime_id,manifest) VALUES(6,1,'{\"schema_version\":1,\"project\":\"pub\",\"runtime\":\"stable\",\"releases\":[]}'); UPDATE runtimes SET active_publication=6 WHERE id=1;")
        .execute(&f.state.db).await.unwrap();
    assert_eq!(
        f.request(Method::GET, "/p/pub/r/stable/files/1.0/tool.bin", &[], None)
            .await
            .status,
        404
    );
    assert_eq!(
        f.request(Method::GET, "/p/pub/r/stable/upstream.json", &[], None)
            .await
            .status,
        404
    );
    assert_eq!(
        f.request(
            Method::GET,
            "/api/admin/archive/files/1",
            &admin.headers(),
            None
        )
        .await
        .status,
        200
    );
}
