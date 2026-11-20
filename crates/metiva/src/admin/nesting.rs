use super::*;
use anyhow::{Context, ensure};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

const SAMPLE_LIMIT: usize = 64;
const OPTION_LIMIT: usize = 256;
const VALUE_LIMIT: usize = 64;
const PATH_LIMIT: usize = 2048;
const URL_FIELDS: &[&str] = &[
    "browser_download_url",
    "download_url",
    "url",
    "href",
    "link",
    "manifest_url",
    "first_page",
    "uri",
    "src",
    "ref",
    "document_url",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Inspect {
    url: Option<String>,
    upload: Option<String>,
    #[serde(default)]
    trail: Vec<String>,
    row: Option<String>,
    collection: Option<String>,
    #[serde(default)]
    filters: Vec<Filter>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    path: String,
    values: Vec<String>,
}
#[derive(Serialize)]
struct Sample {
    id: String,
    pointer: String,
    fields: BTreeMap<String, String>,
}
#[derive(Serialize)]
struct Collection {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version_path: Option<String>,
    count: usize,
    fields: Vec<Value>,
    samples: Vec<Sample>,
}
fn child(base: &str, key: &str) -> String {
    format!("{base}/{}", key.replace('~', "~0").replace('/', "~1"))
}
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() && s.len() <= 255 && !s.chars().any(char::is_control) => {
            Some(s.clone())
        }
        Value::Bool(_) | Value::Number(_) => Some(v.to_string()),
        _ => None,
    }
}
fn entries(v: &Value) -> Box<dyn Iterator<Item = (String, &Value)> + '_> {
    match v {
        Value::Array(a) => Box::new(a.iter().enumerate().map(|(i, r)| (i.to_string(), r))),
        Value::Object(o) => Box::new(o.iter().map(|(k, r)| (k.clone(), r))),
        _ => Box::new(std::iter::empty()),
    }
}
fn url_field(v: &Value) -> Option<&str> {
    if v.as_str().is_some_and(|s| {
        ["https://", "http://", "/", "./", "../"]
            .iter()
            .any(|p| s.starts_with(p))
    }) {
        return Some("");
    }
    URL_FIELDS
        .iter()
        .copied()
        .find(|k| v.get(*k).is_some_and(Value::is_string))
}
fn digest_field(v: &Value) -> Option<(&str, &str)> {
    for key in [
        "sha256", "sha1", "sha512", "sha384", "md5", "hash", "digest",
    ] {
        if let Some(s) = v.get(key).and_then(Value::as_str) {
            let alg = match s.len() {
                32 => "md5",
                40 => "sha1",
                64 => "sha256",
                96 => "sha384",
                128 => "sha512",
                _ => continue,
            };
            if s.bytes().all(|b| b.is_ascii_hexdigit())
                && (alg == key || matches!(key, "hash" | "digest"))
            {
                return Some((key, alg));
            }
        }
    }
    None
}
fn collection_identity(rows: &Value) -> Option<Option<String>> {
    let mut sample = entries(rows).take(12).peekable();
    sample.peek()?;
    let candidates: Vec<_> = sample.map(|(_, r)| r).collect();
    if !candidates.iter().all(|r| r.is_object()) {
        return None;
    }
    if let Some(k) = ["version", "tag_name", "release", "build", "id", "name"]
        .into_iter()
        .find(|k| {
            candidates
                .iter()
                .all(|r| r.get(*k).and_then(scalar).is_some())
        })
    {
        return Some(Some(child("", k)));
    }
    (rows.is_object()
        && candidates.iter().all(|r| {
            url_field(r).is_some()
                || r.as_object()
                    .is_some_and(|o| o.values().any(|v| v.is_object() || v.is_array()))
        }))
    .then_some(None)
}
fn filter_value(row: &Value, key: &str, path: &str) -> Option<String> {
    if path == "@key" {
        Some(key.to_owned())
    } else {
        row.pointer(path).and_then(scalar)
    }
}
fn sample(path: &str, key: &str, row: &Value, id_path: Option<&str>) -> Option<Sample> {
    let id = filter_value(row, key, id_path.unwrap_or("@key"))?;
    let fields = row
        .as_object()?
        .iter()
        .filter(|(k, _)| !URL_FIELDS.contains(&k.as_str()))
        .filter_map(|(k, v)| Some((child("", k), scalar(v)?)))
        .take(24)
        .collect();
    Some(Sample {
        id,
        pointer: child(path, key),
        fields,
    })
}
fn describe_collection(path: &str, rows: &Value, id_path: Option<String>) -> Collection {
    let mut values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut representatives = BTreeMap::new();
    let mut first = Vec::new();
    let mut count = 0;
    for (k, row) in entries(rows) {
        count += 1;
        let Some(s) = sample(path, &k, row, id_path.as_deref()) else {
            continue;
        };
        for (p, val) in &s.fields {
            if Some(p) == id_path.as_ref() || values.len() >= 24 && !values.contains_key(p) {
                continue;
            }
            let set = values.entry(p.clone()).or_default();
            if set.len() <= VALUE_LIMIT && set.insert(val.clone()) {
                representatives.insert((p.clone(), val.clone()), k.clone());
            }
        }
        if first.len() < SAMPLE_LIMIT {
            first.push(k)
        }
    }
    values.retain(|_, vals| vals.len() <= VALUE_LIMIT);
    let mut seen = BTreeSet::new();
    let samples = first
        .iter()
        .take(1)
        .chain(
            representatives
                .iter()
                .filter(|((p, _), _)| values.contains_key(p))
                .map(|(_, k)| k),
        )
        .chain(&first)
        .filter(|k| seen.insert((*k).clone()))
        .take(SAMPLE_LIMIT)
        .filter_map(|k| {
            sample(
                path,
                k,
                rows.get(k).or_else(|| rows.get(k.parse::<usize>().ok()?))?,
                id_path.as_deref(),
            )
        })
        .collect();
    Collection {
        path: path.to_owned(),
        version_path: id_path,
        count,
        fields: values
            .into_iter()
            .map(|(path, values)| json!({"path":path,"values":values}))
            .collect(),
        samples,
    }
}
fn collections(v: &Value, path: &str, depth: usize, out: &mut Vec<Collection>) {
    if out.len() >= 32 || depth > 8 || path.len() > PATH_LIMIT {
        return;
    }
    let identity = collection_identity(v);
    let keyed = matches!(identity, Some(None));
    if let Some(id) = identity {
        out.push(describe_collection(path, v, id));
        if !keyed {
            return;
        }
    }
    if let Some(o) = v.as_object() {
        // A single object wrapper can itself look like a map keyed by release.
        // Inspect each wrapper to retain the more specific collection choice.
        let keys = if keyed && o.len() > 4 { 0 } else { 128 };
        for (k, val) in o
            .iter()
            .take(keys)
            .filter(|(_, v)| v.is_array() || v.is_object())
        {
            collections(val, &child(path, k), depth + 1, out);
        }
    }
}

