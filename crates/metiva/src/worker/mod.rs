mod adapter;
mod download;
mod feed;
mod http;
mod integrity;

use crate::{AppState, SourceInput};
use adapter::{Artifact, Catalog, ManifestAdapter, Mapping};
use anyhow::{Context, Result, ensure};
use download::Transfer;
use feed::{Observation, Source};
use futures_util::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
use serde_json::{Value, json};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
use url::Url;

fn mapping(value: Option<Value>) -> Result<Mapping> {
    value
        .map(serde_json::from_value)
        .transpose()
        .context("invalid mapping")
        .map(Option::unwrap_or_default)
}

pub async fn preview(state: &AppState, input: &SourceInput) -> Result<Value> {
    let source = Source {
        id: 0,
        url: input.url.as_deref().map(Url::parse).transpose()?,
        upload: input.upload.as_ref().map(|v| v.as_bytes().to_vec()),
        mapping: mapping(input.mapping.clone())?,
    };
    let mut catalog = Catalog::new();
    feed::collect(state, &source, None, false, &mut catalog, None).await?;
    Ok(json!({"releases":adapter::releases(&catalog)}))
}

async fn sources(state: &AppState, runtime: i64) -> Result<Vec<Source>> {
    let rows = sqlx::query(
        "SELECT id,url,upload,mapping FROM sources WHERE runtime_id=? AND enabled=1 ORDER BY id",
    )
    .bind(runtime)
    .fetch_all(&state.db)
    .await?;
    ensure!(!rows.is_empty(), "runtime has no sources");
    rows.into_iter()
        .map(|r| {
            Ok(Source {
                id: r.get("id"),
                url: r
                    .get::<Option<String>, _>("url")
                    .as_deref()
                    .map(Url::parse)
                    .transpose()?,
                upload: r.get("upload"),
                mapping: mapping(
                    r.get::<Option<String>, _>("mapping")
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()?,
                )?,
            })
        })
        .collect()
}

struct Settings {
    engine: String,
    retries: u32,
    concurrency: usize,
    per_host: usize,
}
async fn settings(state: &AppState) -> Result<Settings> {
    let row = sqlx::query("SELECT engine,retries,concurrency,per_host FROM settings WHERE id=1")
        .fetch_one(&state.db)
        .await?;
    Ok(Settings {
        engine: row.get("engine"),
        retries: row.get::<i64, _>("retries").clamp(0, 20) as u32,
        concurrency: row.get::<i64, _>("concurrency").clamp(1, 64) as usize,
        per_host: row.get::<i64, _>("per_host").clamp(1, 64) as usize,
    })
}

async fn prepare(
    state: &AppState,
    job: i64,
    runtime: i64,
    release: &str,
    name: &str,
    item: &Artifact,
    engine: &str,
) -> Result<i64> {
    let url = item.url.as_str();
    let size = item.size.map(|s| s as i64);
    let host = item.url.host_str().context("missing artifact host")?;
    let row=sqlx::query("INSERT INTO transfers(job_id,runtime_id,release,filename,host,backend,state,source_url,expected_size,expected_digest,total) VALUES(?,?,?,?,?,?,'queued',?,?,?,?) ON CONFLICT(job_id,release,filename) DO UPDATE SET backend=excluded.backend RETURNING id,source_url,expected_size,expected_digest").bind(job).bind(runtime).bind(release).bind(name).bind(host).bind(engine).bind(url).bind(size).bind(&item.sha256).bind(size).fetch_one(&state.db).await?;
    ensure!(
        row.get::<String, _>("source_url") == url
            && row.get::<Option<i64>, _>("expected_size") == size
            && row.get::<Option<String>, _>("expected_digest") == item.sha256,
        "source changed during transfer recovery; start a fresh sync"
    );
    let id: i64 = row.get("id");
    // Adopt only a stopped transfer with the same identity and expectations.
    let path = state.data.join("staging").join(format!("{id}.part"));
    if !tokio::fs::try_exists(&path).await? {
        let old=sqlx::query("SELECT t.id,t.validator,t.effective_url FROM transfers t JOIN jobs j ON j.id=t.job_id WHERE t.id<>? AND t.runtime_id=? AND t.release=? AND t.filename=? AND t.source_url=? AND t.expected_size IS ? AND t.expected_digest IS ? AND j.state='failed' ORDER BY t.id DESC LIMIT 1").bind(id).bind(runtime).bind(release).bind(name).bind(url).bind(size).bind(&item.sha256).fetch_optional(&state.db).await?;
        if let Some(old) = old {
            let old_path = state
                .data
                .join("staging")
                .join(format!("{}.part", old.get::<i64, _>("id")));
            if tokio::fs::try_exists(&old_path).await? {
                // Record the validator first. A crash before rename safely restarts adoption.
                sqlx::query("UPDATE transfers SET validator=?,effective_url=? WHERE id=?")
                    .bind(old.get::<Option<String>, _>("validator"))
                    .bind(old.get::<Option<String>, _>("effective_url"))
                    .bind(id)
                    .execute(&state.db)
                    .await?;
                tokio::fs::rename(old_path, path).await?;
            }
        }
    }
    Ok(id)
}

