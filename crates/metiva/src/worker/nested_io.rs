use super::*;
use crate::worker::nested::{self, Document, Documents, Nested};

struct Live<'a> {
    state: &'a AppState,
    source: &'a Source,
    job: Option<i64>,
    conditional: bool,
    retries: u32,
    limits: Option<&'a http::Limits>,
    observations: Vec<Observation>,
}

impl Documents for Live<'_> {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        let page = match read(
            self.state,
            self.source,
            url,
            self.conditional,
            self.retries,
            self.limits,
        )
        .await
        {
            Ok(page) => page,
            Err(error) => {
                if error.is::<sqlx::Error>() {
                    return Err(error);
                }
                let message = format!("{error:#}");
                if let (Some(root), Some(url)) = (self.observations.first(), url) {
                    sqlx::query("INSERT INTO nested_fetch_failures(root_observation,request_url,error) VALUES(?,?,?)")
                        .bind(root.id).bind(url.as_str()).bind(&message).execute(&self.state.db).await?;
                }
                return Ok(Err(message));
            }
        };
        let observation = if let Some(job) = self.job {
            let obs = save(
                self.state,
                self.source.id,
                job,
                self.observations.len(),
                url,
                &page,
            )
            .await?;
            let id = obs.id;
            self.observations.push(obs);
            Some(id)
        } else {
            None
        };
        let mut document = Document::new(page.bytes, page.url, observation);
        document.link = page.link;
        Ok(Ok(document))
    }
}

pub(super) async fn collect(
    state: &AppState,
    source: &Source,
    rule: &Nested,
    job: Option<i64>,
    conditional: bool,
    retries: u32,
    limits: Option<&http::Limits>,
) -> Result<(Catalog, Vec<Observation>)> {
    let mut docs = Live {
        state,
        source,
        job,
        conditional,
        retries,
        limits,
        observations: Vec::new(),
    };
    let (local, mut inventory) =
        nested::execute(rule, source.url.as_ref(), &mut docs, job.is_none()).await?;
    for obs in &docs.observations {
        let files = inventory.remove(&obs.id).unwrap_or_default();
        crate::worker::inventory::record(&state.db, obs.id, &files).await?;
    }
    Ok((local, docs.observations))
}

struct Replay<'a> {
    state: &'a AppState,
    job: i64,
    source: i64,
    pending: VecDeque<i64>,
    observations: Vec<Observation>,
}
impl Documents for Replay<'_> {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        if let (Some(root),Some(url))=(self.observations.first(),url)
            && let Some(failure)=sqlx::query_scalar::<_,String>("SELECT error FROM nested_fetch_failures WHERE root_observation=? AND request_url=?")
                .bind(root.id).bind(url.as_str()).fetch_optional(&self.state.db).await? {
            return Ok(Err(failure));
        }
        let id = self
            .pending
            .pop_front()
            .context("missing frozen nested document")?;
        let row=sqlx::query("SELECT o.digest,o.request_url,o.url,o.next_link,d.body FROM source_observations o JOIN raw_documents d USING(digest) WHERE o.id=? AND o.job_id=? AND source_id=?")
            .bind(id).bind(self.job).bind(self.source).fetch_one(&self.state.db).await?;
        ensure!(
            row.get::<Option<&str>, _>("request_url") == url.map(Url::as_str),
            "snapshot does not match selected document"
        );
        let bytes: Vec<u8> = row.get("body");
        let digest: String = row.get("digest");
        ensure!(
            bytes.len() <= PAGE_BYTES && hex::encode(Sha256::digest(&bytes)) == digest,
            "invalid frozen nested document"
        );
        let base = row
            .get::<Option<&str>, _>("url")
            .map(Url::parse)
            .transpose()?;
        self.observations.push(Observation {
            id,
            digest,
            source: self.source,
        });
        let mut document = Document::new(bytes, base, None);
        document.link = row.get("next_link");
        Ok(Ok(document))
    }
}
pub(super) async fn restore(
    state: &AppState,
    source: &Source,
    rule: &Nested,
    job: i64,
    ids: &str,
    catalog: &mut Catalog,
) -> Result<Vec<Observation>> {
    let accepted:Vec<i64>=sqlx::query_scalar("SELECT o.id FROM json_each(?) j JOIN source_observations o ON o.id=j.value WHERE o.job_id=? AND o.source_id=? ORDER BY j.key")
        .bind(ids).bind(job).bind(source.id).fetch_all(&state.db).await?;
    let mut replay = Replay {
        state,
        job,
        source: source.id,
        pending: accepted.into(),
        observations: Vec::new(),
    };
    let (local, _) = nested::execute(rule, source.url.as_ref(), &mut replay, false).await?;
    ensure!(replay.pending.is_empty(), "excess frozen nested documents");
    adapter::merge(catalog, local)?;
    Ok(replay.observations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
