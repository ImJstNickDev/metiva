use super::*;
use crate::worker::nested::{self, Document, Documents, Nested, SelectionCounts};
use std::collections::HashMap;

fn document(page: Page, observation: Option<i64>) -> Document {
    let mut document = Document::new(page.bytes, page.url, observation);
    document.link = page.link;
    document
}

struct Preview<'a> {
    state: &'a AppState,
    source: &'a Source,
}
impl Documents for Preview<'_> {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        match read(self.state, self.source, url, false, 0, None).await {
            Ok(page) => Ok(Ok(document(page, None))),
            Err(error) if error.is::<sqlx::Error>() || error.is::<InvalidCapture>() => Err(error),
            Err(error) => Ok(Err(format!("{error:#}"))),
        }
    }
}

pub(super) async fn preview(
    state: &AppState,
    source: &Source,
    rule: &Nested,
) -> Result<(Catalog, SelectionCounts)> {
    let mut docs = Preview { state, source };
    let (catalog, _, counts) = nested::execute(rule, source.url.as_ref(), &mut docs, true).await?;
    Ok((catalog, counts))
}

struct Fetch<'a> {
    conditional: bool,
    retries: u32,
    limits: Option<&'a http::Limits>,
}

// Keep response bodies in the original observation store. After the walker's
// bounded body cache evicts a document, either URL still identifies its bytes.
enum Outcome {
    Observed(usize),
    Failed(String),
}

const MAX_REQUESTS: usize = 100_000;
const MAX_CAPTURE_BYTES: u64 = 4 << 30;

#[derive(Default)]
struct Budget {
    requests: usize,
    bytes: u64,
}
impl Budget {
    fn request(&mut self) -> Result<()> {
        ensure!(
            self.requests < MAX_REQUESTS,
            "nested history exceeds request limit"
        );
        self.requests += 1;
        Ok(())
    }

    fn body(&mut self, size: usize) -> Result<()> {
        self.bytes += size as u64;
        ensure!(
            self.bytes <= MAX_CAPTURE_BYTES,
            "nested history exceeds total byte limit"
        );
        Ok(())
    }
}

struct Capture<'a> {
    state: &'a AppState,
    job: i64,
    source: &'a Source,
    fetch: Option<Fetch<'a>>,
    pending: VecDeque<i64>,
    budget: Budget,
    observations: Vec<Observation>,
    seen: HashMap<Url, Outcome>,
    failures: HashMap<String, String>,
}

impl<'a> Capture<'a> {
    fn new(
        state: &'a AppState,
        source: &'a Source,
        job: i64,
        pending: VecDeque<i64>,
        fetch: Option<Fetch<'a>>,
    ) -> Self {
        Self {
            state,
            source,
            job,
            pending,
            fetch,
            budget: Budget::default(),
            observations: Vec::new(),
            seen: HashMap::new(),
            failures: HashMap::new(),
        }
    }

    async fn fetch_page(
        &mut self,
        url: Option<&Url>,
    ) -> Result<Result<(Observation, Page), String>> {
        let fetch = self
            .fetch
            .as_ref()
            .context("missing frozen nested document")?;
        let page = match read(
            self.state,
            self.source,
            url,
            fetch.conditional,
            fetch.retries,
            fetch.limits,
        )
        .await
        {
            Ok(page) => page,
            Err(error) if error.is::<sqlx::Error>() || error.is::<InvalidCapture>() => {
                return Err(error);
            }
            Err(error) => {
                let message = format!("{error:#}");
                if let (Some(root), Some(url)) = (self.observations.first(), url) {
                    sqlx::query(
                        "INSERT INTO nested_fetch_failures(root_observation,request_url,error) VALUES(?,?,?)",
                    )
                    .bind(root.id)
                    .bind(url.as_str())
                    .bind(&message)
                    .execute(&self.state.db)
                    .await?;
                    self.seen
                        .insert(url.clone(), Outcome::Failed(message.clone()));
                }
                return Ok(Err(message));
            }
        };
        self.budget.body(page.bytes.len())?;
        let obs = save(
            self.state,
            self.source.id,
            self.job,
            self.observations.len(),
            url,
            &page,
        )
        .await?;
        Ok(Ok((obs, page)))
    }

    fn document(&self, page: Page, index: usize) -> Document {
        let authoritative = page.url.as_ref().is_some_and(
            |base| matches!(self.seen.get(base), Some(Outcome::Observed(prior)) if *prior == index),
        );
        let id = self.fetch.as_ref().map(|_| self.observations[index].id);
        let mut doc = document(page, id);
        doc.cache_effective = authoritative;
        doc
    }
}