struct Candidate {
    catalog: Catalog,
    observations: Vec<Observation>,
    mirror: Catalog,
    upstream: Option<String>,
}

async fn publish(
    state: &AppState,
    runtime: i64,
    job: i64,
    active: Option<i64>,
    project: &str,
    slug: &str,
    candidate: Candidate,
) -> Result<()> {
    let manifest=json!({"schema_version":1,"project":project,"runtime":slug,"releases":adapter::releases(&candidate.mirror)}).to_string();
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let old = sqlx::query("SELECT manifest,upstream FROM publications WHERE id=?")
        .bind(active)
        .fetch_optional(&mut *tx)
        .await?;
    let old_digests:Vec<(i64,String)>=sqlx::query_as("SELECT o.source_id,o.digest FROM publication_sources ps JOIN source_observations o ON o.id=ps.observation_id WHERE ps.publication_id=? ORDER BY o.source_id,o.page").bind(active).fetch_all(&mut *tx).await?;
    let unchanged = old.is_some_and(|o| {
        o.get::<String, _>("manifest") == manifest
            && o.get::<Option<String>, _>("upstream") == candidate.upstream
    }) && old_digests
        .iter()
        .map(|(id, d)| (*id, d))
        .eq(candidate.observations.iter().map(|o| (o.source, &o.digest)));
    let publication = if unchanged {
        active.context("missing active publication")?
    } else {
        let publication =
            sqlx::query("INSERT INTO publications(runtime_id,manifest,upstream) VALUES(?,?,?)")
                .bind(runtime)
                .bind(manifest)
                .bind(candidate.upstream)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid();
        for ((release, filename), item) in &candidate.mirror.assets {
            let source = &candidate.catalog.assets[&(release.clone(), filename.clone())];
            sqlx::query("INSERT INTO files(publication_id,release,filename,source_url,digest,size) VALUES(?,?,?,?,?,?)").bind(publication).bind(release).bind(filename).bind(source.url.as_str()).bind(item.sha256.as_deref().context("missing verified digest")?).bind(item.size.context("missing verified size")? as i64).execute(&mut *tx).await?;
        }
        for obs in candidate.observations {
            sqlx::query(
                "INSERT INTO publication_sources(publication_id,observation_id) VALUES(?,?)",
            )
            .bind(publication)
            .bind(obs.id)
            .execute(&mut *tx)
            .await?;
        }
        publication
    };
    let switched=sqlx::query("UPDATE runtimes SET active_publication=?,last_success=unixepoch(),last_checked=unixepoch() WHERE id=? AND active_publication IS ?").bind(publication).bind(runtime).bind(active).execute(&mut *tx).await?;
    ensure!(
        switched.rows_affected() == 1,
        "active publication changed during synchronization"
    );
    sqlx::query("UPDATE jobs SET state='completed',finished_at=unixepoch(),error=NULL WHERE id=?")
        .bind(job)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE transfers SET state='cancelled',command=NULL,speed=0,error='Removed from recovered candidate' WHERE job_id=? AND state NOT IN ('completed','failed','cancelled')").bind(job).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn sync(state: &AppState, runtime: i64, job: i64, limits: Arc<http::Limits>) -> Result<()> {
    let row=sqlx::query("SELECT p.slug AS project,r.slug,r.active_publication,pub.manifest FROM runtimes r JOIN projects p ON p.id=r.project_id LEFT JOIN publications pub ON pub.id=r.active_publication WHERE r.id=?").bind(runtime).fetch_one(&state.db).await?;
    let project: String = row.get("project");
    let slug: String = row.get("slug");
    let active: Option<i64> = row.get("active_publication");
    let inputs = sources(state, runtime).await?;
    let saved =
        sqlx::query("SELECT previous_publication,observation_ids FROM job_plans WHERE job_id=?")
            .bind(job)
            .fetch_optional(&state.db)
            .await?;
    let (catalog, observations) = if let Some(plan) = saved {
        ensure!(
            active == plan.get::<Option<i64>, _>("previous_publication"),
            "publication changed while a synchronization was waiting"
        );
        feed::restore(state, job, plan.get::<&str, _>("observation_ids"), &inputs).await?
    } else {
        let (catalog, mut observations) = feed::observe(state, &inputs, job, true, &limits).await?;
        let previous: Vec<(String, String)> =
            sqlx::query_as("SELECT release,filename FROM files WHERE publication_id=?")
                .bind(active)
                .fetch_all(&state.db)
                .await?;
        let old_manifest: Option<Value> = row
            .get::<Option<String>, _>("manifest")
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?;
        let release_removed = old_manifest
            .as_ref()
            .and_then(|m| m.get("releases"))
            .and_then(Value::as_array)
            .is_some_and(|releases| {
                releases
                    .iter()
                    .filter_map(|r| r.get("version")?.as_str())
                    .any(|v| !catalog.versions.contains_key(v))
            });
        if release_removed || previous.iter().any(|key| !catalog.assets.contains_key(key)) {
            let (confirmation, checked) =
                feed::observe(state, &inputs, job, false, &limits).await?;
            ensure!(
                catalog == confirmation,
                "independent source check disagreed with proposed removals"
            );
            observations = checked;
        }
        let ids: Vec<_> = observations.iter().map(|o| o.id).collect();
        sqlx::query(
            "INSERT INTO job_plans(job_id,previous_publication,observation_ids) VALUES(?,?,?)",
        )
        .bind(job)
        .bind(active)
        .bind(serde_json::to_string(&ids)?)
        .execute(&state.db)
        .await?;
        (catalog, observations)
    };
    let options = settings(state).await?;
    let mut transfers = Vec::with_capacity(catalog.assets.len());
    for ((release, name), item) in &catalog.assets {
        let id = prepare(state, job, runtime, release, name, item, &options.engine).await?;
        transfers.push((
            (release.clone(), name.clone()),
            Transfer {
                id,
                filename: name.clone(),
                item: item.clone(),
                curl: options.engine == "curl",
                retries: options.retries,
                limits: limits.clone(),
            },
        ));
    }
    let mut pending = stream::iter(transfers)
        .map(|(key, task)| async move {
            let result = task.run(state).await;
            (key, result)
        })
        .buffer_unordered((options.concurrency * 2).clamp(2, 64));
    let mut mirror = catalog.clone();
    let mut deferred = false;
    while let Some(((release, name), result)) = pending.next().await {
        let Some((digest, size)) = result.with_context(|| format!("artifact {release}/{name}"))?
        else {
            deferred = true;
            continue;
        };
        let mut url = Url::parse(&state.public_url)?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("invalid public origin"))?
            .clear()
            .extend(["p", &project, "r", &slug, "files", &release, &name]);
        let asset = mirror
            .assets
            .get_mut(&(release, name))
            .context("candidate identity disappeared")?;
        asset.url = url;
        asset.sha256 = Some(digest);
        asset.size = Some(size);
    }
    if deferred {
        return Ok(());
    }
    let upstream = if inputs.len() == 1 && observations.len() == 1 {
        let row=sqlx::query("SELECT d.body,o.url FROM source_observations o JOIN raw_documents d ON d.digest=o.digest WHERE o.id=?").bind(observations[0].id).fetch_one(&state.db).await?;
        let base = row
            .get::<Option<String>, _>("url")
            .as_deref()
            .map(Url::parse)
            .transpose()?;
        adapter::GenericJson
            .compatible_manifest(
                &row.get::<Vec<u8>, _>("body"),
                &inputs[0].mapping,
                base.as_ref(),
                &mirror,
            )
            .map(|v| v.to_string())
    } else {
        None
    };
    publish(
        state,
        runtime,
        job,
        active,
        &project,
        &slug,
        Candidate {
            catalog,
            observations,
            mirror,
            upstream,
        },
    )
    .await
}

