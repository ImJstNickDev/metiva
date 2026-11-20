pub mod access;
pub mod admin;
pub mod worker;

use anyhow::{Context, Result, ensure};
use axum::{Router, extract::DefaultBodyLimit, middleware, routing::get};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub data: PathBuf,
    pub public_url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    pub url: Option<String>,
    pub upload: Option<String>,
    pub mapping: Option<Value>,
}

pub async fn open(data: &Path, origin: &str) -> Result<AppState> {
    let url = url::Url::parse(origin).context("METIVA_PUBLIC_URL requires an HTTP(S) origin")?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "Invalid METIVA_PUBLIC_URL origin"
    );
    for dir in ["db", "artifacts", "staging", "manifests"] {
        tokio::fs::create_dir_all(data.join(dir)).await?;
    }
    let lock_path = data.join("db/migrations.lock");
    let _migration_lock = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock_path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await??;
    let options = SqliteConnectOptions::new()
        .filename(data.join("db/metiva.sqlite3"))
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(15))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full);
    let db = SqlitePoolOptions::new()
        .max_connections(6)
        .connect_with(options)
        .await?;
    sqlx::migrate!("./migrations").run(&db).await?;
    Ok(AppState {
        db,
        data: data.into(),
        public_url: url.origin().ascii_serialization(),
    })
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .merge(admin::router().route_layer(middleware::from_fn_with_state(
            state.clone(),
            access::require_admin,
        )))
        .merge(access::router(state.clone()))
        .route("/healthz", get(health))
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024))
        .with_state(state)
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::http::StatusCode {
    if sqlx::query("SELECT 1").execute(&state.db).await.is_ok() {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    }
}
