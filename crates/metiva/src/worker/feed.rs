use super::{
    adapter::{self, Catalog, Mapping},
    http,
};
use crate::AppState;
use anyhow::{Context, Result, ensure};
use reqwest::{
    StatusCode,
    header::{
        CACHE_CONTROL, ETAG, HeaderMap, HeaderName, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH,
        LAST_MODIFIED, LINK,
    },
};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashSet;
use url::Url;

const PAGE_BYTES: usize = 8 << 20;
const SOURCE_BYTES: usize = 32 << 20;
const MAX_PAGES: usize = 128;

pub(crate) struct Source {
    pub id: i64,
    pub url: Option<Url>,
    pub upload: Option<Vec<u8>>,
    pub mapping: Mapping,
}
pub(crate) struct Observation {
    pub id: i64,
    pub digest: String,
    pub source: i64,
}
struct Page {
    bytes: Vec<u8>,
    url: Option<Url>,
    etag: Option<String>,
    modified: Option<String>,
    link: Option<String>,
}

fn header(headers: &HeaderMap, key: HeaderName) -> Option<String> {
    headers.get(key)?.to_str().ok().map(str::to_string)
}
fn next_link(value: &str) -> Option<String> {
    value.split(',').find_map(|part| {
        let (target, params) = part.trim().strip_prefix('<')?.split_once('>')?;
        let next = params
            .split(';')
            .filter_map(|p| p.trim().split_once('='))
            .any(|(k, v)| {
                k.eq_ignore_ascii_case("rel")
                    && v.trim_matches('"').split_whitespace().any(|r| r == "next")
            });
        next.then(|| target.to_owned())
    })
}

