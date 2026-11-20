mod nesting;
mod parking;
mod timeline;

use crate::{AppState, SourceInput, worker};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, patch, post},
};
use futures_util::stream;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Column, Row, TypeInfo, ValueRef, sqlite::SqliteRow};
use std::{
    convert::Infallible,
    fs, io,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/projects", get(projects).post(create_project))
        .route(
            "/api/admin/projects/{id}",
            get(project).patch(update_project),
        )
        .route(
            "/api/admin/projects/{id}/runtimes",
            get(runtimes).post(create_runtime),
        )
        .route(
            "/api/admin/runtimes/{id}",
            get(runtime).patch(update_runtime),
        )
        .route("/api/admin/runtimes/{id}/past-files", get(past_files))
        .route(
            "/api/admin/runtimes/{id}/inspect-document",
            post(nesting::inspect),
        )
        .route("/api/admin/runtimes/{id}/preview", post(preview))
        .route("/api/admin/runtimes/{id}/sources", post(add_source))
        .route(
            "/api/admin/sources/{id}",
            patch(replace_source).delete(remove_source),
        )
        .route("/api/admin/runtimes/{id}/sync", post(sync))
        .route("/api/admin/transfers", get(transfers))
        .route("/api/admin/transfers/{id}", get(transfer_history))
        .route("/api/admin/transfers/{id}/{action}", post(control))
        .route("/api/admin/events", get(events))
        .merge(parking::router())
        .merge(timeline::router())
        .merge(worker::selection_router())
        .route("/api/admin/originals/{id}", get(original))
        .route("/api/admin/overview", get(overview))
        .route("/api/admin/settings", get(settings).patch(update_settings))
        .layer(Extension(Storage::default()))
        .layer(axum::middleware::from_fn(crate::access::json_rejections))
}
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::RowNotFound => Self(StatusCode::NOT_FOUND, "Resource not found".into()),
            sqlx::Error::Database(db) if db.is_unique_violation() => Self(
                StatusCode::CONFLICT,
                "This identifier is already in use".into(),
            ),
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                Self(StatusCode::NOT_FOUND, "Parent resource not found".into())
            }
            _ => {
                eprintln!("Database operation failed: {e}");
                Self(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Database operation failed".into(),
                )
            }
        }
    }
}
type ResultJson = Result<Json<Value>, ApiError>;
fn bad(msg: &str) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, msg.into())
}

// Reporting queries explicitly select fields suitable for the authenticated dashboard.
fn record(row: SqliteRow) -> Value {
    let mut obj = serde_json::Map::new();
    for col in row.columns() {
        let i = col.ordinal();
        let raw = row.try_get_raw(i).expect("selected column");
        let val = if raw.is_null() {
            Value::Null
        } else {
            match raw.type_info().name() {
                "INTEGER" => json!(row.get::<i64, _>(i)),
                "REAL" => json!(row.get::<f64, _>(i)),
                _ => json!(row.get::<String, _>(i)),
            }
        };
        obj.insert(col.name().into(), val);
    }
    Value::Object(obj)
}
fn records(rows: Vec<SqliteRow>) -> Value {
    Value::Array(rows.into_iter().map(record).collect())
}

