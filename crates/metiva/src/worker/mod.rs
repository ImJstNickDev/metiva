mod selection;
pub(crate) use selection::router as selection_router;
mod adapter;
mod download;
mod feed;
mod http;
mod integrity;
pub(crate) mod inventory;
mod nested;
pub(crate) use feed::inspect_page;
pub(crate) use nested::verify_inspection;

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
    if let Some(rule) = &source.mapping.nested {
        let (sample, count) = feed::preview_nested(state, &source, rule).await?;
        let warnings: Vec<_> = sample
            .release_errors
            .iter()
            .map(|(version, error)| json!({"version":version, "error":error}))
            .collect();
        return Ok(
            json!({"releases": adapter::releases(&sample), "warnings": warnings,
            "selection": {"matched": count.selected, "previewed": count.previewed}}),
        );
    }
    let mut catalog = Catalog::new();
    feed::collect(state, &source, None, false, &mut catalog, None).await?;
    if let Some((version, error)) = catalog.release_errors.first_key_value() {
        anyhow::bail!("release {version}: {error}");
    }
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
    let row=sqlx::query("INSERT INTO transfers(job_id,runtime_id,release,filename,host,backend,state,source_url,expected_size,expected_digest,total) SELECT ?,?,?,?,?,?,'queued',?,?,?,? WHERE EXISTS(SELECT 1 FROM jobs WHERE id=? AND state='running') ON CONFLICT(job_id,release,filename) DO UPDATE SET backend=excluded.backend RETURNING id,source_url,expected_size,expected_digest").bind(job).bind(runtime).bind(release).bind(name).bind(host).bind(engine).bind(url).bind(size).bind(&item.sha256).bind(size).bind(job).fetch_one(&state.db).await?;
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
    sources: Catalog,
    observations: Vec<Observation>,
    mirror: Catalog,
    failures: std::collections::BTreeMap<String, String>,
    deferred: bool,
    upstream: Option<String>,
}

