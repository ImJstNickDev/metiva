use super::adapter::ManifestAdapter;
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
fn link_header(headers: &HeaderMap) -> Result<Option<String>> {
    let values = headers
        .get_all(LINK)
        .iter()
        .map(|v| v.to_str())
        .collect::<Result<Vec<_>, _>>()?;
    Ok((!values.is_empty()).then(|| values.join(",")))
}
fn next_link(value: &str) -> Result<Option<String>> {
    let mut start = 0;
    let mut angle = false;
    let mut quoted = false;
    let mut escape = false;
    let mut links = Vec::new();
    for (i, c) in value.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if c == '\\' && quoted {
            escape = true;
            continue;
        }
        match c {
            '"' if !angle => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' if !angle && !quoted => {
                links.push(&value[start..i]);
                start = i + 1;
            }
            _ => (),
        }
    }
    ensure!(!angle && !quoted, "malformed pagination Link header");
    links.push(&value[start..]);
    let mut next = None;
    for link in links {
        let (target, params) = link
            .trim()
            .strip_prefix('<')
            .and_then(|v| v.split_once('>'))
            .context("invalid pagination Link header")?;
        let rel = params
            .split(';')
            .filter_map(|v| v.trim().split_once('='))
            .any(|(k, v)| {
                k.trim().eq_ignore_ascii_case("rel")
                    && v.trim()
                        .trim_matches('"')
                        .split_whitespace()
                        .any(|r| r.eq_ignore_ascii_case("next"))
            });
        if rel {
            ensure!(
                next.as_deref().is_none_or(|v| v == target),
                "conflicting pagination links"
            );
            next = Some(target.to_string());
        }
    }
    Ok(next)
}

async fn remote(
    state: &AppState,
    source: i64,
    url: &Url,
    conditional: bool,
    max_retries: u32,
    limits: Option<&http::Limits>,
) -> Result<Page> {
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
    let validators = headers.contains_key(IF_NONE_MATCH) || headers.contains_key(IF_MODIFIED_SINCE);
    let mut retries = 0;
    let response = loop {
        let fetched =
            http::limited_get(&state.db, url.clone(), headers.clone(), false, limits).await;
        if retries == max_retries
            || fetched
                .as_ref()
                .is_ok_and(|r| !matches!(r.status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504))
        {
            break fetched?;
        }
        let backoff = 2_i64.pow(retries.min(8) + 1) + i64::from(rand::random::<u8>() % 3);
        let delay = http::cooldown(&state.db, url.host_str().context("missing host")?)
            .await?
            .map(|d| d - crate::unix_time())
            .unwrap_or(0)
            .max(backoff);
        ensure!(
            delay <= 120,
            "source is temporarily rate limited; the worker will check again on its next scheduled sync"
        );
        drop(fetched);
        tokio::time::sleep(std::time::Duration::from_secs(delay as u64)).await;
        retries += 1;
    };
    if response.status == StatusCode::NOT_MODIFIED {
        ensure!(validators, "unsolicited 304 without a conditional request");
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
            link: link_header(&response.headers)?.or_else(|| row.get("next_link")),
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
    let link = link_header(&response.headers)?;
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
    limits: Option<&http::Limits>,
) -> Result<Vec<Observation>> {
    ensure!(
        source.url.is_some() != source.upload.is_some(),
        "expected one manifest URL or upload"
    );
    let mut next = source.url.clone();
    let mut seen = HashSet::new();
    let mut total = 0;
    let max_retries = if job.is_some() {
        super::settings(state).await?.retries
    } else {
        0
    };
    let mut observations = Vec::new();
    for index in 0..MAX_PAGES {
        let request = next.take();
        let page = if let Some(url) = &request {
            ensure!(seen.insert(url.clone()), "pagination cycle");
            remote(state, source.id, url, conditional, max_retries, limits).await?
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
        next = adapter::GenericJson.parse_page(
            &page.bytes,
            &source.mapping,
            page.url.as_ref(),
            &mut local,
        )?;
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
            && let Some(link) = page.link.as_deref().map(next_link).transpose()?.flatten()
        {
            next = Some(
                page.url
                    .as_ref()
                    .context("pagination requires a base URL")?
                    .join(&link)?,
            );
        }
        if next.is_none() {
            let document: serde_json::Value = serde_json::from_slice(&page.bytes)?;
            ensure!(
                ![
                    "/has_more",
                    "/hasMore",
                    "/pagination/has_more",
                    "/pageInfo/hasNextPage"
                ]
                .iter()
                .any(|p| document.pointer(p).and_then(serde_json::Value::as_bool) == Some(true)),
                "manifest reports additional pages without a next URL"
            );
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
    limits: &http::Limits,
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
        match collect(
            state,
            source,
            Some(job),
            conditional,
            &mut catalog,
            Some(limits),
        )
        .await
        {
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

/// Reconstruct the exact admitted input without polling a mutable upstream during recovery.
pub(crate) async fn restore(
    state: &AppState,
    job: i64,
    ids: &str,
    sources: &[Source],
) -> Result<(Catalog, Vec<Observation>)> {
    use futures_util::TryStreamExt;
    let by_id: std::collections::BTreeMap<_, _> = sources.iter().map(|s| (s.id, s)).collect();
    let mut seen = HashSet::new();
    let mut pages=sqlx::query("SELECT o.id,o.source_id,o.digest,o.url,d.body FROM json_each(?) j JOIN source_observations o ON o.id=j.value JOIN raw_documents d USING(digest) WHERE o.job_id=? ORDER BY j.key").bind(ids).bind(job).fetch(&state.db);
    let mut catalog = Catalog::new();
    let mut observations = Vec::new();
    while let Some(row) = pages.try_next().await? {
        let id: i64 = row.get("source_id");
        let source = by_id
            .get(&id)
            .context("accepted source changed during recovery")?;
        seen.insert(id);
        let url = row
            .get::<Option<String>, _>("url")
            .as_deref()
            .map(Url::parse)
            .transpose()?;
        adapter::GenericJson.parse_page(
            &row.get::<Vec<u8>, _>("body"),
            &source.mapping,
            url.as_ref(),
            &mut catalog,
        )?;
        observations.push(Observation {
            id: row.get("id"),
            source: id,
            digest: row.get("digest"),
        });
    }
    ensure!(
        seen.len() == sources.len()
            && serde_json::from_str::<Vec<i64>>(ids)?.len() == observations.len(),
        "accepted input set is incomplete"
    );
    Ok((catalog, observations))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_next_relation() {
        assert_eq!(
            next_link("<prev>; rel=\"prev\", </feed?page=2>; rel=\"next\"")
                .unwrap()
                .as_deref(),
            Some("/feed?page=2")
        );
    }
}
