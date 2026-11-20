use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use tower::ServiceExt;

#[tokio::test]
async fn upgrade_keeps_current_membership_and_pages_admin_only_revision_history() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("db")).unwrap();
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("db/metiva.sqlite3"))
        .create_if_missing(true);
    let db = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run_to(6, &db).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO projects(name,slug) VALUES('Project','p');
        INSERT INTO runtimes(project_id,name,slug) VALUES(1,'Runtime','r');
        INSERT INTO admin_credential VALUES(1,'admin','unused');
        INSERT INTO runtimes(project_id,name,slug) VALUES(1,'Other','other');
        INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',X'7b7d');
        INSERT INTO jobs(runtime_id,state) VALUES(1,'completed');
        INSERT INTO raw_documents(digest,body) VALUES('original',X'7b7d');
        INSERT INTO source_observations(source_id,job_id,page,digest) VALUES(1,1,0,'original')",
    )
    .execute(&db)
    .await
    .unwrap();
    let token = format!("mts_{}", "0".repeat(64));
    sqlx::query("INSERT INTO admin_sessions VALUES(?,unixepoch()+600)")
        .bind(hex::encode(Sha256::digest(&token)))
        .execute(&db)
        .await
        .unwrap();
    let mut tx = db.begin().await.unwrap();
    for id in 1..=52 {
        sqlx::query(
            "INSERT INTO publications(id,runtime_id,manifest) VALUES(?,1,'{\"releases\":[]}')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("INSERT INTO files(id,publication_id,release,filename,digest,size,source_url) VALUES(?,?,'v','app.bin',?,123,'https://up.example/app')")
            .bind(id).bind(id).bind(format!("{:064x}",id.min(51))).execute(&mut *tx).await.unwrap();
    }
    sqlx::raw_sql("UPDATE runtimes SET active_publication=52 WHERE id=1;
        INSERT INTO publication_sources(publication_id,observation_id) VALUES(1,1);
        INSERT INTO files(id,publication_id,release,filename,digest,size,source_url) VALUES(999,1,'missing','retired.bin','00',2,'https://up.example/retired')")
        .execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    db.close().await;
    let state = metiva::open(dir.path(), "https://mirror.example")
        .await
        .unwrap();
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM files WHERE publication_id=(SELECT active_publication FROM runtimes WHERE id=1)")
               .fetch_one(&state.db).await.unwrap(),1);
    let app = metiva::app(state);
    let request = |url: &str, admin: bool| {
        let mut request = Request::builder().uri(url);
        if admin {
            request = request.header("cookie", format!("metiva_admin={token}"));
        }
        app.clone().oneshot(request.body(Body::empty()).unwrap())
    };
    let snapshots = "/api/admin/runtimes/1/publications";
    assert_eq!(
        request(snapshots, false).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let page = request(snapshots, true).await.unwrap();
    let page: Value =
        serde_json::from_slice(&to_bytes(page.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(page["publications"].as_array().unwrap().len(), 50);
    assert_eq!(page["publications"][0]["id"], 52);
    assert_eq!(page["publications"][0]["current"], true);
    assert_eq!(page["publications"][1]["current"], false);
    assert_eq!(page["next_before"], 3);
    let tail = request("/api/admin/runtimes/1/publications?before=3", true)
        .await
        .unwrap();
    let tail: Value =
        serde_json::from_slice(&to_bytes(tail.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(tail["publications"][1]["id"], 1);
    assert!(tail["next_before"].is_null());
    let detail = request("/api/admin/runtimes/1/publications/1", true)
        .await
        .unwrap();
    let detail: Value =
        serde_json::from_slice(&to_bytes(detail.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(detail["manifest"]["releases"], serde_json::json!([]));
    assert_eq!(detail["originals"][0]["id"], 1);
    let original = request("/api/admin/originals/1", true).await.unwrap();
    assert_eq!(
        to_bytes(original.into_body(), 1000).await.unwrap().as_ref(),
        b"{}"
    );
    assert_eq!(
        request("/api/admin/runtimes/2/publications/1", true)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request("/api/admin/runtimes/999/publications", true)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let path = "/api/admin/files/52/history";
    assert_eq!(
        request(path, false).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let first = request(path, true).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(first["revisions"].as_array().unwrap().len(), 50);
    assert_eq!(first["revisions"][0]["id"], 51);
    assert_eq!(first["revisions"][0]["current"], true);
    assert_eq!(first["revisions"][1]["current"], false);
    assert_eq!(first["next_before"], 2);
    let next = request("/api/admin/files/52/history?before=2", true)
        .await
        .unwrap();
    let next: Value =
        serde_json::from_slice(&to_bytes(next.into_body(), 1 << 20).await.unwrap()).unwrap();
    assert_eq!(next["revisions"].as_array().unwrap().len(), 1);
    assert_eq!(next["revisions"][0]["id"], 1);
    assert!(next["next_before"].is_null());
    let url = next["revisions"][0]["download_url"].as_str().unwrap();
    assert_eq!(
        request(url, false).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let download = request(url, true).await.unwrap();
    assert_eq!(download.status(), StatusCode::OK);
    assert_eq!(
        download.headers()["x-accel-redirect"].to_str().unwrap(),
        format!("/_objects/{:064x}", 1)
    );
    assert_eq!(
        request("/api/admin/file-revisions/999/download", true)
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}
