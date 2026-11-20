use super::*;
use crate::worker::{feed, http, sources};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

#[tokio::test]
async fn recurring_checks_share_large_parsed_pages() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let state = crate::open(dir.path(), "https://mirror.example").await?;
    sqlx::raw_sql(
        "INSERT INTO projects(name,slug) VALUES('Project','p');
         INSERT INTO runtimes(project_id,name,slug) VALUES(1,'Runtime','r');
         INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',X'00')",
    )
    .execute(&state.db)
    .await?;
    let files: Vec<_> = (0..1000)
        .map(|i| json!({"filename":format!("f{i}"), "url":format!("https://files.example/f{i}"), "size":1}))
        .collect();
    let mut page = json!({"releases":[{"version":"v1","files":files}]});
    let limits = http::Limits::new(1, 1);
    for poll in 1..=13 {
        // Bytes, file order and response metadata can vary independently of
        // which parsed artifacts the page actually contained.
        if poll == 12 {
            page["releases"][0]["files"]
                .as_array_mut()
                .unwrap()
                .reverse();
            page["note"] = json!("upstream edited its banner");
        }
        if poll == 13 {
            page["releases"][0]["files"][0]["size"] = json!(2);
        }
        sqlx::query("UPDATE sources SET upload=? WHERE id=1")
            .bind(serde_json::to_vec(&page)?)
            .execute(&state.db)
            .await?;
        let job = sqlx::query("INSERT INTO jobs(runtime_id,state) VALUES(1,'running')")
            .execute(&state.db)
            .await?
            .last_insert_rowid();
        let inputs = sources(&state, 1).await?;
        let (catalog, observations) = feed::observe(&state, &inputs, job, true, &limits).await?;
        assert_eq!(catalog.assets.len(), 1000);
        assert_eq!(observations.len(), 1);
        sqlx::query("UPDATE jobs SET state='completed' WHERE id=?")
            .bind(job)
            .execute(&state.db)
            .await?;
        if poll >= 12 {
            let counts: (i64, i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM source_observations),
                        (SELECT count(*) FROM raw_documents),
                        (SELECT count(*) FROM parsed_inventories),
                        (SELECT count(*) FROM inventory_artifacts),
                        (SELECT count(*) FROM observation_artifacts)",
            )
            .fetch_one(&state.db)
            .await?;
            if poll == 12 {
                assert_eq!(counts, (12, 2, 1, 1000, 12000));
            } else {
                assert_eq!(counts, (13, 3, 2, 2000, 13000));
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn interrupted_upgrade_retains_each_observation_and_its_historical_artifacts() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    std::fs::create_dir(&path)?;
    let db = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path.join("metiva.sqlite3"))
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate!("./migrations").run_to(8, &db).await?;
    sqlx::raw_sql(
        "INSERT INTO projects(name,slug) VALUES('P','p');
         INSERT INTO runtimes(project_id,name,slug) VALUES(1,'R','r');
         INSERT INTO sources(runtime_id,kind,url) VALUES(1,'url','https://feed.example/');
         INSERT INTO jobs(runtime_id,state) VALUES(1,'completed'),(1,'completed'),(1,'completed');
         INSERT INTO raw_documents VALUES('body',X'7b7d');
         INSERT INTO source_observations(source_id,job_id,page,digest,url,request_url,etag,last_modified,next_link)
         SELECT 1,id,0,'body','https://feed.example/redirect','https://feed.example/',printf('tag%d',id),'Wed, 21 Oct 2015 07:28:00 GMT','<page2>; rel=next' FROM jobs;
         INSERT INTO publications(runtime_id,manifest) VALUES(1,'{}');
         INSERT INTO publication_sources(publication_id,observation_id) VALUES(1,1);
         INSERT INTO job_plans(job_id,previous_publication,observation_ids) VALUES(2,1,'[2]');
         INSERT INTO observation_artifacts(observation_id,release,filename,source_url,metadata)
         SELECT id,'v1','package','https://files.example/package',CASE id WHEN 3 THEN '{\"size\":2}' ELSE '{\"size\":1}' END FROM jobs",
    ).execute(&db).await?;
    let rows = "SELECT observation_id,release,filename,source_url,metadata FROM observation_artifacts ORDER BY observation_id";
    let before: Vec<(i64, String, String, String, String)> =
        sqlx::query_as(rows).fetch_all(&db).await?;
    sqlx::migrate!("./migrations").run_to(9, &db).await?;
    sqlx::raw_sql(
        "CREATE TRIGGER fail_upgrade BEFORE UPDATE OF inventory_id ON source_observations
                   WHEN OLD.id=2 BEGIN SELECT RAISE(ABORT,'simulated interrupted upgrade'); END",
    )
    .execute(&db)
    .await?;
    assert!(migrate_legacy(&db).await.is_err());
    let remaining: Vec<i64> = sqlx::query_scalar(
        "SELECT observation_id FROM legacy_observation_artifacts ORDER BY observation_id",
    )
    .fetch_all(&db)
    .await?;
    assert_eq!(remaining, [2, 3]);
    sqlx::query("DROP TRIGGER fail_upgrade")
        .execute(&db)
        .await?;
    db.close().await;

    let state = crate::open(dir.path(), "https://mirror.example").await?;
    let after: Vec<(i64, String, String, String, String)> =
        sqlx::query_as(rows).fetch_all(&state.db).await?;
    assert_eq!(after, before);
    let links: Vec<i64> =
        sqlx::query_scalar("SELECT inventory_id FROM source_observations ORDER BY id")
            .fetch_all(&state.db)
            .await?;
    assert_eq!(links[0], links[1]);
    assert_ne!(links[1], links[2]);
    let original: (Vec<u8>, String, String, String, String, String) = sqlx::query_as(
        "SELECT d.body,o.request_url,o.url,o.etag,o.last_modified,o.next_link
         FROM publication_sources ps JOIN source_observations o ON o.id=ps.observation_id
         JOIN raw_documents d USING(digest) WHERE ps.publication_id=1",
    )
    .fetch_one(&state.db)
    .await?;
    assert_eq!(
        original,
        (
            b"{}".to_vec(),
            "https://feed.example/".into(),
            "https://feed.example/redirect".into(),
            "tag1".into(),
            "Wed, 21 Oct 2015 07:28:00 GMT".into(),
            "<page2>; rel=next".into()
        )
    );
    let recovery: String = sqlx::query_scalar(
        "SELECT o.etag FROM job_plans jp, json_each(jp.observation_ids) ids
         JOIN source_observations o ON o.id=ids.value WHERE jp.job_id=2",
    )
    .fetch_one(&state.db)
    .await?;
    assert_eq!(recovery, "tag2");
    Ok(())
}