macro_rules! project_query {
    ($filter:literal) => { concat!(
        "SELECT p.*,count(r.id) runtime_count,max(r.last_success) last_success,
         CASE WHEN p.parked_at IS NOT NULL THEN 'parked'
              WHEN max(j.state='running') THEN 'running'
              WHEN max(j.state='queued') THEN 'queued'
              WHEN max(j.state='failed') THEN 'attention'
              WHEN max(r.last_success) IS NOT NULL THEN 'ready'
              ELSE 'idle' END status
         FROM projects p LEFT JOIN runtimes r ON r.project_id=p.id AND r.parked_at IS NULL
         LEFT JOIN jobs j ON j.id=(SELECT max(latest.id) FROM jobs latest WHERE latest.runtime_id=r.id)
         WHERE ", $filter, " GROUP BY p.id ORDER BY p.name,p.id"
    )};
}
async fn projects(State(s): State<AppState>) -> ResultJson {
    Ok(Json(records(
        sqlx::query(project_query!("p.parked_at IS NULL"))
            .fetch_all(&s.db)
            .await?,
    )))
}
async fn project(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    Ok(Json(record(
        sqlx::query(project_query!("p.id=?"))
            .bind(id)
            .fetch_one(&s.db)
            .await?,
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    name: String,
    slug: String,
    description: Option<String>,
    visibility: Option<String>,
    sync_interval: Option<i64>,
}
fn identity(c: &Create) -> Result<(), ApiError> {
    if c.name.trim().is_empty() || c.name.len() > 128 || c.name.chars().any(char::is_control) {
        return Err(bad("Use a name between 1 and 128 bytes"));
    }
    if c.slug.is_empty()
        || c.slug.len() > 80
        || !c
            .slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        || c.slug.starts_with('-')
        || c.slug.ends_with('-')
    {
        return Err(bad(
            "Use lowercase letters, numbers and interior hyphens for the slug",
        ));
    }
    Ok(())
}
fn visibility(value: &str, runtime: bool) -> Result<(), ApiError> {
    if matches!(value, "public" | "private") || runtime && value == "inherit" {
        Ok(())
    } else {
        Err(bad("Invalid visibility"))
    }
}
fn interval(i: i64) -> Result<(), ApiError> {
    if (60..=31_536_000).contains(&i) {
        Ok(())
    } else {
        Err(bad("Sync interval must be 60 to 31536000 seconds"))
    }
}
async fn create_project(State(s): State<AppState>, Json(c): Json<Create>) -> ResultJson {
    identity(&c)?;
    if c.sync_interval.is_some() {
        return Err(bad("Set the sync interval on a runtime"));
    }
    let vis = c.visibility.as_deref().unwrap_or("private");
    visibility(vis, false)?;
    if c.description.as_ref().is_some_and(|d| d.len() > 4096) {
        return Err(bad("Description exceeds 4096 bytes"));
    }
    Ok(Json(record(
        sqlx::query(
            "INSERT INTO projects(name,slug,description,visibility) VALUES(?,?,?,?) RETURNING *",
        )
        .bind(c.name.trim())
        .bind(c.slug)
        .bind(c.description.unwrap_or_default())
        .bind(vis)
        .fetch_one(&s.db)
        .await?,
    )))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    visibility: Option<String>,
    sync_interval: Option<i64>,
}
async fn update_project(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<Edit>,
) -> ResultJson {
    if c.sync_interval.is_some() {
        return Err(bad("Set the sync interval on a runtime"));
    }
    let vis = c
        .visibility
        .as_deref()
        .ok_or_else(|| bad("Visibility required"))?;
    visibility(vis, false)?;
    Ok(Json(record(
        sqlx::query("UPDATE projects SET visibility=? WHERE id=? RETURNING *")
            .bind(vis)
            .bind(id)
            .fetch_one(&s.db)
            .await?,
    )))
}
async fn runtimes(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    sqlx::query("SELECT id FROM projects WHERE id=?")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    Ok(Json(records(sqlx::query("SELECT r.*, COALESCE((SELECT state FROM jobs WHERE runtime_id=r.id ORDER BY id DESC LIMIT 1),'idle') status FROM runtimes r WHERE project_id=? AND r.parked_at IS NULL ORDER BY r.name,r.id").bind(id).fetch_all(&s.db).await?)))
}
async fn create_runtime(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<Create>,
) -> ResultJson {
    identity(&c)?;
    if c.description.is_some() {
        return Err(bad("Runtime description is unsupported"));
    }
    let vis = c.visibility.as_deref().unwrap_or("inherit");
    visibility(vis, true)?;
    if let Some(i) = c.sync_interval {
        interval(i)?
    }
    Ok(Json(record(sqlx::query("INSERT INTO runtimes(project_id,name,slug,visibility,sync_interval) SELECT id,?,?,?,COALESCE(?,(SELECT sync_interval FROM settings)) FROM projects WHERE id=? AND parked_at IS NULL RETURNING *").bind(c.name.trim()).bind(c.slug).bind(vis).bind(c.sync_interval).bind(id).fetch_one(&s.db).await?)))
}
async fn update_runtime(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<Edit>,
) -> ResultJson {
    if c.sync_interval.is_none() && c.visibility.is_none() {
        return Err(bad("Specify visibility or sync interval"));
    }
    if let Some(v) = &c.visibility {
        visibility(v, true)?
    }
    if let Some(i) = c.sync_interval {
        interval(i)?
    }
    Ok(Json(record(sqlx::query("UPDATE runtimes SET visibility=coalesce(?,visibility),sync_interval=coalesce(?,sync_interval) WHERE id=? RETURNING *").bind(c.visibility).bind(c.sync_interval).bind(id).fetch_one(&s.db).await?)))
}
// v0.1.0 publications could retire an entire identity. Keep these older objects
// reachable from the administrator's runtime without adding them to the mirror.
macro_rules! past_files_query {
    ($select:literal, $tail:literal) => { concat!($select,
        " FROM files f JOIN publications pub ON pub.id=f.publication_id
          JOIN runtimes r ON r.id=pub.runtime_id
          WHERE r.id=? AND NOT EXISTS(SELECT 1 FROM files live WHERE live.publication_id=r.active_publication
            AND live.release=f.release AND live.filename=f.filename)", $tail) };
}
#[derive(Deserialize)]
struct FilePage {
    before: Option<i64>,
}
async fn past_files(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Query(page): Query<FilePage>,
) -> ResultJson {
    let mut rows = sqlx::query(past_files_query!(
        "SELECT f.id,f.release,f.filename,f.digest,f.size,f.source_url,pub.created_at",
        " AND f.id < ? AND f.id=(SELECT max(previous.id) FROM files previous JOIN publications snapshot ON snapshot.id=previous.publication_id
          WHERE snapshot.runtime_id=r.id AND previous.release=f.release AND previous.filename=f.filename AND previous.digest=f.digest)
          ORDER BY f.id DESC LIMIT 51"
    )).bind(id).bind(page.before.unwrap_or(i64::MAX)).fetch_all(&s.db).await?;
    let next = (rows.len() > 50).then(|| rows[49].get::<i64, _>("id"));
    rows.truncate(50);
    Ok(Json(json!({"files": records(rows),"next_before":next})))
}
async fn runtime(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    let mut item=record(sqlx::query("SELECT r.*,p.slug project_slug,p.visibility project_visibility,p.parked_at project_parked_at,COALESCE((SELECT state FROM jobs WHERE runtime_id=r.id ORDER BY id DESC LIMIT 1),'idle') status,pub.upstream IS NOT NULL has_upstream,pub.manifest FROM runtimes r JOIN projects p ON p.id=r.project_id LEFT JOIN publications pub ON pub.id=r.active_publication WHERE r.id=?").bind(id).fetch_one(&s.db).await?);
    item["manifest"] = item["manifest"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or(Value::Null);
    item["sources"]=records(sqlx::query("SELECT id,kind,url,mapping,etag,last_modified,last_checked,last_success,status,error FROM sources WHERE runtime_id=? AND enabled=1 ORDER BY id").bind(id).fetch_all(&s.db).await?);
    item["has_upstream"] = json!(item["has_upstream"].as_i64() == Some(1));
    let publication = item["active_publication"].as_i64().unwrap_or(-1);
    item["current_files"] = files(&s, publication).await?;
    item["originals"] = originals(&s, publication).await?;
    item["has_past_files"] = json!(
        sqlx::query_scalar::<_, bool>(past_files_query!("SELECT EXISTS(SELECT 1", ")"))
            .bind(id)
            .fetch_one(&s.db)
            .await?
    );
    item["history"] = records(sqlx::query(
        "SELECT j.id,j.state,j.created_at,j.finished_at,j.error,j.verified_releases,j.failed_releases,
         CASE WHEN j.state='running' AND NOT EXISTS(SELECT 1 FROM job_plans WHERE job_id=j.id)
         THEN (SELECT count(*) FROM sources s JOIN source_observations o ON o.source_id=s.id
               AND o.job_id=j.id WHERE s.runtime_id=j.runtime_id) END AS documents_read
         FROM jobs j WHERE j.runtime_id=? ORDER BY j.id DESC LIMIT 100"
    ).bind(id).fetch_all(&s.db).await?);
    Ok(Json(item))
}
async fn check_source(s: &AppState, id: i64, c: &SourceInput) -> Result<Value, ApiError> {
    parking::require_available(&mut *s.db.acquire().await?, id).await?;
    if c.url.is_some() == c.upload.is_some() {
        return Err(bad("Supply one manifest URL or uploaded JSON document"));
    }
    worker::preview(s, c).await.map_err(|e| {
        let mut message = format!("{e:#}");
        if c.mapping.is_none() && message.contains("missing artifacts for release") {
            message.push_str(". Inspect the manifest to choose a linked index or a release list, or set a manual field mapping.");
        }
        bad(&message)
    })
}
async fn preview(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<SourceInput>,
) -> ResultJson {
    Ok(Json(check_source(&s, id, &c).await?))
}
async fn add_source(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<SourceInput>,
) -> ResultJson {
    save_source(&s, id, None, c).await
}

async fn require_idle(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    runtime: i64,
) -> Result<(), ApiError> {
    parking::require_available(tx, runtime).await?;
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM jobs j WHERE j.runtime_id=? AND (
            j.state='running' OR (j.state='queued' AND EXISTS(
                SELECT 1 FROM sources s JOIN source_observations o ON o.source_id=s.id
                AND o.job_id=j.id WHERE s.runtime_id=j.runtime_id))))",
    )
    .bind(runtime)
    .fetch_one(&mut **tx)
    .await?;
    if busy {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Wait for the active sync to finish before editing sources".into(),
        ));
    }
    Ok(())
}

async fn save_source(
    s: &AppState,
    runtime: i64,
    replaces: Option<i64>,
    c: SourceInput,
) -> ResultJson {
    check_source(s, runtime, &c).await?;
    let kind = if c.url.is_some() { "url" } else { "upload" };
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    require_idle(&mut tx, runtime).await?;
    if let Some(old) = replaces {
        sqlx::query(
            "UPDATE sources SET enabled=0 WHERE id=? AND runtime_id=? AND enabled=1 RETURNING id",
        )
        .bind(old)
        .bind(runtime)
        .fetch_one(&mut *tx)
        .await?;
    }
    let id = sqlx::query(
        "INSERT INTO sources(runtime_id,kind,url,upload,mapping) VALUES(?,?,?,?,?) RETURNING id",
    )
    .bind(runtime)
    .bind(kind)
    .bind(c.url)
    .bind(c.upload.map(String::into_bytes))
    .bind(c.mapping.map(|v| v.to_string()))
    .fetch_one(&mut *tx)
    .await?
    .get::<i64, _>("id");
    tx.commit().await?;
    Ok(Json(json!({"id":id,"kind":kind})))
}

async fn replace_source(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(c): Json<SourceInput>,
) -> ResultJson {
    let runtime = sqlx::query_scalar("SELECT runtime_id FROM sources WHERE id=? AND enabled=1")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    save_source(&s, runtime, Some(id), c).await
}

async fn remove_source(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    let runtime: i64 =
        sqlx::query_scalar("SELECT runtime_id FROM sources WHERE id=? AND enabled=1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    require_idle(&mut tx, runtime).await?;
    sqlx::query("UPDATE sources SET enabled=0 WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE jobs SET state='cancelled',finished_at=unixepoch(),error='All sources removed'
                 WHERE runtime_id=? AND state='queued' AND NOT EXISTS(SELECT 1 FROM sources WHERE runtime_id=? AND enabled=1)")
        .bind(runtime).bind(runtime).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn queue_sync(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    runtime: i64,
) -> Result<(), ApiError> {
    parking::require_available(tx, runtime).await?;
    let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sources WHERE runtime_id=r.id AND enabled=1) FROM runtimes r WHERE r.id=?")
        .bind(runtime).fetch_one(&mut **tx).await?;
    if !ready {
        return Err(bad("Import a source before starting a sync"));
    }
    sqlx::query("INSERT OR IGNORE INTO jobs(runtime_id) VALUES(?)")
        .bind(runtime)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn sync(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    queue_sync(&mut tx, id).await?;
    let job = record(
        sqlx::query(
            "SELECT id,state FROM jobs WHERE runtime_id=? AND state IN('queued','running')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?,
    );
    tx.commit().await?;
    Ok(Json(job))
}
async fn transfer_list(s: &AppState) -> Result<Value, sqlx::Error> {
    Ok(records(sqlx::query("SELECT t.id,t.job_id,t.runtime_id,t.filename,t.host,t.backend,t.state,t.bytes,t.total,t.speed,t.retries,t.retry_at,t.error,p.name project,r.name runtime FROM transfers t JOIN runtimes r ON r.id=t.runtime_id JOIN projects p ON p.id=r.project_id ORDER BY CASE WHEN t.state IN ('completed','failed','cancelled') THEN 1 ELSE 0 END,t.id DESC LIMIT 1000").fetch_all(&s.db).await?))
}
async fn transfers(State(s): State<AppState>) -> ResultJson {
    Ok(Json(transfer_list(&s).await?))
}
async fn transfer_history(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    sqlx::query("SELECT id FROM transfers WHERE id=?")
        .bind(id)
        .fetch_one(&s.db)
        .await?;
    let rows=sqlx::query("SELECT created_at,state,bytes,retries,error FROM transfer_events WHERE transfer_id=? ORDER BY id DESC LIMIT 100")
        .bind(id).fetch_all(&s.db).await?;
    Ok(Json(json!({"history":records(rows)})))
}

async fn control(State(s): State<AppState>, Path((id, action)): Path<(i64, String)>) -> ResultJson {
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    let row=sqlx::query("SELECT t.state,t.runtime_id,t.command,j.state job_state FROM transfers t JOIN jobs j ON j.id=t.job_id WHERE t.id=?").bind(id).fetch_one(&mut *tx).await?;
    parking::require_available(&mut tx, row.get("runtime_id")).await?;
    let state = row.get::<String, _>("state");
    if row
        .get::<Option<String>, _>("command")
        .is_some_and(|pending| pending != action)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Another transfer command is pending".into(),
        ));
    }
    match (action.as_str(), state.as_str()) {
        ("retry", "failed" | "cancelled") => {
            if matches!(
                row.get::<String, _>("job_state").as_str(),
                "queued" | "running"
            ) {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "Wait for the current sync to finish before retrying".into(),
                ));
            }
            queue_sync(&mut tx, row.get("runtime_id")).await?;
        }
        ("pause", "queued" | "connecting" | "downloading" | "retrying")
        | ("resume", "paused")
        | ("cancel", "queued" | "connecting" | "downloading" | "retrying" | "paused")
        | ("retry", "retrying") => {
            sqlx::query("UPDATE transfers SET command=? WHERE id=?")
                .bind(action)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        _ => {
            return Err(bad(
                "This action is unavailable in the current transfer state",
            ));
        }
    }
    tx.commit().await?;
    Ok(Json(json!({"accepted":true})))
}
async fn events(State(s): State<AppState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    // Bound each connection lifetime so the browser periodically reauthenticates.
    let snapshots = stream::unfold(
        (s, headers, 0, String::new()),
        |(s, headers, n, last)| async move {
            if n >= 30 {
                return None;
            }
            if n > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await
            }
            if !crate::access::valid_session(&s.db, &headers).await {
                return None;
            }
            let fresh = transfer_list(&s).await.ok()?.to_string();
            let event = if fresh != last {
                Event::default().event("transfers").data(&fresh)
            } else {
                Event::default().comment("connected")
            };
            Some((Ok::<_, Infallible>(event), (s, headers, n + 1, fresh)))
        },
    );
    Sse::new(snapshots).keep_alive(KeepAlive::default())
}
struct Usage {
    modified: SystemTime,
    scanned: Instant,
    bytes: u64,
}
#[derive(Clone, Default)]
struct Storage(Arc<Mutex<Option<Usage>>>);