impl Documents for Capture<'_> {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        if let Some(url) = url {
            match self.seen.get(url) {
                Some(Outcome::Observed(index)) => {
                    let id = self.observations[*index].id;
                    let (_, _, page) =
                        captured_page(self.state, self.source.id, self.job, id).await?;
                    return Ok(Ok(self.document(page, *index)));
                }
                Some(Outcome::Failed(error)) => return Ok(Err(error.clone())),
                None => (),
            }
        }
        self.budget.request()?;
        if let Some(url) = url
            && let Some(error) = self.failures.remove(url.as_str())
        {
            self.seen
                .insert(url.clone(), Outcome::Failed(error.clone()));
            return Ok(Err(error));
        }

        let (obs, page) = if let Some(id) = self.pending.pop_front() {
            let (obs, request, page) =
                captured_page(self.state, self.source.id, self.job, id).await?;
            ensure!(
                request.as_deref() == url.map(Url::as_str),
                "snapshot does not match selected document"
            );
            self.budget.body(page.bytes.len())?;
            (obs, page)
        } else {
            match self.fetch_page(url).await? {
                Ok(result) => result,
                Err(error) => return Ok(Err(error)),
            }
        };

        let id = obs.id;
        let index = self.observations.len();
        self.observations.push(obs);
        if index == 0 {
            self.failures = sqlx::query_as(
                "SELECT request_url,error FROM nested_fetch_failures WHERE root_observation=?",
            )
            .bind(id)
            .fetch_all(&self.state.db)
            .await?
            .into_iter()
            .collect();
        }
        if let Some(base) = &page.url {
            if let Some(Outcome::Observed(prior)) = self.seen.get(base)
                && self.observations[*prior].digest != self.observations[index].digest
            {
                let message = "document changed between redirect aliases".to_string();
                if let Some(url) = url {
                    self.seen
                        .insert(url.clone(), Outcome::Failed(message.clone()));
                }
                return Ok(Err(message));
            }
            self.seen
                .entry(base.clone())
                .or_insert(Outcome::Observed(index));
        }
        if let Some(url) = url {
            self.seen.insert(url.clone(), Outcome::Observed(index));
        }
        Ok(Ok(self.document(page, index)))
    }
}

pub(super) async fn collect(
    state: &AppState,
    source: &Source,
    rule: &Nested,
    job: i64,
    conditional: bool,
    retries: u32,
    limits: Option<&http::Limits>,
) -> Result<(Catalog, Vec<Observation>)> {
    let pending = captured_prefix(state, source.id, job).await?;
    let mut docs = Capture::new(
        state,
        source,
        job,
        pending,
        Some(Fetch {
            conditional,
            retries,
            limits,
        }),
    );
    let (local, mut inventory, _) =
        nested::execute(rule, source.url.as_ref(), &mut docs, false).await?;
    ensure!(docs.pending.is_empty(), "excess captured nested documents");
    for obs in &docs.observations {
        let files = inventory.remove(&obs.id).unwrap_or_default();
        crate::worker::inventory::record(&state.db, obs.id, &files).await?;
    }
    Ok((local, docs.observations))
}