struct Branches {
    options: BTreeMap<(String, String, String), Value>,
    remaining: usize,
    truncated: bool,
}
impl Branches {
    fn insert(&mut self, rule: &str, kind: &str, field: &str, option: Value) {
        if self.options.len() < OPTION_LIMIT {
            self.options
                .entry((rule.into(), kind.into(), field.into()))
                .or_insert(option);
        } else {
            self.truncated = true;
        }
    }

    fn walk(&mut self, v: &Value, rule: &str, ptr: &str, depth: usize, map_keys: bool) {
        if depth > 12
            || rule.len() > PATH_LIMIT
            || ptr.len() > PATH_LIMIT
            || self.remaining == 0
            || self.options.len() >= OPTION_LIMIT
        {
            self.truncated = true;
            return;
        }
        self.remaining -= 1;
        let url = url_field(v);
        let hash = digest_field(v);
        if url.is_some() || hash.is_some() {
            let (field, kind) = match url {
                Some(u) => (u, "url"),
                None => (hash.unwrap().0, "digest"),
            };
            let relative = if field.is_empty() {
                String::new()
            } else {
                child("", field)
            };
            let leaf = v.pointer(&relative).and_then(Value::as_str).unwrap_or("");
            let mut b = json!({"kind":kind,"path":rule,"sample_pointer":format!("{ptr}{relative}"),"sample":leaf.chars().take(300).collect::<String>()});
            if url.is_some() {
                b["url_path"] = json!(relative)
            }
            if let Some((k, alg)) = hash {
                b["digest_path"] = json!(child("", k));
                b["algorithm"] = json!(alg)
            }
            if v.get("size").is_some_and(Value::is_u64) {
                b["size_path"] = json!("/size")
            }
            if let Some(k) = ["filename", "path", "name"]
                .into_iter()
                .find(|k| v.get(*k).is_some_and(Value::is_string))
            {
                b["name_path"] = json!(child("", k))
            } else if map_keys && url.is_none() {
                b["name_path"] = json!("@key")
            }
            self.insert(rule, kind, &relative, b);
            // Secondary links can address a different resource, such as the API
            // representation accompanying a download. Anchor their rules at the
            // string itself so the worker cannot inherit the payload's integrity.
            for &field in URL_FIELDS {
                if Some(field) != url
                    && let Some(link) = v.get(field).and_then(Value::as_str)
                {
                    let path = child(rule, field);
                    let option = json!({
                        "kind": "url",
                        "path": path,
                        "url_path": "",
                        "sample_pointer": child(ptr, field),
                        "sample": link.chars().take(300).collect::<String>(),
                    });
                    self.insert(&path, "url", "", option);
                }
            }
        }
        if let Some(a) = v.as_array() {
            for (i, val) in a.iter().take(SAMPLE_LIMIT).enumerate() {
                self.walk(
                    val,
                    &format!("{rule}/*"),
                    &child(ptr, &i.to_string()),
                    depth + 1,
                    false,
                );
            }
        } else if let Some(o) = v.as_object() {
            let hashed = !o.is_empty()
                && o.values()
                    .take(12)
                    .all(|r| digest_field(r).is_some() && url_field(r).is_none());
            let large = o.len() > 24 && o.values().take(12).all(|r| url_field(r).is_some());
            for (k, val) in o
                .iter()
                .take(if hashed || large { 8 } else { OPTION_LIMIT })
            {
                let descriptor_field = val.is_string()
                    && (url.is_some() || hash.is_some())
                    && (URL_FIELDS.contains(&k.as_str())
                        || matches!(k.as_str(), "filename" | "path" | "name"));
                if k == "*" || descriptor_field {
                    continue;
                }
                let pattern = if hashed || large {
                    format!("{rule}/*")
                } else {
                    child(rule, k)
                };
                self.walk(val, &pattern, &child(ptr, k), depth + 1, hashed || large);
            }
        }
    }
}