impl Storage {
    // Completed objects are immutable. Adding, replacing or removing an object changes the
    // directory mtime. Periodically rescan even when its filesystem has coarse timestamps.
    async fn bytes(self, dir: PathBuf) -> Result<u64, ApiError> {
        let scan = tokio::task::spawn_blocking(move || -> io::Result<u64> {
            let mut cache = self
                .0
                .lock()
                .map_err(|_| io::Error::other("Storage cache unavailable"))?;
            let modified = fs::metadata(&dir)?.modified()?;
            if let Some(last) = cache.as_ref()
                && last.modified == modified
                && last.scanned.elapsed() < Duration::from_secs(30)
            {
                return Ok(last.bytes);
            }
            let mut bytes = 0u64;
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.len() != 64
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
                {
                    continue;
                }
                match entry.metadata() {
                    Ok(m) if m.is_file() => bytes = bytes.saturating_add(m.len()),
                    Ok(_) => (),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
            }
            if fs::metadata(&dir)?.modified()? == modified {
                *cache = Some(Usage {
                    modified,
                    scanned: Instant::now(),
                    bytes,
                });
            }
            Ok(bytes)
        })
        .await;
        scan.ok().and_then(Result::ok).ok_or_else(|| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not inspect artifact storage".into(),
            )
        })
    }
}
async fn overview(State(s): State<AppState>, Extension(storage): Extension<Storage>) -> ResultJson {
    let mut item=record(sqlx::query("SELECT (SELECT COUNT(*) FROM projects WHERE parked_at IS NULL) projects,(SELECT COUNT(*) FROM available_runtimes) runtimes,(SELECT COUNT(*) FROM transfers WHERE state IN ('connecting','downloading','verifying')) active,(SELECT COUNT(*) FROM transfers WHERE state='queued') queued,(SELECT MAX(last_success) FROM available_runtimes) last_success,(SELECT COUNT(DISTINCT r.project_id) FROM available_runtimes r WHERE (SELECT state FROM jobs WHERE runtime_id=r.id ORDER BY id DESC LIMIT 1)='failed') attention,engine FROM settings").fetch_one(&s.db).await?);
    item["storage_bytes"] = json!(storage.bytes(s.data.join("artifacts")).await?);
    item["activity"]=records(sqlx::query("SELECT j.id,p.name project,r.name runtime,j.state,j.created_at,j.error FROM jobs j JOIN runtimes r ON r.id=j.runtime_id JOIN projects p ON p.id=r.project_id ORDER BY j.id DESC LIMIT 30").fetch_all(&s.db).await?);
    Ok(Json(item))
}
async fn curl_available() -> bool {
    tokio::process::Command::new("curl")
        .arg("--version")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}