async fn remote(state: &AppState, source: i64, url: &Url, conditional: bool) -> Result<Page> {
    let cached = if conditional {
        sqlx::query("SELECT o.url,o.etag,o.last_modified,o.next_link,o.digest FROM source_observations o WHERE source_id=? AND request_url=? ORDER BY id DESC LIMIT 1").bind(source).bind(url.as_str()).fetch_optional(&state.db).await?
    } else {
        None
    };
    let mut headers = HeaderMap::new();
    if !conditional {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    if let Some(row) = &cached {
        for (column, key) in [
            ("etag", IF_NONE_MATCH),
            ("last_modified", IF_MODIFIED_SINCE),
        ] {
            if let Some(v) = row.get::<Option<String>, _>(column) {
                headers.insert(key, HeaderValue::from_str(&v)?);
            }
        }
    }
    let response = http::get(url.clone(), headers, false).await?;
    if response.status == StatusCode::NOT_MODIFIED {
        let row = cached.context("unexpected 304 without a previous observation")?;
        ensure!(
            row.get::<Option<String>, _>("url").as_deref() == Some(response.url.as_str()),
            "304 from changed destination"
        );
        let bytes = sqlx::query_scalar("SELECT body FROM raw_documents WHERE digest=?")
            .bind(row.get::<String, _>("digest"))
            .fetch_one(&state.db)
            .await?;
        return Ok(Page {
            bytes,
            url: Some(response.url),
            etag: header(&response.headers, ETAG).or_else(|| row.get("etag")),
            modified: header(&response.headers, LAST_MODIFIED).or_else(|| row.get("last_modified")),
            link: header(&response.headers, LINK).or_else(|| row.get("next_link")),
        });
    }
    ensure!(
        response.status == StatusCode::OK,
        "manifest server returned HTTP {}",
        response.status.as_u16()
    );
    let page_url = response.url.clone();
    let etag = header(&response.headers, ETAG);
    let modified = header(&response.headers, LAST_MODIFIED);
    let link = header(&response.headers, LINK);
    Ok(Page {
        bytes: response.bounded(PAGE_BYTES).await?,
        url: Some(page_url),
        etag,
        modified,
        link,
    })
}

async fn save(
    state: &AppState,
    source: i64,
    job: i64,
    index: usize,
    request: Option<&Url>,
    page: &Page,
) -> Result<Observation> {
    let digest = hex::encode(Sha256::digest(&page.bytes));
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query("INSERT OR IGNORE INTO raw_documents(digest,body) VALUES(?,?)")
        .bind(&digest)
        .bind(&page.bytes)
        .execute(&mut *tx)
        .await?;
    let id=sqlx::query("INSERT INTO source_observations(source_id,job_id,page,url,digest,etag,last_modified,request_url,next_link) VALUES(?,?,?,?,?,?,?,?,?)").bind(source).bind(job).bind(index as i64).bind(page.url.as_ref().map(Url::as_str)).bind(&digest).bind(&page.etag).bind(&page.modified).bind(request.map(Url::as_str)).bind(&page.link).execute(&mut *tx).await?.last_insert_rowid();
    if index == 0 {
        sqlx::query("UPDATE sources SET etag=?,last_modified=? WHERE id=?")
            .bind(&page.etag)
            .bind(&page.modified)
            .bind(source)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Observation { id, digest, source })
}

pub(crate) async fn collect(
    state: &AppState,
    source: &Source,
    job: Option<i64>,
    conditional: bool,
    catalog: &mut Catalog,
) -> Result<Vec<Observation>> {
    ensure!(
        source.url.is_some() != source.upload.is_some(),
        "expected one manifest URL or upload"
    );
    let mut next = source.url.clone();
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut observations = Vec::new();
    for index in 0..MAX_PAGES {
        let request = next.take();
        let page = if let Some(url) = &request {
            ensure!(seen.insert(url.clone()), "pagination cycle");
            remote(state, source.id, url, conditional).await?
        } else {
            let bytes = source.upload.as_ref().context("missing source data")?;
            ensure!(
                bytes.len() <= PAGE_BYTES,
                "uploaded source exceeds byte limit"
            );
            Page {
                bytes: bytes.clone(),
                url: None,
                etag: None,
                modified: None,
                link: None,
            }
        };
        total += page.bytes.len();
        ensure!(total <= SOURCE_BYTES, "source exceeds byte limit");
        if let Some(job) = job {
            observations.push(save(state, source.id, job, index, request.as_ref(), &page).await?);
        }
        let mut local = Catalog::new();
        next = adapter::parse(&page.bytes, &source.mapping, page.url.as_ref(), &mut local)?;
        if job.is_some() {
            let observation = observations.last().context("missing snapshot identity")?;
            let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
            for ((version, name), item) in &local.assets {
                sqlx::query("INSERT INTO observation_artifacts(observation_id,release,filename,source_url,metadata) VALUES(?,?,?,?,?)").bind(observation.id).bind(version).bind(name).bind(item.url.as_str()).bind(serde_json::to_string(item)?).execute(&mut *tx).await?;
            }
            tx.commit().await?;
        }
        adapter::merge(catalog, local)?;
        if next.is_none()
            && source.mapping.next_path.is_none()
            && let Some(link) = page.link.as_deref().and_then(next_link)
        {
            next = Some(
                page.url
                    .as_ref()
                    .context("pagination requires a base URL")?
                    .join(&link)?,
            );
        }
        if next.is_none() {
            return Ok(observations);
        }
    }
    anyhow::bail!("pagination exceeds page limit")
}

pub(crate) async fn observe(
    state: &AppState,
    sources: &[Source],
    job: i64,
    conditional: bool,
) -> Result<(Catalog, Vec<Observation>)> {
    let mut catalog = Catalog::new();
    let mut observations = Vec::new();
    for source in sources {
        sqlx::query(
            "UPDATE sources SET last_checked=unixepoch(),status='checking',error=NULL WHERE id=?",
        )
        .bind(source.id)
        .execute(&state.db)
        .await?;
        match collect(state, source, Some(job), conditional, &mut catalog).await {
            Ok(rows) => {
                observations.extend(rows);
                sqlx::query(
                    "UPDATE sources SET status='ready',last_success=unixepoch() WHERE id=?",
                )
                .bind(source.id)
                .execute(&state.db)
                .await?;
            }
            Err(e) => {
                sqlx::query("UPDATE sources SET status='failed',error=? WHERE id=?")
                    .bind(format!("{e:#}"))
                    .bind(source.id)
                    .execute(&state.db)
                    .await?;
                return Err(e.context(format!("source {}", source.id)));
            }
        }
    }
    Ok((catalog, observations))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_next_relation() {
        assert_eq!(
            next_link("<prev>; rel=\"prev\", </feed?page=2>; rel=\"next\"").as_deref(),
            Some("/feed?page=2")
        );
    }
}
