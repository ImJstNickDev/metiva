use super::adapter::Catalog;
use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, QueryBuilder, SqliteConnection, SqlitePool};

#[derive(FromRow)]
struct Entry {
    release: String,
    filename: String,
    source_url: String,
    metadata: String,
}

impl Entry {
    // Both the catalog's BTreeMap and the migration's ORDER BY use binary
    // (release, filename) order. Length prefixes make field boundaries explicit.
    fn digest(entries: &[Self]) -> [u8; 32] {
        let mut hash = Sha256::new();
        for entry in entries {
            for field in [
                &entry.release,
                &entry.filename,
                &entry.source_url,
                &entry.metadata,
            ] {
                hash.update((field.len() as u64).to_be_bytes());
                hash.update(field.as_bytes());
            }
        }
        hash.finalize().into()
    }
}

pub(super) async fn record(db: &SqlitePool, observation: i64, catalog: &Catalog) -> Result<()> {
    let mut entries = Vec::with_capacity(catalog.assets.len());
    for ((release, filename), artifact) in &catalog.assets {
        entries.push(Entry {
            release: release.clone(),
            filename: filename.clone(),
            source_url: artifact.url.to_string(),
            metadata: serde_json::to_string(artifact)?,
        });
    }
    let digest = Entry::digest(&entries);
    let mut tx = db.begin_with("BEGIN IMMEDIATE").await?;
    attach(&mut tx, observation, digest, &entries).await?;
    tx.commit().await?;
    Ok(())
}

async fn attach(
    conn: &mut SqliteConnection,
    observation: i64,
    digest: [u8; 32],
    entries: &[Entry],
) -> Result<()> {
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT id FROM parsed_inventories WHERE digest=?")
            .bind(digest.as_slice())
            .fetch_optional(&mut *conn)
            .await?;
    let inventory = match existing {
        Some(id) => id,
        None => {
            let id = sqlx::query("INSERT INTO parsed_inventories(digest) VALUES(?)")
                .bind(digest.as_slice())
                .execute(&mut *conn)
                .await?
                .last_insert_rowid();
            // Keep each insert below SQLite's bound-parameter limit.
            for chunk in entries.chunks(500) {
                let mut insert = QueryBuilder::new(
                    "INSERT INTO inventory_artifacts(inventory_id,release,filename,source_url,metadata) ",
                );
                insert
                    .push_values(chunk, |mut row, entry| {
                        row.push_bind(id)
                            .push_bind(&entry.release)
                            .push_bind(&entry.filename)
                            .push_bind(&entry.source_url)
                            .push_bind(&entry.metadata);
                    })
                    .build()
                    .execute(&mut *conn)
                    .await?;
            }
            id
        }
    };
    let attached = sqlx::query(
        "UPDATE source_observations SET inventory_id=?
         WHERE id=? AND (inventory_id IS NULL OR inventory_id=?)",
    )
    .bind(inventory)
    .bind(observation)
    .bind(inventory)
    .execute(conn)
    .await?;
    ensure!(
        attached.rows_affected() == 1,
        "observation {observation} is missing or already has a different inventory"
    );
    Ok(())
}

/// The startup migration lock excludes other new processes. Commit each page
/// with its legacy-row removal, so an interrupted upgrade resumes at the next
/// page. An old observation with no rows may be a parse failure and stays unset.
pub(crate) async fn migrate_legacy(db: &SqlitePool) -> Result<()> {
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='legacy_observation_artifacts')",
    ).fetch_one(db).await?;
    if !pending {
        return Ok(());
    }
    loop {
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await?;
        let observation: Option<i64> =
            sqlx::query_scalar("SELECT min(observation_id) FROM legacy_observation_artifacts")
                .fetch_one(&mut *tx)
                .await?;
        let Some(observation) = observation else {
            sqlx::query("DROP TABLE legacy_observation_artifacts")
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(());
        };
        let entries = sqlx::query_as(
            "SELECT release,filename,source_url,metadata FROM legacy_observation_artifacts
             WHERE observation_id=? ORDER BY release,filename",
        )
        .bind(observation)
        .fetch_all(&mut *tx)
        .await?;
        attach(&mut tx, observation, Entry::digest(&entries), &entries).await?;
        sqlx::query("DELETE FROM legacy_observation_artifacts WHERE observation_id=?")
            .bind(observation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
}

#[cfg(test)]
mod tests;
