use super::*;
use axum::extract::Query;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/runtimes/{id}/publications", get(publications))
        .route(
            "/api/admin/runtimes/{id}/publications/{pub_id}",
            get(publication),
        )
        .route("/api/admin/files/{id}/history", get(history))
        .route("/api/admin/file-revisions/{id}/download", get(download))
}

#[derive(Deserialize)]
struct Page {
    before: Option<i64>,
}

const PAGE_SIZE: usize = 50;

fn cursor(rows: &mut Vec<SqliteRow>) -> Option<i64> {
    if rows.len() <= PAGE_SIZE {
        return None;
    }
    rows.truncate(PAGE_SIZE);
    rows.last().map(|row| row.get("id"))
}

async fn publications(
    State(state): State<AppState>,
    Path(runtime): Path<i64>,
    Query(page): Query<Page>,
) -> ResultJson {
    sqlx::query("SELECT id FROM runtimes WHERE id=?")
        .bind(runtime)
        .fetch_one(&state.db)
        .await?;
    let mut rows = sqlx::query(
        "SELECT p.id,p.created_at,p.id IS r.active_publication AS current
         FROM publications p JOIN runtimes r ON r.id=p.runtime_id
         WHERE p.runtime_id=? AND p.id<? ORDER BY p.id DESC LIMIT ?",
    )
    .bind(runtime)
    .bind(page.before.unwrap_or(i64::MAX))
    .bind(PAGE_SIZE as i64 + 1)
    .fetch_all(&state.db)
    .await?;
    let next = cursor(&mut rows);
    let snapshots: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id":row.get::<i64,_>("id"),
                "created_at":row.get::<i64,_>("created_at"),
                "current":row.get::<bool,_>("current")
            })
        })
        .collect();
    Ok(Json(json!({"publications":snapshots,"next_before":next})))
}

async fn publication(
    State(state): State<AppState>,
    Path((runtime, id)): Path<(i64, i64)>,
) -> ResultJson {
    let row =
        sqlx::query("SELECT id,created_at,manifest FROM publications WHERE runtime_id=? AND id=?")
            .bind(runtime)
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    let manifest: Value = serde_json::from_str(row.get("manifest")).map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored manifest is invalid".into(),
        )
    })?;
    let originals = originals(&state, id).await?;
    Ok(Json(
        json!({"id":id,"created_at":row.get::<i64,_>("created_at"),"manifest":manifest,"originals":originals}),
    ))
}

async fn history(
    State(state): State<AppState>,
    Path(file): Path<i64>,
    Query(page): Query<Page>,
) -> ResultJson {
    // Resolve the immutable snapshot row to the stable file identity first.
    let (runtime,release,filename): (i64,String,String) = sqlx::query_as(
        "SELECT p.runtime_id,f.release,f.filename FROM files f JOIN publications p ON p.id=f.publication_id WHERE f.id=?"
    ).bind(file).fetch_one(&state.db).await?;
    let mut rows = sqlx::query(
        "SELECT h.file_id AS id,p.created_at,f.digest,f.size,f.source_url,h.job_id,
                h.file_id=(SELECT max(latest.file_id) FROM file_revisions latest
                           WHERE latest.runtime_id=h.runtime_id AND latest.release=h.release AND latest.filename=h.filename)
                AND EXISTS(SELECT 1 FROM files current WHERE current.publication_id=r.active_publication
                           AND current.release=h.release AND current.filename=h.filename AND current.digest=f.digest) AS current
         FROM file_revisions h JOIN files f ON f.id=h.file_id JOIN publications p ON p.id=f.publication_id
         JOIN runtimes r ON r.id=h.runtime_id
         WHERE h.runtime_id=? AND h.release=? AND h.filename=? AND h.file_id < ?
         ORDER BY h.file_id DESC LIMIT ?"
    ).bind(runtime).bind(release).bind(filename).bind(page.before.unwrap_or(i64::MAX)).bind(PAGE_SIZE as i64+1).fetch_all(&state.db).await?;
    let next = cursor(&mut rows);
    let revisions: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let id: i64 = row.get("id");
            let mut revision = record(row);
            revision["current"] = json!(revision["current"].as_i64() == Some(1));
            revision["download_url"] = json!(format!("/api/admin/file-revisions/{id}/download"));
            revision
        })
        .collect();
    Ok(Json(json!({"revisions":revisions,"next_before":next})))
}

async fn download(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let (digest,filename) = sqlx::query_as(
        "SELECT f.digest,f.filename FROM file_revisions h JOIN files f ON f.id=h.file_id WHERE h.file_id=?"
    ).bind(id).fetch_one(&state.db).await?;
    Ok(crate::access::consumer::attachment(digest, filename)
        .unwrap_or_else(IntoResponse::into_response))
}
