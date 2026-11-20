use super::*;
use sqlx::SqliteConnection;

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/archive", get(archive))
        .route("/api/admin/projects/{id}/park", post(park_project))
        .route("/api/admin/projects/{id}/restore", post(restore_project))
        .route("/api/admin/runtimes/{id}/park", post(park_runtime))
        .route("/api/admin/runtimes/{id}/restore", post(restore_runtime))
}

async fn archive(State(s): State<AppState>) -> ResultJson {
    let rows = sqlx::query(
        "SELECT 'project' kind,p.id,p.name,p.slug,p.id project_id,p.name project_name,p.parked_at,0 parent_parked
         FROM projects p WHERE p.parked_at IS NOT NULL
         UNION ALL SELECT 'runtime',r.id,r.name,r.slug,p.id,p.name,r.parked_at,p.parked_at IS NOT NULL
         FROM runtimes r JOIN projects p ON p.id=r.project_id WHERE r.parked_at IS NOT NULL
         ORDER BY parked_at DESC,kind,name,id"
    ).fetch_all(&s.db).await?;
    let items: Vec<_> = rows
        .into_iter()
        .map(|r| {
            let mut item = record(r);
            item["parent_parked"] = json!(item["parent_parked"].as_i64() == Some(1));
            item
        })
        .collect();
    Ok(Json(json!(items)))
}

pub(super) async fn require_available(
    conn: &mut SqliteConnection,
    runtime: i64,
) -> Result<(), ApiError> {
    let available: bool = sqlx::query_scalar(
        "SELECT r.parked_at IS NULL AND p.parked_at IS NULL FROM runtimes r JOIN projects p ON p.id=r.project_id WHERE r.id=?"
    ).bind(runtime).fetch_one(conn).await?;
    if !available {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "Restore the mirror before starting work".into(),
        ));
    }
    Ok(())
}

enum Target {
    Project,
    Runtime,
}

async fn change(s: AppState, target: Target, id: i64, park: bool) -> ResultJson {
    let mut tx = s.db.begin_with("BEGIN IMMEDIATE").await?;
    if matches!(target, Target::Runtime) && !park {
        let parent_parked: bool = sqlx::query_scalar("SELECT p.parked_at IS NOT NULL FROM projects p JOIN runtimes r ON p.id=r.project_id WHERE r.id=?")
            .bind(id).fetch_one(&mut *tx).await?;
        if parent_parked {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Restore the project first".into(),
            ));
        }
    }
    let update = match target {
        Target::Project => {
            "UPDATE projects SET parked_at=CASE WHEN ? THEN COALESCE(parked_at,unixepoch()) END WHERE id=? RETURNING id,parked_at"
        }
        Target::Runtime => {
            "UPDATE runtimes SET parked_at=CASE WHEN ? THEN COALESCE(parked_at,unixepoch()) END WHERE id=? RETURNING id,parked_at"
        }
    };
    let result = record(
        sqlx::query(update)
            .bind(park)
            .bind(id)
            .fetch_one(&mut *tx)
            .await?,
    );
    if park {
        sqlx::query("UPDATE transfers SET state='cancelled',command=NULL,speed=0,retry_at=NULL,error='Mirror parked'
                     WHERE state NOT IN ('completed','cancelled','failed') AND runtime_id NOT IN (SELECT id FROM available_runtimes)")
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE jobs SET state='cancelled',finished_at=unixepoch(),error='Mirror parked'
                     WHERE state IN ('queued','running') AND runtime_id NOT IN (SELECT id FROM available_runtimes)")
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(result))
}
async fn park_project(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    change(s, Target::Project, id, true).await
}
async fn restore_project(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    change(s, Target::Project, id, false).await
}
async fn park_runtime(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    change(s, Target::Runtime, id, true).await
}
async fn restore_runtime(State(s): State<AppState>, Path(id): Path<i64>) -> ResultJson {
    change(s, Target::Runtime, id, false).await
}