pub(super) async fn restore(
    state: &AppState,
    source: &Source,
    rule: &Nested,
    job: i64,
    ids: &str,
    catalog: &mut Catalog,
) -> Result<Vec<Observation>> {
    let accepted: Vec<i64> = sqlx::query_scalar("SELECT o.id FROM json_each(?) j JOIN source_observations o ON o.id=j.value WHERE o.job_id=? AND o.source_id=? ORDER BY j.key")
        .bind(ids).bind(job).bind(source.id).fetch_all(&state.db).await?;
    let mut docs = Capture::new(state, source, job, accepted.into(), None);
    let (local, _, _) = nested::execute(rule, source.url.as_ref(), &mut docs, false).await?;
    ensure!(docs.pending.is_empty(), "excess frozen nested documents");
    adapter::merge(catalog, local)?;
    Ok(docs.observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn capture_budget_includes_its_boundaries() {
        let mut budget = Budget {
            requests: MAX_REQUESTS - 1,
            bytes: MAX_CAPTURE_BYTES - 1,
        };
        assert!(budget.request().is_ok());
        assert!(budget.request().is_err());
        assert!(budget.body(1).is_ok());
        assert!(budget.body(1).is_err());
    }

    #[tokio::test]
    async fn interrupted_history_restores_original_sources_and_aliases_after_cache_eviction()
    -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let state = crate::open(tmp.path(), "https://mirror.example").await?;
        sqlx::raw_sql(
            "INSERT INTO projects(name,slug) VALUES('p','p');
            INSERT INTO runtimes(project_id,name,slug) VALUES(1,'r','r');
            INSERT INTO jobs(runtime_id) VALUES(1);
            INSERT INTO sources(runtime_id,kind,upload) VALUES(1,'upload',X'00'),(1,'upload',X'00');
            INSERT INTO origin_cooldowns(host,retry_after) VALUES('8.8.8.8',unixepoch()+86400);
            UPDATE settings SET retries=0",
        )
        .execute(&state.db)
        .await?;
        let base = Url::parse("https://8.8.8.8/metadata/")?;
        let page = |body: Value, url: Option<Url>| Page {
            bytes: serde_json::to_vec(&body).unwrap(),
            url,
            etag: Some(String::from("\"original\"")),
            modified: None,
            link: None,
        };
        let generic = page(
            json!({"releases":[{"version":"plain","files":[{"url":"https://files.example/plain.bin"}]}]}),
            None,
        );
        let plain = save(&state, 1, 1, 0, None, &generic).await?;
        let mut rows = Vec::new();
        let mut children = Vec::new();
        let mut add =
            |name: &str, request: &str, effective: &str, filename: &str, link: Option<&str>| {
                let request = base.join(request).unwrap();
                rows.push(json!({"id":name,"url":request}));
                let mut response = page(
                    json!({"file":{"url":filename}}),
                    Some(base.join(effective).unwrap()),
                );
                response.link = link.map(str::to_owned);
                children.push((request, response));
            };
        add("original", "first", "canonical", "original.bin", None);
        for i in 0..140 {
            let name = format!("doc-{i}");
            add(&name, &name, &name, "payload.bin", None);
        }
        add(
            "changed-headers",
            "second",
            "canonical",
            "original.bin",
            Some("<next>; rel=next"),
        );
        add("changed-bytes", "third", "canonical", "changed.bin", None);
        rows.insert(0, json!({"id":"failed-first","url":base.join("failed")?}));
        for (version, url) in [
            ("via-effective", "canonical"),
            ("via-request", "first"),
            ("failed-again", "failed"),
            ("new-request", "unvisited"),
        ] {
            rows.push(json!({"id":version, "url":base.join(url)?}));
        }
        let root = save(&state, 2, 1, 0, None, &page(json!({"rows":rows}), None)).await?;
        sqlx::query("INSERT INTO nested_fetch_failures VALUES(?,?,?)")
            .bind(root.id)
            .bind(base.join("failed")?.as_str())
            .bind("original upstream failure")
            .execute(&state.db)
            .await?;
        let mut ids = vec![plain.id, root.id];
        for (i, (request, response)) in children.iter().enumerate() {
            ids.push(save(&state, 2, 1, i + 1, Some(request), response).await?.id);
        }
        let sources = [
            Source {
                id: 1,
                url: None,
                upload: Some(b"changed".to_vec()),
                mapping: Mapping::default(),
            },
            Source {
                id: 2,
                url: None,
                upload: Some(b"changed".to_vec()),
                mapping: serde_json::from_value(
                    json!({"nested":{"path":"/rows","version_path":"/id",
                    "rules":[{"kind":"document","path":"","rules":[{"kind":"file","path":"/file"}]}]}}),
                )?,
            },
        ];
        // The captured prefix ends before the final child. Its first request
        // reaches the stored cooldown, so this test never contacts a network origin.
        let (catalog, observations) =
            observe(&state, &sources, 1, true, &http::Limits::new(1, 1)).await?;
        assert!(observations.iter().map(|o| o.id).eq(ids.iter().copied()));
        assert_eq!(catalog.assets.len(), 144);
        for version in ["original", "via-effective", "via-request"] {
            assert_eq!(
                catalog.assets[&(version.into(), "original.bin".into())].url,
                base.join("original.bin")?
            );
        }
        assert!(catalog.release_errors["changed-headers"].contains("HTTP next link"));
        assert!(catalog.release_errors["changed-bytes"].contains("redirect aliases"));
        assert!(catalog.release_errors["new-request"].contains("Retry-After"));
        assert_eq!(
            catalog.release_errors["failed-first"],
            catalog.release_errors["failed-again"]
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM nested_fetch_failures")
                .fetch_one(&state.db)
                .await?,
            2
        );
        let attributed: Vec<String> = sqlx::query_scalar(
            "SELECT release FROM observation_artifacts WHERE observation_id=? ORDER BY release",
        )
        .bind(ids[2])
        .fetch_all(&state.db)
        .await?;
        assert_eq!(attributed, ["original", "via-effective", "via-request"]);
        let (frozen, replayed) =
            super::super::restore(&state, 1, &serde_json::to_string(&ids)?, &sources).await?;
        assert!(frozen == catalog);
        assert!(replayed.iter().map(|o| o.id).eq(ids.iter().copied()));
        sqlx::query("UPDATE raw_documents SET body=X'00' WHERE digest=(SELECT digest FROM source_observations WHERE id=?)")
            .bind(ids[2]).execute(&state.db).await?;
        let corrupt = super::super::restore(&state, 1, &serde_json::to_string(&ids)?, &sources)
            .await
            .err()
            .unwrap();
        assert!(corrupt.is::<InvalidCapture>());
        Ok(())
    }

    #[tokio::test]
    async fn accepted_graph_replays_failed_and_shared_edges_without_contacting_the_origin() {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::open(tmp.path(), "https://mirror.example")
            .await
            .unwrap();
        sqlx::raw_sql("INSERT INTO projects(name,slug) VALUES('p','p'); INSERT INTO runtimes(project_id,name,slug) VALUES(1,'r','r'); INSERT INTO jobs(runtime_id) VALUES(1); INSERT INTO sources(runtime_id,kind,url) VALUES(1,'url','https://example.org/root')").execute(&state.db).await.unwrap();
        let source = Source {
            id: 1,
            url: Some(Url::parse("https://example.org/root").unwrap()),
            upload: None,
            mapping: serde_json::from_value(json!({"nested":{"path":"","rules":[
                {"kind":"document","path":"","rules":[{"kind":"file","path":"/direct"}]}]}}))
            .unwrap(),
        };
        let mut ids = Vec::new();
        for (i, (request, effective, body)) in [
            (
                "root",
                "root",
                json!({"a":{"url":"shared"},"b":{"url":"missing"},"c":{"url":"redirected/shared"}}),
            ),
            (
                "shared",
                "redirected/shared",
                json!({"direct":{"url":"payload.json"}}),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let base = source.url.as_ref().unwrap();
            let page = Page {
                bytes: serde_json::to_vec(&body).unwrap(),
                url: Some(base.join(effective).unwrap()),
                etag: None,
                modified: None,
                link: None,
            };
            ids.push(
                save(&state, 1, 1, i, Some(&base.join(request).unwrap()), &page)
                    .await
                    .unwrap()
                    .id,
            );
        }
        sqlx::query("INSERT INTO nested_fetch_failures(root_observation,request_url,error) VALUES(?, 'https://example.org/missing','manifest server returned HTTP 503')")
            .bind(ids[0]).execute(&state.db).await.unwrap();
        let sources = [source];
        let (out, obs) = crate::worker::feed::restore(&state, 1, &json!(ids).to_string(), &sources)
            .await
            .unwrap();
        assert_eq!(obs.len(), 2);
        assert_eq!(out.release_errors.len(), 1);
        assert!(out.release_errors["b"].contains("503"));
        assert_eq!(out.assets.len(), 2);
        assert_eq!(
            out.assets[&("c".into(), "payload.json".into())].url.path(),
            "/redirected/payload.json"
        );
        for bad in [
            json!([ids[0]]),
            json!([ids[1], ids[0]]),
            json!([ids[0], ids[1], ids[1]]),
        ] {
            assert!(
                crate::worker::feed::restore(&state, 1, &bad.to_string(), &sources)
                    .await
                    .is_err()
            );
        }
        sqlx::query("UPDATE source_observations SET next_link='<next.json>; rel=next' WHERE id=?")
            .bind(ids[0])
            .execute(&state.db)
            .await
            .unwrap();
        let error = crate::worker::feed::restore(&state, 1, &json!(ids).to_string(), &sources)
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("HTTP next link"));
        sqlx::query("UPDATE source_observations SET next_link=NULL WHERE id=?")
            .bind(ids[0])
            .execute(&state.db)
            .await
            .unwrap();
        sqlx::query("UPDATE source_observations SET next_link='<next.json>; rel=next' WHERE id=?")
            .bind(ids[1])
            .execute(&state.db)
            .await
            .unwrap();
        let (out, _) = crate::worker::feed::restore(&state, 1, &json!(ids).to_string(), &sources)
            .await
            .unwrap();
        assert!(out.assets.is_empty());
        for version in ["a", "c"] {
            assert!(out.release_errors[version].contains("HTTP next link"));
        }
        sqlx::query("DELETE FROM nested_fetch_failures")
            .execute(&state.db)
            .await
            .unwrap();
        assert!(
            crate::worker::feed::restore(&state, 1, &json!(ids).to_string(), &sources)
                .await
                .is_err()
        );
    }
}