// The active publication is the only baseline. Old snapshots may contain
// intentionally unavailable members from an earlier version of Metiva.
fn cumulative_releases(previous: Option<&str>, verified: &Catalog) -> Result<Value> {
    let mut releases = std::collections::BTreeMap::new();
    if let Some(manifest) = previous {
        let previous: Value = serde_json::from_str(manifest)?;
        for release in previous["releases"]
            .as_array()
            .context("invalid stored releases")?
        {
            releases.insert(
                release["version"]
                    .as_str()
                    .context("invalid stored version")?
                    .to_owned(),
                release.clone(),
            );
        }
    }
    for mut release in adapter::releases(verified)
        .as_array()
        .context("invalid verified releases")?
        .clone()
    {
        let version = release["version"]
            .as_str()
            .context("missing version")?
            .to_owned();
        if let Some(prior) = releases.get(&version) {
            let mut files = std::collections::BTreeMap::new();
            for file in prior["artifacts"]
                .as_array()
                .context("invalid stored artifacts")?
                .iter()
                .chain(
                    release["artifacts"]
                        .as_array()
                        .context("invalid verified artifacts")?,
                )
            {
                files.insert(
                    file["filename"]
                        .as_str()
                        .context("invalid filename")?
                        .to_owned(),
                    file.clone(),
                );
            }
            release["artifacts"] = json!(files.into_values().collect::<Vec<_>>());
        }
        releases.insert(version, release);
    }
    Ok(json!(releases.into_values().collect::<Vec<_>>()))
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
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let previous: Option<String> = sqlx::query_scalar(
        "SELECT pub.manifest FROM runtimes r JOIN projects p ON p.id=r.project_id
         JOIN jobs j ON j.runtime_id=r.id LEFT JOIN publications pub ON pub.id=r.active_publication
         WHERE r.id=? AND r.active_publication IS ? AND r.parked_at IS NULL AND p.parked_at IS NULL
         AND j.id=? AND j.state='running'",
    )
    .bind(runtime)
    .bind(active)
    .bind(job)
    .fetch_one(&mut *tx)
    .await
    .context("synchronization is no longer active")?;
    let verified = candidate.mirror.versions.len();
    let failed = candidate.failures.len();
    let mut publication = active;
    if verified > 0 || (failed == 0 && !candidate.deferred) {
        let releases = cumulative_releases(previous.as_deref(), &candidate.mirror)?;
        // An upstream-shaped document describes this snapshot only when it can
        // represent every retained member and all observed releases verified.
        let upstream = candidate.upstream.filter(|_| {
            !candidate.deferred && failed == 0 && releases == adapter::releases(&candidate.mirror)
        });
        let manifest =
            json!({"schema_version":1,"project":project,"runtime":slug,"releases":releases})
                .to_string();
        let old_upstream: Option<String> =
            sqlx::query_scalar("SELECT upstream FROM publications WHERE id=?")
                .bind(active)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        let old_digests:Vec<(i64,String)>=sqlx::query_as("SELECT o.source_id,o.digest FROM publication_sources ps JOIN source_observations o ON o.id=ps.observation_id WHERE ps.publication_id=? ORDER BY o.source_id,o.page").bind(active).fetch_all(&mut *tx).await?;
        let unchanged = previous.as_deref() == Some(manifest.as_str())
            && old_upstream == upstream
            && old_digests
                .iter()
                .map(|(id, d)| (*id, d))
                .eq(candidate.observations.iter().map(|o| (o.source, &o.digest)));
        if !unchanged {
            let id =
                sqlx::query("INSERT INTO publications(runtime_id,manifest,upstream) VALUES(?,?,?)")
                    .bind(runtime)
                    .bind(manifest)
                    .bind(upstream)
                    .execute(&mut *tx)
                    .await?
                    .last_insert_rowid();
            sqlx::query(
                "INSERT INTO files(publication_id,release,filename,source_url,digest,size)
                SELECT ?,release,filename,source_url,digest,size FROM files WHERE publication_id=?",
            )
            .bind(id)
            .bind(active)
            .execute(&mut *tx)
            .await?;
            for (key, item) in &candidate.mirror.assets {
                let (release, filename) = key;
                let source = &candidate.sources.assets[key];
                sqlx::query("INSERT INTO files(publication_id,release,filename,source_url,digest,size) VALUES(?,?,?,?,?,?)
                    ON CONFLICT(publication_id,release,filename) DO UPDATE SET source_url=excluded.source_url,digest=excluded.digest,size=excluded.size")
                    .bind(id).bind(release).bind(filename).bind(source.url.as_str()).bind(item.sha256.as_deref().context("missing verified digest")?)
                    .bind(item.size.context("missing verified size")? as i64).execute(&mut *tx).await?;
            }
            sqlx::query(
                "INSERT INTO file_revisions(file_id,runtime_id,release,filename,job_id)
                SELECT f.id,?,f.release,f.filename,? FROM files f LEFT JOIN files old
                ON old.publication_id=? AND old.release=f.release AND old.filename=f.filename
                WHERE f.publication_id=? AND old.digest IS NOT f.digest",
            )
            .bind(runtime)
            .bind(job)
            .bind(active)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            for obs in candidate.observations {
                sqlx::query(
                    "INSERT INTO publication_sources(publication_id,observation_id) VALUES(?,?)",
                )
                .bind(id)
                .bind(obs.id)
                .execute(&mut *tx)
                .await?;
            }
            publication = Some(id);
        }
    }
    let finished = !candidate.deferred;
    sqlx::query("UPDATE runtimes SET active_publication=?,last_checked=CASE WHEN ? THEN unixepoch() ELSE last_checked END,
        last_success=CASE WHEN ? THEN unixepoch() ELSE last_success END WHERE id=?")
        .bind(publication).bind(finished).bind(finished && failed==0).bind(runtime).execute(&mut *tx).await?;
    // The check against active_publication on recovery must include any releases
    // this same frozen plan already published while another release was paused.
    sqlx::query("UPDATE job_plans SET previous_publication=? WHERE job_id=?")
        .bind(publication)
        .bind(job)
        .execute(&mut *tx)
        .await?;
    let status = if !finished {
        "running"
    } else if failed > 0 {
        "failed"
    } else {
        "completed"
    };
    let error = (failed > 0).then(|| {
        let details = candidate
            .failures
            .iter()
            .take(12)
            .map(|(release, error)| format!("{release}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        format!("{verified} verified, {failed} failed. {details}")
    });
    sqlx::query("UPDATE jobs SET state=?,finished_at=CASE WHEN ? THEN unixepoch() END,error=?,verified_releases=?,failed_releases=? WHERE id=?")
        .bind(status).bind(finished).bind(error).bind(verified as i64).bind(failed as i64).bind(job).execute(&mut *tx).await?;
    if finished {
        sqlx::query("UPDATE transfers SET state='cancelled',command=NULL,speed=0,retry_at=NULL,error='Sync finished before this transfer completed' WHERE job_id=? AND state NOT IN ('completed','failed','cancelled')").bind(job).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn sync(state: &AppState, runtime: i64, job: i64, limits: Arc<http::Limits>) -> Result<()> {
    let row=sqlx::query("SELECT p.slug AS project,r.slug,r.active_publication FROM runtimes r JOIN projects p ON p.id=r.project_id WHERE r.id=? AND r.parked_at IS NULL AND p.parked_at IS NULL AND EXISTS (SELECT 1 FROM jobs WHERE id=? AND runtime_id=r.id AND state='running')").bind(runtime).bind(job).fetch_one(&state.db).await?;
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
        let (catalog, observations) = feed::observe(state, &inputs, job, true, &limits).await?;
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
        if catalog.release_errors.contains_key(release) {
            continue;
        }
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
    let mut failures = catalog.release_errors.clone();
    let mut waiting = std::collections::BTreeSet::new();
    while let Some(((release, name), result)) = pending.next().await {
        let (digest, size) = match result {
            Ok(Some(value)) => value,
            Ok(None) => {
                waiting.insert(release);
                continue;
            }
            Err(error) => {
                failures
                    .entry(release)
                    .or_insert_with(|| format!("{name}: {error:#}"));
                continue;
            }
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
    // A permanent failure already rules out this entire release. Its paused
    // siblings need not hold the job open after all other releases settle.
    waiting.retain(|release| !failures.contains_key(release));
    let accepted = |version: &str| !failures.contains_key(version) && !waiting.contains(version);
    mirror.versions.retain(|v, _| accepted(v));
    mirror.assets.retain(|(v, _), _| accepted(v));
    let deferred = !waiting.is_empty();
    let upstream = if !deferred
        && failures.is_empty()
        && inputs.len() == 1
        && observations.len() == 1
    {
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
            sources: catalog,
            observations,
            mirror,
            failures,
            deferred,
            upstream,
        },
    )
    .await
}

async fn process(state: AppState, runtime: i64, job: i64, limits: Arc<http::Limits>) -> Result<()> {
    let cancellation = async {
        loop {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let running: bool = sqlx::query_scalar("SELECT state='running' FROM jobs WHERE id=?")
                .bind(job)
                .fetch_one(&state.db)
                .await?;
            if !running {
                return Ok::<(), sqlx::Error>(());
            }
        }
    };
    let outcome = tokio::select! {
        result=sync(&state,runtime,job,limits)=>result,
        result=cancellation=>return Ok(result?),
    };
    if let Err(error) = outcome {
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let stopped=sqlx::query("UPDATE jobs SET state='failed',finished_at=unixepoch(),error=? WHERE id=? AND state='running'")
            .bind(format!("{error:#}"))
            .bind(job)
            .execute(&mut *tx)
            .await?;
        if stopped.rows_affected() == 0 {
            return Ok(());
        }
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
                sqlx::query("INSERT OR IGNORE INTO jobs(runtime_id) SELECT r.id FROM runtimes r JOIN projects p ON p.id=r.project_id WHERE r.parked_at IS NULL AND p.parked_at IS NULL AND EXISTS(SELECT 1 FROM sources s WHERE s.runtime_id=r.id AND s.enabled=1) AND (r.last_checked IS NULL OR r.last_checked+r.sync_interval<=unixepoch())").execute(&state.db).await?;
                if work.is_empty() {let options=settings(&state).await?; limits=Arc::new(http::Limits::new(options.concurrency,options.per_host));}
                while work.len()<4 {
                    let claimed:Option<(i64,i64)>=sqlx::query_as("UPDATE jobs SET state='running' WHERE id=(SELECT j.id FROM jobs j JOIN runtimes r ON r.id=j.runtime_id JOIN projects p ON p.id=r.project_id WHERE r.parked_at IS NULL AND p.parked_at IS NULL AND j.id NOT IN(SELECT value FROM json_each(?)) AND (j.state='queued' OR (j.state='running' AND EXISTS(SELECT 1 FROM transfers t WHERE t.job_id=j.id AND (t.command IS NOT NULL OR t.state IN('queued','downloading','connecting','verifying') OR (t.state='retrying' AND coalesce(t.retry_at,0)<=unixepoch()))))) ORDER BY j.id LIMIT 1) RETURNING id,runtime_id").bind(serde_json::to_string(&active_jobs)?).fetch_optional(&state.db).await?;
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
            1
        );
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"archive");
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT digest FROM files WHERE publication_id=?")
                .bind(active(&s).await)
                .fetch_one(&s.db)
                .await
                .unwrap(),
            digest
        );
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
    async fn publishes_good_releases_and_records_only_verified_byte_changes() {
        let dir = tempfile::tempdir().unwrap();
        let state = crate::open(dir.path(), "https://mirror.example")
            .await
            .unwrap();
        sqlx::raw_sql(
            "INSERT INTO projects(name,slug) VALUES('P','p');
            INSERT INTO runtimes(project_id,name,slug) VALUES(1,'R','r');
            INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',X'00');
            UPDATE settings SET retries=0",
        )
        .execute(&state.db)
        .await
        .unwrap();
        let seed = |bytes: &[u8]| {
            let digest = hex::encode(Sha256::digest(bytes));
            std::fs::write(state.data.join("artifacts").join(&digest), bytes).unwrap();
            digest
        };
        let old = seed(b"previous bytes");
        let new = seed(b"updated bytes");
        let asset = |name: &str, sha: &str| json!({"filename":name,"url":format!("https://files.example/{name}"),"sha256":sha});
        let sync = |releases: Value| {
            let state = state.clone();
            async move {
                sqlx::query("UPDATE sources SET upload=?")
                    .bind(json!({"releases":releases}).to_string().into_bytes())
                    .execute(&state.db)
                    .await
                    .unwrap();
                let id = job(&state).await;
                process(state, 1, id, Arc::new(http::Limits::new(2, 2)))
                    .await
                    .unwrap();
                id
            }
        };
        sync(json!([{"version":"a","files":[asset("app.bin",&old),asset("kept.bin",&old)]}])).await;
        sqlx::query("UPDATE runtimes SET last_success=17")
            .execute(&state.db)
            .await
            .unwrap();

        let mut invalid = asset("broken.bin", &new);
        invalid["size"] = json!(i64::MAX);
        let partial = sync(
            json!([{"version":"a","files":[asset("app.bin",&new),invalid]},
            {"version":"b","files":[asset("app.bin",&new)]}]),
        )
        .await;
        let result: (String, i64, i64, String) = sqlx::query_as(
            "SELECT state,verified_releases,failed_releases,error FROM jobs WHERE id=?",
        )
        .bind(partial)
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!((&result.0[..], result.1, result.2), ("failed", 1, 1));
        assert!(result.3.contains("a: broken.bin"));
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT last_success FROM runtimes")
                .fetch_one(&state.db)
                .await
                .unwrap(),
            17
        );
        async fn current_files(s: &AppState) -> Vec<(String, String, String)> {
            sqlx::query_as("SELECT release,filename,digest FROM files WHERE publication_id=(SELECT active_publication FROM runtimes) ORDER BY release,filename")
                .fetch_all(&s.db).await.unwrap()
        }
        assert_eq!(
            current_files(&state).await,
            vec![
                ("a".into(), "app.bin".into(), old.clone()),
                ("a".into(), "kept.bin".into(), old.clone()),
                ("b".into(), "app.bin".into(), new.clone())
            ]
        );
        let update = json!([{"version":"a","files":[asset("app.bin",&new)]}]);
        sync(update.clone()).await;
        let updated = active(&state).await;
        sync(update).await;
        assert_eq!(active(&state).await, updated);
        assert_eq!(
            current_files(&state).await,
            vec![
                ("a".into(), "app.bin".into(), new.clone()),
                ("a".into(), "kept.bin".into(), old.clone()),
                ("b".into(), "app.bin".into(), new.clone())
            ]
        );
        assert!(
            sqlx::query_scalar::<_, Option<String>>("SELECT upstream FROM publications WHERE id=?")
                .bind(updated)
                .fetch_one(&state.db)
                .await
                .unwrap()
                .is_none()
        );
        sync(json!([{"version":"a","files":[asset("app.bin",&old)]}])).await;
        let history: Vec<String> = sqlx::query_scalar(
            "SELECT f.digest FROM file_revisions h JOIN files f ON f.id=h.file_id
            WHERE h.release='a' AND h.filename='app.bin' ORDER BY h.file_id",
        )
        .fetch_all(&state.db)
        .await
        .unwrap();
        assert_eq!(history, vec![old.clone(), new, old]);
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
        let upload = json!({"releases":[{"version":"frozen","artifacts":assets},
            {"version":"ready","artifacts":[{"filename":"stable.bin","url":"https://files.example/stable.bin","sha256":digest}]}]}).to_string();
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
            let job = sqlx::query("INSERT INTO jobs(runtime_id,state) VALUES(?,'running')")
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
        // A paused release must not block its fully verified sibling, including on
        // first publication. Only a complete job advances last_success.
        let released:Vec<(i64,String)>=sqlx::query_as("SELECT r.id,f.release FROM runtimes r JOIN files f ON f.publication_id=r.active_publication WHERE r.id<5 AND r.last_success IS NULL ORDER BY r.id")
            .fetch_all(&state.db).await.unwrap();
        assert_eq!(
            released,
            (1..5)
                .map(|id| (id, "ready".to_string()))
                .collect::<Vec<_>>()
        );
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