async fn settings(State(s): State<AppState>, Extension(storage): Extension<Storage>) -> ResultJson {
    let mut item = record(
        sqlx::query("SELECT engine,concurrency,per_host,retries,sync_interval FROM settings")
            .fetch_one(&s.db)
            .await?,
    );
    item["storage_bytes"] = json!(storage.bytes(s.data.join("artifacts")).await?);
    item["version"] = json!(env!("CARGO_PKG_VERSION"));
    item["public_url"] = json!(s.public_url);
    item["curl_available"] = json!(curl_available().await);
    Ok(Json(item))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    engine: String,
    concurrency: i64,
    per_host: i64,
    retries: i64,
    sync_interval: i64,
}
async fn update_settings(
    State(s): State<AppState>,
    storage: Extension<Storage>,
    Json(c): Json<Settings>,
) -> ResultJson {
    if !matches!(c.engine.as_str(), "native" | "curl")
        || c.engine == "curl" && !curl_available().await
    {
        return Err(bad("Download engine unavailable"));
    }
    interval(c.sync_interval)?;
    if !(1..=32).contains(&c.concurrency)
        || !(1..=c.concurrency).contains(&c.per_host)
        || !(0..=12).contains(&c.retries)
    {
        return Err(bad(
            "Use 1–32 connections, a per-host limit within that total, and 0–12 retries",
        ));
    }
    sqlx::query("UPDATE settings SET engine=?,concurrency=?,per_host=?,retries=?,sync_interval=? WHERE id=1").bind(c.engine).bind(c.concurrency).bind(c.per_host).bind(c.retries).bind(c.sync_interval).execute(&s.db).await?;
    settings(State(s), storage).await
}

