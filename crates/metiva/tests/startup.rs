use metiva::open;

#[tokio::test]
async fn concurrent_first_start_serializes_migrations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path();
    let (api, worker) = tokio::join!(
        open(path, "https://mirror.example"),
        open(path, "https://mirror.example")
    );
    let api = api.unwrap();
    let worker = worker.unwrap();
    sqlx::query("INSERT INTO projects(name,slug) VALUES('Portable','portable')")
        .execute(&api.db)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM projects")
        .fetch_one(&worker.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