// Only the primary descriptor URL inherits sibling size and hash fields.
// Other options, including URL strings in arrays, select the scalar itself.
fn link_descriptor<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let (parent, field) = pointer.rsplit_once('/')?;
    let descriptor = root.pointer(parent)?;
    (url_field(descriptor) == Some(field)).then_some(descriptor)
}

fn pointer_ok(p: &str) -> bool {
    (p.is_empty() || p.starts_with('/'))
        && p.len() <= PATH_LIMIT
        && !p.chars().any(char::is_control)
        && p.split('~')
            .skip(1)
            .all(|v| v.starts_with('0') || v.starts_with('1'))
}
impl Inspect {
    async fn inspect(self, state: &AppState) -> anyhow::Result<Value> {
        ensure!(
            self.url.is_some() != self.upload.is_some(),
            "Supply one URL or JSON upload"
        );
        ensure!(
            self.trail.len() <= 8 && self.trail.iter().all(|p| pointer_ok(p)),
            "Invalid document trail"
        );
        ensure!(
            self.row.is_none() || self.collection.is_none(),
            "Choose a sample or a filtered collection"
        );
        ensure!(
            self.row
                .iter()
                .chain(&self.collection)
                .all(|p| pointer_ok(p)),
            "Invalid record pointer"
        );
        ensure!(
            self.filters.len() <= 32
                && (self.filters.is_empty() || self.collection.is_some())
                && self
                    .filters
                    .iter()
                    .all(|f| (f.path == "@key" || pointer_ok(&f.path))
                        && (1..=512).contains(&f.values.len())
                        && f.values.iter().all(|v| v.len() <= 255)),
            "Invalid sample filters"
        );
        let initial = self.url.as_deref().map(Url::parse).transpose()?;
        let (mut bytes, mut base) =
            worker::inspect_page(state, initial.as_ref(), self.upload.as_deref()).await?;
        let mut seen: BTreeSet<Url> = initial.into_iter().chain(base.clone()).collect();
        let mut total = bytes.len();
        let mut document: Value =
            serde_json::from_slice(&bytes).context("Invalid JSON document")?;
        for pointer in &self.trail {
            let raw = document
                .pointer(pointer)
                .and_then(Value::as_str)
                .context("Sample link is missing")?;
            let target = match &base {
                Some(url) => url.join(raw)?,
                None => Url::parse(raw).context("Uploaded documents need absolute links")?,
            };
            ensure!(seen.insert(target.clone()), "Document cycle in sample");
            let (child, final_url) = worker::inspect_page(state, Some(&target), None).await?;
            if let Some(url) = &final_url
                && url != &target
            {
                ensure!(
                    seen.insert(url.clone()),
                    "Document redirect cycle in sample"
                )
            }
            if let Some(descriptor) = link_descriptor(&document, pointer) {
                worker::verify_inspection(&child, descriptor)?;
            }
            total += child.len();
            ensure!(total <= 32 << 20, "Document sample exceeds byte limit");
            document =
                serde_json::from_slice(&child).context("Linked document contains invalid JSON")?;
            bytes = child;
            base = final_url;
        }
        let mut lists = Vec::new();
        if self.trail.is_empty() && self.row.is_none() && self.collection.is_none() {
            collections(&document, "", 0, &mut lists)
        }
        let mut chosen = None;
        let row = if let Some(path) = &self.collection {
            let rows = document
                .pointer(path)
                .context("Release collection is missing")?;
            let id = collection_identity(rows).context("Unrecognized release collection")?;
            chosen = entries(rows)
                .filter(|(k, r)| {
                    self.filters
                        .iter()
                        .all(|f| filter_value(r, k, &f.path).is_some_and(|v| f.values.contains(&v)))
                })
                .find_map(|(k, r)| sample(path, &k, r, id.as_deref()));
            chosen.as_ref().map(|s| s.pointer.as_str())
        } else {
            Some(self.row.as_deref().unwrap_or(""))
        };
        let mut branches = Branches {
            options: BTreeMap::new(),
            remaining: 4096,
            truncated: false,
        };
        if let Some(p) = row {
            branches.walk(
                document.pointer(p).context("Sample record is missing")?,
                "",
                p,
                0,
                false,
            );
        }
        Ok(
            json!({"collections":lists,"branches":branches.options.into_values().collect::<Vec<_>>(),
            "sample":chosen,"document_bytes":bytes.len(),"truncated":branches.truncated}),
        )
    }
}