async fn files(s: &AppState, id: i64) -> Result<Value, sqlx::Error> {
    Ok(records(sqlx::query("SELECT id,release,filename,size,source_url,digest FROM files WHERE publication_id=? ORDER BY release,filename").bind(id).fetch_all(&s.db).await?))
}

async fn originals(s: &AppState, id: i64) -> Result<Value, sqlx::Error> {
    Ok(records(sqlx::query("SELECT o.id,o.source_id,o.page,o.url,o.digest,length(d.body) size FROM publication_sources ps JOIN source_observations o ON o.id=ps.observation_id JOIN raw_documents d ON d.digest=o.digest WHERE ps.publication_id=? ORDER BY o.source_id,o.page").bind(id).fetch_all(&s.db).await?))
}
async fn original(
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, ApiError> {
    let body: Vec<u8> = sqlx::query_scalar(
        "SELECT d.body FROM source_observations o JOIN raw_documents d USING(digest) WHERE o.id=?",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await?;
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "application/json"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=source.json",
            ),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        body,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn queued_job_keeps_its_captured_sources_during_recovery() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let state = crate::open(dir.path(), "https://mirror.example").await?;
        let mut tx = state.db.begin().await?;
        sqlx::raw_sql(
            "INSERT INTO projects(name,slug) VALUES('P','p');
             INSERT INTO runtimes(project_id,name,slug) VALUES(1,'R','r');
             INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',X'7B7D');
             INSERT INTO jobs(runtime_id,state) VALUES(1,'queued');",
        )
        .execute(&mut *tx)
        .await?;
        assert!(require_idle(&mut tx, 1).await.is_ok());
        sqlx::raw_sql(
            "INSERT INTO raw_documents(digest,body) VALUES('root',X'7B7D');
             INSERT INTO source_observations(source_id,job_id,page,digest) VALUES(1,1,0,'root');",
        )
        .execute(&mut *tx)
        .await?;
        assert_eq!(
            require_idle(&mut tx, 1).await.unwrap_err().0,
            StatusCode::CONFLICT
        );
        sqlx::query("UPDATE jobs SET state='completed' WHERE id=1")
            .execute(&mut *tx)
            .await?;
        assert!(require_idle(&mut tx, 1).await.is_ok());
        Ok(())
    }
}
