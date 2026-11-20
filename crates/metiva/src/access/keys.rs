use super::*;
use axum::{
    extract::{Path, State},
    middleware,
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, Sqlite, Transaction};

pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/admin/keys", get(list).post(create))
        .route("/api/admin/keys/{id}", patch(rename).delete(revoke))
        .route("/api/admin/keys/{id}/rotate", post(rotate))
        .route_layer(middleware::from_fn_with_state(state, require_admin))
}

#[derive(Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct Metadata {
    id: i64,
    name: String,
    prefix: String,
    scope: String,
    project_id: Option<i64>,
    runtime_id: Option<i64>,
    created_at: i64,
    last_used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NewKey {
    name: String,
    scope: String,
    project_id: Option<i64>,
    runtime_id: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    name: String,
}

#[derive(Serialize)]
struct Issued {
    #[serde(flatten)]
    metadata: Metadata,
    token: String,
}

async fn list(State(state): State<AppState>) -> Result<Json<Vec<Metadata>>> {
    let rows = sqlx::query_as("SELECT * FROM api_keys ORDER BY id DESC")
        .fetch_all(&state.db)
        .await?;
    Ok(Json(rows))
}

fn clean_name(value: &str) -> Result<&str> {
    let name = value.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        Err(invalid("Key name must contain 1–120 bytes"))
    } else {
        Ok(name)
    }
}

async fn issue(tx: &mut Transaction<'_, Sqlite>, input: NewKey) -> Result<Issued> {
    let name = clean_name(&input.name)?;
    let project = match (input.scope.as_str(), input.project_id, input.runtime_id) {
        ("global", None, None) => None,
        ("project", Some(id), None) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM projects WHERE id=?)")
                    .bind(id)
                    .fetch_one(&mut **tx)
                    .await?;
            if !exists {
                return Err(invalid("Unknown project"));
            }
            Some(id)
        }
        ("runtime", project, Some(runtime)) => {
            let parent: i64 = sqlx::query_scalar("SELECT project_id FROM runtimes WHERE id=?")
                .bind(runtime)
                .fetch_optional(&mut **tx)
                .await?
                .ok_or(invalid("Unknown runtime"))?;
            if project.is_some_and(|p| p != parent) {
                return Err(invalid("Runtime is outside this project"));
            }
            Some(parent)
        }
        _ => {
            return Err(invalid(
                "Specify a global, project, or runtime scope with its matching identifiers",
            ));
        }
    };
    let raw = token("mtv_");
    let metadata=sqlx::query_as("INSERT INTO api_keys(name,prefix,digest,scope,project_id,runtime_id) VALUES(?,?,?,?,?,?) RETURNING *")
        .bind(name).bind(&raw[..12]).bind(digest(&raw)).bind(input.scope).bind(project).bind(input.runtime_id).fetch_one(&mut **tx).await?;
    Ok(Issued {
        metadata,
        token: raw,
    })
}

async fn create(
    State(state): State<AppState>,
    Json(input): Json<NewKey>,
) -> Result<(StatusCode, Json<Issued>)> {
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let issued = issue(&mut tx, input).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(issued)))
}

async fn rotate(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<(StatusCode, Json<Issued>)> {
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let prior: Metadata =
        sqlx::query_as("SELECT * FROM api_keys WHERE id=? AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(missing)?;
    let issued = issue(
        &mut tx,
        NewKey {
            name: prior.name,
            scope: prior.scope,
            project_id: prior.project_id,
            runtime_id: prior.runtime_id,
        },
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(issued)))
}

async fn rename(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<Rename>,
) -> Result<Json<Metadata>> {
    let name = clean_name(&input.name)?;
    let row = sqlx::query_as("UPDATE api_keys SET name=? WHERE id=? RETURNING *")
        .bind(name)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(missing)?;
    Ok(Json(row))
}

async fn revoke(State(state): State<AppState>, Path(id): Path<i64>) -> Result<StatusCode> {
    let n =
        sqlx::query("UPDATE api_keys SET revoked_at=coalesce(revoked_at,unixepoch()) WHERE id=?")
            .bind(id)
            .execute(&state.db)
            .await?
            .rows_affected();
    if n == 0 {
        return Err(missing());
    }
    Ok(StatusCode::NO_CONTENT)
}