async fn process(state: AppState, runtime: i64, job: i64, limits: Arc<http::Limits>) -> Result<()> {
    if let Err(error) = sync(&state, runtime, job, limits).await {
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE jobs SET state='failed',finished_at=unixepoch(),error=? WHERE id=?")
            .bind(format!("{error:#}"))
            .bind(job)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE runtimes SET last_checked=unixepoch() WHERE id=?")
            .bind(runtime)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE transfers SET state='cancelled',command=NULL,speed=0,retry_at=NULL,error='Synchronization stopped' WHERE job_id=? AND state NOT IN('completed','failed','cancelled')").bind(job).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(())
}

pub async fn run(state: AppState) -> Result<()> {
    // The worker process holds an exclusive lock. Incomplete jobs keep their transfer IDs
    // and validators; last-good publications stay visible throughout recovery.
    sqlx::query("UPDATE jobs SET state='queued' WHERE state='running'")
        .execute(&state.db)
        .await?;
    let options = settings(&state).await?;
    let mut limits = Arc::new(http::Limits::new(options.concurrency, options.per_host));
    let mut work = FuturesUnordered::new();
    let mut active_jobs = std::collections::BTreeSet::new();
    let mut timer = tokio::time::interval(Duration::from_secs(1));
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some((id,result))=work.next(), if !work.is_empty()=>{active_jobs.remove(&id);result?;},
            _=timer.tick()=>{
                sqlx::query("INSERT OR IGNORE INTO jobs(runtime_id) SELECT r.id FROM runtimes r WHERE EXISTS(SELECT 1 FROM sources s WHERE s.runtime_id=r.id AND s.enabled=1) AND (r.last_checked IS NULL OR r.last_checked+r.sync_interval<=unixepoch())").execute(&state.db).await?;
                if work.is_empty() {let options=settings(&state).await?; limits=Arc::new(http::Limits::new(options.concurrency,options.per_host));}
                while work.len()<4 {
                    let claimed:Option<(i64,i64)>=sqlx::query_as("UPDATE jobs SET state='running' WHERE id=(SELECT j.id FROM jobs j WHERE j.id NOT IN(SELECT value FROM json_each(?)) AND (j.state='queued' OR (j.state='running' AND EXISTS(SELECT 1 FROM transfers t WHERE t.job_id=j.id AND (t.command IS NOT NULL OR t.state IN('queued','downloading','connecting','verifying') OR (t.state='retrying' AND coalesce(t.retry_at,0)<=unixepoch()))))) ORDER BY j.id LIMIT 1) RETURNING id,runtime_id").bind(serde_json::to_string(&active_jobs)?).fetch_optional(&state.db).await?;
                    let Some((job,runtime))=claimed else {break};
                    active_jobs.insert(job);let s=state.clone();let l=limits.clone();
                    work.push(async move{(job,process(s,runtime,job,l).await)});
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    async fn job(state: &AppState) -> i64 {
        sqlx::query("INSERT INTO jobs(runtime_id,state) VALUES(1,'running')")
            .execute(&state.db)
            .await
            .unwrap()
            .last_insert_rowid()
    }
    async fn active(state: &AppState) -> i64 {
        sqlx::query_scalar("SELECT active_publication FROM runtimes WHERE id=1")
            .fetch_one(&state.db)
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn failed_source_and_publication_transaction_preserve_available_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let s = crate::open(dir.path(), "https://mirror.example")
            .await
            .unwrap();
        for query in [
            "INSERT INTO projects(name,slug) VALUES('Project','p')",
            "INSERT INTO runtimes(project_id,name,slug) VALUES(1,'Runtime','r')",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(query))
                .execute(&s.db)
                .await
                .unwrap();
        }
        let digest = hex::encode(Sha256::digest(b"archive"));
        let path = s.data.join("artifacts").join(&digest);
        tokio::fs::write(&path, b"archive").await.unwrap();
        let source=json!({"releases":[{"version":"a","assets":[{"filename":"package.bin","url":"https://example.com/download","size":7,"sha256":digest}]}],"unknown":"keep"}).to_string();
        sqlx::query("INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',?)")
            .bind(source.as_bytes())
            .execute(&s.db)
            .await
            .unwrap();
        let limits = Arc::new(http::Limits::new(1, 1));
        process(s.clone(), 1, job(&s).await, limits.clone())
            .await
            .unwrap();
        let published = active(&s).await;
        let upstream: String = sqlx::query_scalar("SELECT upstream FROM publications WHERE id=?")
            .bind(published)
            .fetch_one(&s.db)
            .await
            .unwrap();
        assert!(
            upstream.contains("https://mirror.example/p/p/r/r/files/a/package.bin")
                && upstream.contains("keep")
        );
        sqlx::query("UPDATE sources SET upload=X'7b' WHERE id=1")
            .execute(&s.db)
            .await
            .unwrap();
        let failed = job(&s).await;
        process(s.clone(), 1, failed, limits.clone()).await.unwrap();
        assert_eq!(active(&s).await, published);
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE id=?")
                .bind(failed)
                .fetch_one(&s.db)
                .await
                .unwrap(),
            "failed"
        );
        let empty = b"{\"releases\":[]}";
        sqlx::query("UPDATE sources SET upload=? WHERE id=1")
            .bind(empty.as_slice())
            .execute(&s.db)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER inject_failure BEFORE UPDATE OF active_publication ON runtimes BEGIN SELECT RAISE(ABORT,'simulated publication failure'); END").execute(&s.db).await.unwrap();
        process(s.clone(), 1, job(&s).await, limits.clone())
            .await
            .unwrap();
        assert_eq!(active(&s).await, published);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM publications")
                .fetch_one(&s.db)
                .await
                .unwrap(),
            1
        );
        sqlx::query("DROP TRIGGER inject_failure")
            .execute(&s.db)
            .await
            .unwrap();
        let recovered = job(&s).await;
        // With a job persisted as running, restart the actual scheduler and await publication.
        let worker = tokio::spawn(run(s.clone()));
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if active(&s).await != published {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        worker.abort();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM source_observations WHERE job_id=?")
                .bind(recovered)
                .fetch_one(&s.db)
                .await
                .unwrap(),
            2
        );
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"archive");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM files WHERE publication_id=?")
                .bind(published)
                .fetch_one(&s.db)
                .await
                .unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn parked_transfers_yield_to_ready_work_and_recover_the_frozen_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::open(dir.path(), "https://mirror.example")
            .await
            .unwrap();
        sqlx::query("UPDATE settings SET concurrency=1,per_host=1")
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO projects(name,slug) VALUES('P','p')")
            .execute(&state.db)
            .await
            .unwrap();
        let digest = hex::encode(Sha256::digest(b"content"));
        tokio::fs::write(state.data.join("artifacts").join(&digest), b"content")
            .await
            .unwrap();
        let assets:Vec<_>=["a","b","c"].iter().map(|n|json!({"filename":n,"url":format!("https://files.example/{n}"),"sha256":digest})).collect();
        let upload = json!({"releases":[{"version":"frozen","artifacts":assets}]}).to_string();
        let mut catalog = Catalog::new();
        adapter::GenericJson
            .parse_page(upload.as_bytes(), &Mapping::default(), None, &mut catalog)
            .unwrap();
        for runtime in 1..=5i64 {
            sqlx::query("INSERT INTO runtimes(id,project_id,name,slug) VALUES(?,1,'R',?)")
                .bind(runtime)
                .bind(format!("r{runtime}"))
                .execute(&state.db)
                .await
                .unwrap();
            sqlx::query("INSERT INTO sources(runtime_id,kind,upload) VALUES(?,'upload',?)")
                .bind(runtime)
                .bind(upload.as_bytes())
                .execute(&state.db)
                .await
                .unwrap();
            let job = sqlx::query("INSERT INTO jobs(runtime_id) VALUES(?)")
                .bind(runtime)
                .execute(&state.db)
                .await
                .unwrap()
                .last_insert_rowid();
            if runtime < 5 {
                for name in ["a", "b"] {
                    let item = &catalog.assets[&("frozen".to_string(), name.to_string())];
                    let id = prepare(&state, job, runtime, "frozen", name, item, "native")
                        .await
                        .unwrap();
                    sqlx::query("UPDATE transfers SET state='paused' WHERE id=?")
                        .bind(id)
                        .execute(&state.db)
                        .await
                        .unwrap();
                }
            }
        }
        let worker = tokio::spawn(run(state.clone()));
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT active_publication FROM runtimes WHERE id=5",
                )
                .fetch_one(&state.db)
                .await
                .unwrap()
                .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM transfers WHERE runtime_id<5 AND filename='c' AND state='completed'").fetch_one(&state.db).await.unwrap(),4);
        worker.abort();
        let _ = worker.await;
        // Simulate the upstream changing after a crash. Recovery must use the admitted bytes.
        sqlx::query("UPDATE sources SET upload=? WHERE runtime_id=1")
            .bind(b"{\"releases\":[]}".as_slice())
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE transfers SET command='resume' WHERE runtime_id=1 AND state='paused'")
            .execute(&state.db)
            .await
            .unwrap();
        let worker = tokio::spawn(run(state.clone()));
        tokio::time::timeout(Duration::from_secs(15),async{loop{if sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM files JOIN publications p ON p.id=publication_id WHERE p.runtime_id=1 AND release='frozen'").fetch_one(&state.db).await.unwrap()==3 {break;}tokio::time::sleep(Duration::from_millis(50)).await;}}).await.unwrap();
        worker.abort();
        let _ = worker.await;
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM source_observations o JOIN sources s ON s.id=o.source_id WHERE s.runtime_id=1").fetch_one(&state.db).await.unwrap(),1);
    }
}