pub(super) async fn inspect(
    State(s): State<AppState>,
    Path(id): Path<i64>,
    Json(input): Json<Inspect>,
) -> ResultJson {
    parking::require_available(&mut *s.db.acquire().await?, id).await?;
    Ok(Json(
        input
            .inspect(&s)
            .await
            .map_err(|e| bad(&format!("{e:#}")))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suggests_filtered_samples_across_the_entire_inventory() {
        let mut rows: Vec<_> = (0..700)
            .map(|i| json!({"id":i,"track":"daily","url":"https://example.org/detail"}))
            .collect();
        rows.push(json!({"id":"new","track":"periodic","url":"https://example.org/different"}));
        let source = json!({"wrapper":{"entries":rows}});
        let mut lists = Vec::new();
        collections(&source, "", 0, &mut lists);
        let records = lists.iter().find(|v| v.path == "/wrapper/entries").unwrap();
        assert_eq!(records.version_path.as_deref(), Some("/id"));
        assert_eq!(records.count, 701);
        assert!(records.samples.len() <= SAMPLE_LIMIT);
        assert!(
            records
                .samples
                .iter()
                .any(|s| s.id == "new" && s.pointer == "/wrapper/entries/700")
        );
    }
    #[test]
    fn samples_array_and_keyed_leaves_with_escaped_concrete_pointers() {
        let source = json!({"dependencies":[{"file":{"url":"part.bin","path":"a/part.bin"}}],
                          "objects":{"a/b~c":{"hash":"00".repeat(20),"size":4}}});
        let mut found = Branches {
            options: BTreeMap::new(),
            remaining: 4096,
            truncated: false,
        };
        found.walk(&source, "", "/records/3", 0, false);
        let url = &found.options[&("/dependencies/*/file".into(), "url".into(), "/url".into())];
        assert_eq!(url["sample_pointer"], "/records/3/dependencies/0/file/url");
        assert_eq!(url["name_path"], "/path");
        let derived = &found.options[&("/objects/*".into(), "digest".into(), "/hash".into())];
        assert_eq!(derived["sample_pointer"], "/records/3/objects/a~1b~0c/hash");
        assert_eq!(derived["name_path"], "@key");
        assert_eq!(derived["algorithm"], "sha1");
    }
    #[tokio::test]
    async fn descriptors_expose_nested_files_and_independent_metadata_links() -> anyhow::Result<()>
    {
        let tmp = tempfile::tempdir()?;
        let state = crate::open(tmp.path(), "https://mirror.example").await?;
        let server = json!({"browser_download_url":"https://files.example/server.jar",
            "url":"https://api.example/asset.json","size":17,"sha1":"01".repeat(20)});
        let manifest = json!({"versions":[{"id":"v1","url":"https://api.example/release.json",
            "downloads":{"server":server},"releaseNotes":"https://files.example/notes.html",
            "other":[{"href":"a"},{"url":"b"}]}]});
        let source = manifest.to_string();
        let inspect: Inspect =
            serde_json::from_value(json!({"upload":source,"row":"/versions/0"}))?;
        let choices = inspect.inspect(&state).await?;
        let options = choices["branches"].as_array().unwrap();
        let option = |path: &str, field: &str| {
            options
                .iter()
                .find(|v| v["path"] == path && v["url_path"] == field)
                .unwrap()
        };
        option("", "/url");
        option("/releaseNotes", "");
        option("/other/*", "/href");
        option("/other/*", "/url");
        let file = option("/downloads/server", "/browser_download_url");
        assert_eq!(file["size_path"], "/size");
        assert_eq!(file["algorithm"], "sha1");
        let meta = option("/downloads/server/url", "");
        assert!(meta.get("size_path").is_none() && meta.get("digest_path").is_none());
        assert_eq!(
            link_descriptor(&manifest, file["sample_pointer"].as_str().unwrap()),
            Some(&server)
        );
        assert!(link_descriptor(&manifest, meta["sample_pointer"].as_str().unwrap()).is_none());

        let rules: Vec<_> = [file,meta].into_iter().map(|option|
            json!({"kind":"file","path":option["path"],"url_path":option["url_path"],
                "size_path":option["size_path"],"digest_path":option["digest_path"],"algorithm":option["algorithm"]})
        ).collect();
        let preview = worker::preview(
            &state,
            &SourceInput {
                upload: Some(source),
                url: None,
                mapping: Some(json!({"nested":{
                    "path":"/versions","version_path":"/id","rules":rules
                }})),
            },
        )
        .await?;
        let files = preview["releases"][0]["artifacts"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["filename"], "asset.json");
        assert!(files[0]["size"].is_null());
        assert_eq!(files[1]["filename"], "server.jar");
        assert_eq!(files[1]["size"], 17);
        Ok(())
    }
}
