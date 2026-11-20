use super::{
    adapter::ManifestAdapter,
    selection::{self, Discovery, Index, Matches},
};
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
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{HashSet, VecDeque};
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

#[derive(Default)]
struct Chain {
    key: Option<String>,
    visited: u64,
    pages: Option<u64>,
    count: Option<u64>,
    rows: u64,
}

fn agree(value: &mut Option<u64>, reported: Option<u64>, label: &str) -> Result<()> {
    if let Some(n) = reported {
        ensure!(
            value.is_none_or(|old| old == n),
            "conflicting {label} in pagination"
        );
        *value = Some(n);
    }
    Ok(())
}
impl Chain {
    fn advance(&mut self, doc: &Value, next: Option<&Url>, rows: Option<u64>) -> Result<()> {
        self.visited += 1;
        // A standalone 'page' or 'count' can be application metadata. Treat a
        // numerical page/pages envelope or an index page declaration as pagination.
        let pagination = doc
            .get("pagination")
            .filter(|v| v.is_object())
            .unwrap_or(doc);
        if pagination.get("pages").is_some_and(Value::is_number)
            || pagination.get("total_pages").is_some()
            || self.pages.is_some()
        {
            for field in ["pages", "total_pages"] {
                agree(
                    &mut self.pages,
                    selection::number(pagination, field)?,
                    "page totals",
                )?;
            }
            if let Some(n) = selection::number(pagination, "page")? {
                ensure!(n == self.visited, "unexpected page number");
            }
            if let Some(n) = self.pages {
                ensure!(
                    self.visited <= n && (self.visited < n) == next.is_some(),
                    "page total disagrees with available next URL"
                );
            }
            if self.pages == Some(1) {
                agree(
                    &mut self.count,
                    selection::number(pagination, "count")?,
                    "release counts",
                )?;
            }
            agree(
                &mut self.count,
                selection::number(pagination, "total_count")?,
                "release counts",
            )?;
        }
        if let Some(rows) = rows {
            self.rows = self
                .rows
                .checked_add(rows)
                .context("too many release rows")?;
            if next.is_none() {
                ensure!(
                    self.count.is_none_or(|n| self.rows == n),
                    "release count disagrees with index"
                );
            }
        }
        if next.is_none() {
            ensure!(
                ![
                    "/has_more",
                    "/hasMore",
                    "/pagination/has_more",
                    "/pagination/has_next",
                    "/has_next",
                    "/pageInfo/hasNextPage"
                ]
                .iter()
                .any(|p| doc.pointer(p).and_then(Value::as_bool) == Some(true)),
                "manifest reports additional pages without a next URL"
            );
        }
        Ok(())
    }
}
struct Target {
    url: Option<Url>,
    chain: Chain,
    root: bool,
}
struct Walk {
    pending: VecDeque<Target>,
    strict: bool,
    matches: Matches,
    seen: HashSet<Url>,
    bytes: usize,
    pages: usize,
}
impl Walk {
    fn new(url: Option<Url>, strict: bool) -> Self {
        Self {
            strict,
            matches: Matches::default(),
            pending: VecDeque::from([Target {
                url,
                chain: Chain::default(),
                root: true,
            }]),
            seen: HashSet::new(),
            bytes: 0,
            pages: 0,
        }
    }
    fn take(&mut self) -> Result<Option<Target>> {
        let Some(target) = self.pending.pop_front() else {
            return Ok(None);
        };
        ensure!(self.pages < MAX_PAGES, "pagination exceeds page limit");
        if let Some(url) = &target.url {
            ensure!(
                self.seen.insert(url.clone()),
                "pagination cycle or repeated index target"
            );
        }
        self.pages += 1;
        Ok(Some(target))
    }
    fn accept(
        &mut self,
        mut target: Target,
        page: &Page,
        mapping: &Mapping,
        inspect: Option<&mut Discovery>,
        index: Option<&Index>,
    ) -> Result<Catalog> {
        self.bytes += page.bytes.len();
        ensure!(self.bytes <= SOURCE_BYTES, "source exceeds byte limit");
        let doc: Value = serde_json::from_slice(&page.bytes).context("invalid JSON document")?;
        let base = page.url.as_ref();
        let index = index.or_else(|| mapping.selection.as_ref()?.index.as_ref());
        let mut local = Catalog::new();
        let branches = if target.root {
            index
                .map(|i| i.branches(&doc, base, self.strict))
                .transpose()?
        } else {
            None
        };
        let mut rows = None;
        let next = if branches.is_some() || inspect.is_some() || mapping.selection.is_some() {
            if branches.is_none() {
                if let Some(inspect) = inspect {
                    inspect.scan(&doc, "", index.is_none())?;
                } else if let Some(rule) = &mapping.selection {
                    rows = Some(rule.parse(
                        &doc,
                        base,
                        target.chain.key.as_deref(),
                        &mut self.matches,
                        &mut local,
                    )?);
                }
            }
            [
                "/next",
                "/next_url",
                "/next_page",
                "/pagination/next",
                "/pagination/next_url",
                "/pagination/next_page",
                "/links/next",
            ]
            .iter()
            .filter_map(|p| doc.pointer(p))
            .find(|v| !v.is_null())
            .map(|v| {
                adapter::parse_url(
                    v.as_str()
                        .or_else(|| v.get("href")?.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .context("next page must be a URL")?,
                    base,
                )
            })
            .transpose()?
        } else {
            adapter::GenericJson.parse_page(&page.bytes, mapping, base, &mut local)?
        };
        let header = if mapping.next_path.is_none() {
            page.link
                .as_deref()
                .map(next_link)
                .transpose()?
                .flatten()
                .map(|link| adapter::parse_url(&link, base))
                .transpose()?
        } else {
            None
        };
        ensure!(
            !matches!((&header,&next),(Some(a),Some(b)) if a != b),
            "conflicting pagination links"
        );
        let next = next.or(header);
        target.chain.advance(&doc, next.as_ref(), rows)?;
        if let Some(branches) = branches {
            ensure!(next.is_none(), "paginated link indexes are unsupported");
            for branch in branches {
                self.pending.push_back(Target {
                    url: Some(branch.url),
                    root: false,
                    chain: Chain {
                        key: Some(branch.key),
                        pages: branch.pages,
                        count: branch.count,
                        ..Chain::default()
                    },
                });
            }
        } else if let Some(url) = next {
            self.pending.push_front(Target {
                url: Some(url),
                root: false,
                chain: target.chain,
            });
        }
        if self.strict
            && self.pending.is_empty()
            && let Some(rule) = &mapping.selection
        {
            self.matches.complete(rule)?;
        }
        Ok(local)
    }
}

async fn read(
    state: &AppState,
    source: &Source,
    url: Option<&Url>,
    conditional: bool,
    retries: u32,
    limits: Option<&http::Limits>,
) -> Result<Page> {
    if let Some(url) = url {
        return remote(state, source.id, url, conditional, retries, limits).await;
    }
    let bytes = source.upload.as_ref().context("missing source data")?;
    ensure!(
        bytes.len() <= PAGE_BYTES,
        "uploaded source exceeds byte limit"
    );
    Ok(Page {
        bytes: bytes.clone(),
        url: None,
        etag: None,
        modified: None,
        link: None,
    })
}
pub(super) async fn discover(
    state: &AppState,
    url: Option<&str>,
    upload: Option<String>,
    index: Option<&Index>,
) -> Result<Value> {
    ensure!(
        url.is_some() != upload.is_some(),
        "supply one manifest URL or uploaded JSON document"
    );
    let source = Source {
        id: 0,
        url: url.map(Url::parse).transpose()?,
        upload: upload.map(String::into_bytes),
        mapping: Mapping::default(),
    };
    let mut walk = Walk::new(source.url.clone(), true);
    let mut discovery = Discovery::default();
    while let Some(target) = walk.take()? {
        let page = read(state, &source, target.url.as_ref(), false, 0, None).await?;
        walk.accept(target, &page, &source.mapping, Some(&mut discovery), index)?;
    }
    Ok(discovery.json())
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
    source.mapping.validate()?;
    let retries = if job.is_some() {
        super::settings(state).await?.retries
    } else {
        0
    };
    let mut walk = Walk::new(source.url.clone(), job.is_none());
    let mut observations = Vec::new();
    while let Some(target) = walk.take()? {
        let page = read(
            state,
            source,
            target.url.as_ref(),
            conditional,
            retries,
            limits,
        )
        .await?;
        let observation = if let Some(job) = job {
            Some(
                save(
                    state,
                    source.id,
                    job,
                    walk.pages - 1,
                    target.url.as_ref(),
                    &page,
                )
                .await?,
            )
        } else {
            None
        };
        let local = walk.accept(target, &page, &source.mapping, None, None)?;
        if let Some(observation) = observation {
            let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
            for ((version, name), item) in &local.assets {
                sqlx::query("INSERT INTO observation_artifacts(observation_id,release,filename,source_url,metadata) VALUES(?,?,?,?,?)").bind(observation.id).bind(version).bind(name).bind(item.url.as_str()).bind(serde_json::to_string(item)?).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            observations.push(observation);
        }
        adapter::merge(catalog, local)?;
    }
    Ok(observations)
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
    let expected: Vec<i64> = serde_json::from_str(ids)?;
    let mut observations = Vec::new();
    let mut catalog = Catalog::new();
    // Replay the same traversal, including the index, using only the admitted
    // observation IDs and the response headers captured alongside their bytes.
    for source in sources {
        source.mapping.validate()?;
        let mut walk = Walk::new(source.url.clone(), false);
        let mut pages = sqlx::query("SELECT o.id,o.digest,o.url,o.request_url,o.next_link,d.body FROM json_each(?) j JOIN source_observations o ON o.id=j.value JOIN raw_documents d USING(digest) WHERE o.job_id=? AND o.source_id=? ORDER BY j.key")
            .bind(ids).bind(job).bind(source.id).fetch(&state.db);
        while let Some(row) = pages.try_next().await? {
            let target = walk.take()?.context("excess snapshot pages")?;
            ensure!(
                target.url.as_ref().map(Url::as_str) == row.get::<Option<&str>, _>("request_url"),
                "snapshot does not match selected page"
            );
            let page = Page {
                url: row
                    .get::<Option<&str>, _>("url")
                    .map(Url::parse)
                    .transpose()?,
                bytes: row.get("body"),
                link: row.get("next_link"),
                etag: None,
                modified: None,
            };
            let local = walk.accept(target, &page, &source.mapping, None, None)?;
            adapter::merge(&mut catalog, local)?;
            observations.push(Observation {
                id: row.get("id"),
                source: source.id,
                digest: row.get("digest"),
            });
        }
        ensure!(walk.pending.is_empty(), "accepted input set is incomplete");
    }
    ensure!(
        observations.iter().map(|o| o.id).eq(expected),
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

#[cfg(test)]
mod selected_tests {
    use super::*;
    use serde_json::json;

    fn page(body: Value, base: Option<Url>) -> Page {
        Page {
            bytes: serde_json::to_vec(&body).unwrap(),
            url: base,
            etag: None,
            modified: None,
            link: None,
        }
    }
    fn mapping() -> Mapping {
        serde_json::from_value(json!({"selection":{
            "index":{"path":"/feed~1dir","link_path":"/where~0to","keys":["edge/one"]},
            "path":"/records","version_path":"/id","groups":[{"path":"/engine~1native","keys":["work~station"]}]
        }})).unwrap()
    }
    fn root(link: &str, count: usize, pages: usize) -> Value {
        json!({"feed/dir":{"edge/one":{"where~to":link,"pages":pages,"count":count},"ignored":{"where~to":"/leave-alone.json"}}})
    }
    fn record(version: &str) -> Value {
        json!({"id":version,"engine/native":{
            "work~station":{"url":"payload/a.bin","size":42},"other":{"url":"payload/other.bin"}
        },"other":{"work~station":{"url":"payload/else.bin"}}})
    }
    fn walk(mapping: &Mapping, docs: Vec<Value>) -> Result<(Vec<Url>, Catalog)> {
        walk_mode(mapping, docs, true)
    }
    fn walk_mode(mapping: &Mapping, docs: Vec<Value>, strict: bool) -> Result<(Vec<Url>, Catalog)> {
        mapping.validate()?;
        let mut walk = Walk::new(Some(Url::parse("https://example.com/index")?), strict);
        let mut visited = Vec::new();
        let mut catalog = Catalog::new();
        for doc in docs {
            let target = walk.take()?.context("fixture has extra pages")?;
            let url = target.url.clone().unwrap();
            visited.push(url.clone());
            adapter::merge(
                &mut catalog,
                walk.accept(target, &page(doc, Some(url)), mapping, None, None)?,
            )?;
        }
        ensure!(walk.take()?.is_none(), "fixture ended early");
        Ok((visited, catalog))
    }

    #[test]
    fn resolves_current_index_and_real_next_links_only() {
        let m = mapping();
        for prefix in ["first", "changed"] {
            let (visited,catalog)=walk(&m,vec![
                root(&format!("{prefix}/current.json"),2,2),
                json!({"page":1,"pages":2,"records":[record("one")],"next_page":"../more?cursor=opaque"}),
                json!({"page":2,"pages":2,"records":[record("two")]})
            ]).unwrap();
            assert_eq!(visited[1].path(), format!("/{prefix}/current.json"));
            assert_eq!(
                visited[2].as_str(),
                "https://example.com/more?cursor=opaque"
            );
            assert_eq!(catalog.assets.len(), 2);
            assert_eq!(
                catalog.assets[&("one".into(), "a.bin".into())].metadata["platform"],
                "work~station"
            );
            assert_eq!(catalog.versions["two"]["channel"], "edge/one");
            assert!(
                !catalog
                    .assets
                    .contains_key(&("one".into(), "else.bin".into()))
            );
        }
    }
    #[test]
    fn missing_selections_and_incomplete_pages_fail_closed() {
        let m = mapping();
        for docs in [
            vec![json!({"feed/dir":{"unselected":{"where~to":"/other.json"}}})],
            vec![
                root("one.json", 1, 2),
                json!({"page":1,"records":[record("one")]}),
            ],
            vec![
                root("one.json", 2, 1),
                json!({"page":1,"records":[record("one")]}),
            ],
            vec![
                root("one.json", 1, 1),
                json!({"records":[{"id":"one","another":{"work~station":{"url":"/oops"}}}]}),
            ],
            vec![
                root("one.json", 1, 1),
                json!({"records":[{"id":"one","engine/native":{"other":{"url":"/oops"}}}]}),
            ],
            vec![
                root("one.json", 1, 1),
                json!({"records":[record("one")],"has_more":true}),
            ],
            vec![root("file:///etc/hosts", 1, 1)],
        ] {
            assert!(walk(&m, docs).is_err());
        }
    }

    #[test]
    fn admitted_selection_survives_omitted_keys_and_empty_inventories() {
        let m=serde_json::from_value(json!({"selection":{
            "index":{"path":"/index","link_path":"/first","keys":["a","b"]},
            "path":"/entries","version_path":"/id","groups":[{"path":"/component","keys":["selected"]}]
        }})).unwrap();
        let index =
            json!({"index":{"a":{"first":"a"},"b":{"first":"b"},"new":{"first":"outside"}}});
        let other = json!({"entries":[{"id":"v1","component":{"other":{"url":"unselected"}}}]});
        let valid = json!({"entries":[{"id":"v2","component":{"selected":{"url":"approved"}}}]});
        // Admission verifies the requested variants across the inventory, even
        // when a particular branch has never carried the selected variant.
        let (_, preview) = walk(&m, vec![index.clone(), other.clone(), valid.clone()]).unwrap();
        assert_eq!(preview.assets.len(), 1);
        let reduced = json!({"index":{"b":{"first":"changed"},"new":{"first":"outside"}}});
        assert!(walk(&m, vec![reduced.clone(), valid.clone()]).is_err());
        let (visited, surviving) =
            walk_mode(&m, vec![reduced.clone(), valid.clone()], false).unwrap();
        assert_eq!(visited.len(), 2);
        assert_eq!(visited[1].path(), "/changed");
        assert!(
            surviving
                .assets
                .contains_key(&("v2".into(), "approved".into()))
        );
        for (root, docs) in [
            (json!({"index":{}}), vec![]),
            (reduced.clone(), vec![other.clone()]),
            (reduced.clone(), vec![json!({"entries":[]})]),
        ] {
            let (_, out) =
                walk_mode(&m, std::iter::once(root).chain(docs).collect(), false).unwrap();
            assert!(out.assets.is_empty());
        }
        assert!(walk_mode(&m, vec![json!({"index":[]})], false).is_err());
        assert!(walk_mode(&m, vec![json!({"index":{"b":{}}})], false).is_err());
        assert!(walk_mode(&m, vec![reduced, json!({"different":[]})], false).is_err());
        let (_, restored) = walk_mode(&m, vec![index, valid.clone(), other], false).unwrap();
        assert!(
            restored
                .assets
                .contains_key(&("v2".into(), "approved".into()))
        );
    }
    #[test]
    fn direct_discovery_and_scope_skip_unselected_branches() {
        let data = json!({"nested":{"items":[
            {"version":"kept","channel":"release","component":{"p/x":{"url":"/p/x"},"extra":{"url":"/p/y"}}},
            {"version":"ignored","channel":"other","component":{"p/x":{"url":"/bad"}}}
        ]},"branches":{"blue":{"directory":"/list.json"},"red":{"directory":"/other.json"}}});
        let mut d = Discovery::default();
        d.scan(&data, "", true).unwrap();
        let found = d.json();
        assert!(
            found["indexes"].as_array().unwrap().contains(
                &json!({"path":"/branches","link_path":"/directory","keys":["blue","red"]})
            )
        );
        assert!(found["collections"].as_array().unwrap().contains(&json!({"path":"/nested/items","version_path":"/version","groups":[{"path":"/component","keys":["extra","p/x"]}],"channels":{"path":"/channel","keys":["other","release"]}})));
        let m=serde_json::from_value(json!({"selection":{"path":"/nested/items","version_path":"/version","groups":[{"path":"/component","keys":["p/x"]}],"channels":{"path":"/channel","keys":["release"]}}})).unwrap();
        let (_, out) = walk(&m, vec![data.clone()]).unwrap();
        assert_eq!(out.assets.len(), 1);
        assert!(out.assets.contains_key(&("kept".into(), "x".into())));
        assert!(adapter::rewrite(&serde_json::to_vec(&data).unwrap(), &m, None, &out).is_none());
        let mut stale = data;
        stale["nested"]["items"][0]["channel"] = json!("other");
        assert!(walk(&m, vec![stale]).is_err());
    }
    #[test]
    fn malformed_selected_file_isolates_the_identified_release() {
        let mut bad = record("bad");
        bad["engine/native"]["work~station"]["sha256"] = json!("broken");
        let (_, out) = walk(
            &mapping(),
            vec![
                root("one.json", 3, 1),
                json!({"records":[record("good"),record("bad"),bad]}),
            ],
        )
        .unwrap();
        assert_eq!(out.assets.len(), 1);
        assert!(out.assets.contains_key(&("good".into(), "a.bin".into())));
        assert!(!out.versions.contains_key("bad"));
        assert!(out.release_errors["bad"].contains("digest"));
    }
    #[tokio::test]
    async fn recovery_replays_saved_link_and_selected_page_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::open(tmp.path(), "https://mirror.example")
            .await
            .unwrap();
        sqlx::raw_sql("INSERT INTO projects(name,slug) VALUES('p','p'); INSERT INTO runtimes(project_id,name,slug) VALUES(1,'r','r'); INSERT INTO jobs(runtime_id) VALUES(1); INSERT INTO sources(runtime_id,kind,url) VALUES(1,'url','https://example.com/root')").execute(&state.db).await.unwrap();
        let source = Source {
            id: 1,
            url: Some(Url::parse("https://example.com/root").unwrap()),
            upload: None,
            mapping: mapping(),
        };
        let old = Url::parse("https://example.com/old.json").unwrap();
        let first = save(
            &state,
            1,
            1,
            0,
            source.url.as_ref(),
            &page(root("old.json", 1, 1), source.url.clone()),
        )
        .await
        .unwrap()
        .id;
        let second = save(
            &state,
            1,
            1,
            1,
            Some(&old),
            &page(json!({"records":[record("accepted")]}), Some(old.clone())),
        )
        .await
        .unwrap()
        .id;
        let sources = [source];
        let (catalog, obs) = restore(&state, 1, &json!([first, second]).to_string(), &sources)
            .await
            .unwrap();
        assert_eq!(obs.len(), 2);
        assert!(
            catalog
                .assets
                .contains_key(&("accepted".into(), "a.bin".into()))
        );
        assert!(
            restore(&state, 1, &json!([first]).to_string(), &sources)
                .await
                .is_err()
        );
        assert!(
            restore(&state, 1, &json!([second, first]).to_string(), &sources)
                .await
                .is_err()
        );
    }
}
