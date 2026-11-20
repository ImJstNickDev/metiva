//! Execute saved JSON-pointer selections against an immutable graph of documents.
use super::adapter::{self, Artifact, Catalog, Mapping};
use anyhow::{Context, Result, anyhow, bail, ensure};
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::{Arc, OnceLock},
};
use url::Url;

const MAX_RULES: usize = 128;
const MAX_DEPTH: usize = 8;
const MAX_STEPS: usize = 2_000_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Nested {
    path: String,
    version_path: Option<String>,
    #[serde(default)]
    filters: Vec<Filter>,
    limit: Option<usize>,
    rules: Vec<Rule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    path: String,
    values: Vec<Value>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Document,
    File,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    path: String,
    kind: Kind,
    #[serde(default = "required")]
    required: bool,
    url_path: Option<String>,
    name_path: Option<String>,
    size_path: Option<String>,
    digest_path: Option<String>,
    algorithm: Option<String>,
    url_template: Option<String>,
    #[serde(default)]
    rules: Vec<Rule>,
}
fn required() -> bool {
    true
}

fn pointer(path: &str) -> Result<()> {
    ensure!(
        path.len() <= 2048
            && (path.is_empty() || path.starts_with('/'))
            && !path.chars().any(char::is_control)
            && path.split('/').count() <= 65
            && path
                .split('~')
                .skip(1)
                .all(|s| s.starts_with('0') || s.starts_with('1')),
        "invalid nested selection pointer"
    );
    Ok(())
}

impl Nested {
    pub(super) fn validate(&self) -> Result<()> {
        pointer(&self.path)?;
        if let Some(path) = &self.version_path
            && path != "@key"
        {
            pointer(path)?;
        }
        ensure!(
            self.limit
                .is_none_or(|limit| (1..=adapter::MAX_ITEMS).contains(&limit)),
            "invalid release limit"
        );
        ensure!(self.filters.len() <= 32, "too many release filters");
        for filter in &self.filters {
            if filter.path != "@key" {
                pointer(&filter.path)?;
            }
            ensure!(
                !filter.values.is_empty()
                    && filter.values.len() <= 512
                    && filter
                        .values
                        .iter()
                        .all(|v| matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))),
                "select scalar values for each filter"
            );
        }
        check_rules(&self.rules, 1, &mut 0)
    }
}

fn check_rules(rules: &[Rule], depth: usize, count: &mut usize) -> Result<()> {
    ensure!(
        !rules.is_empty() && depth <= MAX_DEPTH,
        "empty or too deeply nested document selection"
    );
    *count += rules.len();
    ensure!(*count <= MAX_RULES, "too many nested rules");
    for rule in rules {
        pointer(&rule.path)?;
        for path in [&rule.url_path, &rule.size_path, &rule.digest_path]
            .into_iter()
            .flatten()
        {
            pointer(path)?;
        }
        if let Some(path) = &rule.name_path
            && path != "@key"
        {
            pointer(path)?;
        }
        ensure!(
            rule.algorithm.is_some() == rule.digest_path.is_some(),
            "supply a digest pointer and algorithm together"
        );
        if let Some(alg) = &rule.algorithm {
            ensure!(
                ["md5", "sha1", "sha256", "sha384", "sha512"].contains(&alg.as_str()),
                "unsupported digest algorithm"
            );
        }
        if let Some(template) = &rule.url_template {
            let literal = template.replace("{digest}", "").replace("{prefix}", "");
            ensure!(
                rule.url_path.is_none()
                    && rule.algorithm.is_some()
                    && template.len() <= 4096
                    && template.contains("{digest}")
                    && !literal.contains(['{', '}']),
                "invalid content URL template"
            );
        }
        match rule.kind {
            Kind::Document => {
                ensure!(
                    rule.name_path.is_none(),
                    "a document rule cannot select a filename"
                );
                check_rules(&rule.rules, depth + 1, count)?;
            }
            Kind::File => ensure!(rule.rules.is_empty(), "file rule contains child rules"),
        }
    }
    Ok(())
}

#[derive(Default)]
struct Integrity {
    size: Option<u64>,
    hashes: BTreeMap<String, String>,
}

fn digest_algorithm(hash: &str) -> Option<&'static str> {
    match hash.len() {
        32 => Some("md5"),
        40 => Some("sha1"),
        64 => Some("sha256"),
        96 => Some("sha384"),
        128 => Some("sha512"),
        _ => None,
    }
}

impl Integrity {
    fn from(descriptor: &Value, rule: Option<&Rule>) -> Result<Self> {
        let custom = rule.and_then(|r| r.digest_path.as_deref().zip(r.algorithm.as_deref()));
        // A singular bare digest may carry its algorithm in the saved rule.
        // For conventional hash/digest fields, their hex widths distinguish
        // the five algorithms accepted by the verifier.
        let inferred = descriptor
            .get("digest")
            .and_then(Value::as_str)
            .and_then(|s| digest_algorithm(s).map(|alg| ("/digest", alg)));
        let mut hashes = adapter::descriptor_hashes(
            descriptor,
            &Mapping::default(),
            custom
                .filter(|(p, _)| *p == "/digest")
                .or(inferred)
                .or(custom),
        )?;
        if let Some(value) = descriptor.get("hash").filter(|v| !v.is_null()) {
            let raw = value.as_str().context("invalid descriptor hash")?;
            let (alg, hash) = raw
                .split_once(':')
                .or_else(|| digest_algorithm(raw).map(|alg| (alg, raw)))
                .context("unrecognized descriptor hash")?;
            adapter::add_hash(&mut hashes, alg, hash)?;
        }
        if let Some((path, alg)) = custom {
            let raw = descriptor
                .pointer(path)
                .and_then(Value::as_str)
                .context("selected digest is missing")?;
            let hash = if let Some((prefix, digest)) = raw.split_once(':') {
                ensure!(
                    prefix.eq_ignore_ascii_case(alg),
                    "declared digest algorithm disagrees with selected algorithm"
                );
                digest
            } else {
                raw
            };
            adapter::add_hash(&mut hashes, alg, hash)?;
        }
        let mut size = None;
        for v in ["size", "bytes"]
            .iter()
            .filter_map(|k| descriptor.get(k))
            .filter(|v| !v.is_null())
            .chain(
                rule.and_then(|r| r.size_path.as_ref())
                    .map(|path| descriptor.pointer(path).context("selected size is missing"))
                    .transpose()?,
            )
        {
            let n = v
                .as_u64()
                .or_else(|| v.as_str()?.parse().ok())
                .filter(|n| *n <= i64::MAX as u64)
                .context("invalid descriptor size")?;
            ensure!(
                size.is_none_or(|prev| prev == n),
                "conflicting declared sizes"
            );
            size = Some(n);
        }
        Ok(Self { size, hashes })
    }

    fn verify(&self, bytes: &[u8], cache: &DigestCache) -> Result<()> {
        ensure!(
            self.size.is_none_or(|size| size == bytes.len() as u64),
            "child document size mismatch"
        );
        for (algorithm, expected) in &self.hashes {
            let observed = match algorithm.as_str() {
                "md5" => cache
                    .md5
                    .get_or_init(|| hex::encode(md5::Md5::digest(bytes))),
                "sha1" => cache
                    .sha1
                    .get_or_init(|| hex::encode(sha1::Sha1::digest(bytes))),
                "sha256" => cache
                    .sha256
                    .get_or_init(|| hex::encode(Sha256::digest(bytes))),
                "sha384" => cache
                    .sha384
                    .get_or_init(|| hex::encode(sha2::Sha384::digest(bytes))),
                "sha512" => cache
                    .sha512
                    .get_or_init(|| hex::encode(sha2::Sha512::digest(bytes))),
                _ => bail!("unsupported child document digest"),
            };
            ensure!(observed == expected, "child document {algorithm} mismatch");
        }
        Ok(())
    }
}

#[derive(Default)]
struct DigestCache {
    md5: OnceLock<String>,
    sha1: OnceLock<String>,
    sha256: OnceLock<String>,
    sha384: OnceLock<String>,
    sha512: OnceLock<String>,
}

pub(crate) fn verify_inspection(bytes: &[u8], descriptor: &Value) -> Result<()> {
    Integrity::from(descriptor, None)?.verify(bytes, &DigestCache::default())
}

pub(super) struct Document {
    bytes: Vec<u8>,
    base: Option<Url>,
    observation: Option<i64>,
    pub(super) link: Option<String>,
    hashes: DigestCache,
    parsed: OnceLock<Result<Value, String>>,
    complete: OnceLock<Result<(), String>>,
}
impl Document {
    pub(super) fn new(bytes: Vec<u8>, base: Option<Url>, observation: Option<i64>) -> Self {
        Self {
            bytes,
            base,
            observation,
            link: None,
            hashes: DigestCache::default(),
            parsed: OnceLock::new(),
            complete: OnceLock::new(),
        }
    }
    fn linked_json(&self) -> Result<&Value> {
        let body = self.json()?;
        self.complete
            .get_or_init(|| {
                super::feed::check_nested_child(body, self.link.as_deref())
                    .map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(|e| anyhow!("{e}"))?;
        Ok(body)
    }
    fn json(&self) -> Result<&Value> {
        self.parsed
            .get_or_init(|| {
                serde_json::from_slice(&self.bytes)
                    .map_err(|e| format!("invalid JSON document: {e}"))
            })
            .as_ref()
            .map_err(|e| anyhow!("{e}"))
    }
}

/// Inner errors describe an observed upstream failure. Outer errors indicate
/// that storage or replay failed, so no release may publish from this graph.
pub(super) trait Documents {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>>;
}

#[derive(Debug)]
struct GraphError;
impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "manifest graph is incomplete")
    }
}
impl std::error::Error for GraphError {}

fn bound(condition: bool, error: &'static str) -> Result<()> {
    if !condition {
        return Err(anyhow!(error).context(GraphError));
    }
    Ok(())
}

struct Graph<'a, D> {
    docs: &'a mut D,
    cache: HashMap<Option<Url>, Result<Arc<Document>, String>>,
    requests: usize,
    bytes: usize,
    steps: usize,
    descriptor_bytes: usize,
    leaf_count: usize,
    inventory: BTreeMap<i64, Catalog>,
}
impl<D: Documents> Graph<'_, D> {
    async fn load(&mut self, url: Option<&Url>) -> Result<Arc<Document>> {
        if let Some(cached) = self.cache.get(&url.cloned()) {
            return cached.clone().map_err(|e| anyhow!("{e}"));
        }
        self.requests += 1;
        bound(
            self.requests <= super::feed::MAX_PAGES,
            "nested manifest exceeds document limit",
        )?;
        let mut result = self.docs.load(url).await.context(GraphError)?.map(Arc::new);
        if let Ok(doc) = &result {
            self.bytes += doc.bytes.len();
            bound(
                self.bytes <= super::feed::SOURCE_BYTES,
                "nested manifest exceeds byte limit",
            )?;
            if let Some(base) = &doc.base {
                match self.cache.get(&Some(base.clone())) {
                    Some(Ok(prior)) if prior.bytes != doc.bytes => {
                        result = Err("document changed between redirect aliases".into());
                    }
                    None => {
                        self.cache.insert(Some(base.clone()), result.clone());
                    }
                    _ => (),
                }
            }
        }
        self.cache.insert(url.cloned(), result.clone());
        result.map_err(|e| anyhow!("{e}"))
    }

    async fn rules(
        &mut self,
        release: &str,
        node: &Value,
        doc: &Document,
        rules: &[Rule],
        ancestors: &[Url],
        out: &mut Catalog,
    ) -> Result<()> {
        for rule in rules {
            let entries = matches(node, &rule.path, rule.required, &mut self.steps)
                .with_context(|| format!("selected path {}", rule.path))?;
            for (descriptor, key) in entries {
                let integrity = Integrity::from(descriptor, Some(rule))?;
                let url = descriptor_url(rule, descriptor, &integrity, doc.base.as_ref())?;
                match rule.kind {
                    Kind::Document => {
                        ensure!(
                            !ancestors.contains(&url),
                            "document reference cycle at {}",
                            rule.path
                        );
                        let child = self.load(Some(&url)).await?;
                        ensure!(
                            !child
                                .base
                                .as_ref()
                                .is_some_and(|base| ancestors.contains(base)),
                            "document redirect cycle at {}",
                            rule.path
                        );
                        integrity.verify(&child.bytes, &child.hashes)?;
                        let body = child.linked_json()?;
                        let mut chain = ancestors.to_vec();
                        chain.push(url);
                        chain.extend(child.base.clone());
                        Box::pin(self.rules(release, body, &child, &rule.rules, &chain, out))
                            .await?;
                    }
                    Kind::File => {
                        self.leaf_count += 1;
                        bound(
                            self.leaf_count <= adapter::MAX_ITEMS,
                            "too many observation artifact references",
                        )?;
                        let logical = logical_name(rule, descriptor, key, &url)?;
                        let (name, mut metadata) = filename(&logical)?;
                        for k in ["platform", "os", "arch", "architecture"] {
                            if let Some(v) = descriptor.get(k).filter(|v| v.is_string()) {
                                metadata.insert(k.into(), v.clone());
                            }
                        }
                        self.descriptor_bytes += release.len()
                            + name.len()
                            + url.as_str().len()
                            + metadata
                                .values()
                                .filter_map(Value::as_str)
                                .map(str::len)
                                .sum::<usize>();
                        bound(
                            self.descriptor_bytes <= super::feed::SOURCE_BYTES * 4,
                            "nested descriptors exceed expanded byte limit",
                        )?;
                        let id = (release.to_owned(), name);
                        let asset = Artifact {
                            url,
                            size: integrity.size,
                            sha256: integrity.hashes.get("sha256").cloned(),
                            hashes: integrity.hashes,
                            metadata,
                        };
                        adapter::insert(out, id.clone(), asset.clone())?;
                        if let Some(obs) = doc.observation {
                            adapter::insert(self.inventory.entry(obs).or_default(), id, asset)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn descriptor_url(
    rule: &Rule,
    descriptor: &Value,
    integrity: &Integrity,
    base: Option<&Url>,
) -> Result<Url> {
    if let Some(template) = &rule.url_template {
        let digest = integrity
            .hashes
            .get(
                rule.algorithm
                    .as_ref()
                    .context("missing template algorithm")?,
            )
            .context("missing template digest")?;
        let value = template
            .replace("{digest}", digest)
            .replace("{prefix}", &digest[..2]);
        return adapter::parse_url(&value, base);
    }
    let raw = match &rule.url_path {
        Some(path) => descriptor.pointer(path).and_then(Value::as_str),
        None => descriptor.as_str().or_else(|| {
            adapter::URLS
                .iter()
                .find_map(|k| descriptor.get(k)?.as_str())
        }),
    }
    .filter(|s| !s.trim().is_empty())
    .context("selected descriptor URL is missing")?;
    adapter::parse_url(raw, base)
}

fn logical_name(rule: &Rule, descriptor: &Value, key: Option<&str>, url: &Url) -> Result<String> {
    let selected = match rule.name_path.as_deref() {
        Some("@key") => Some(key.context("selected filename needs a map key")?),
        Some(path) => Some(
            descriptor
                .pointer(path)
                .and_then(Value::as_str)
                .context("selected filename is missing")?,
        ),
        None => ["path", "filename", "name"]
            .into_iter()
            .find_map(|k| descriptor.get(k)?.as_str()),
    };
    if let Some(name) = selected {
        return Ok(name.to_string());
    }
    Ok(percent_decode_str(
        url.path_segments()
            .and_then(|mut p| p.next_back())
            .context("missing URL filename")?,
    )
    .decode_utf8()?
    .into_owned())
}

fn filename(logical: &str) -> Result<(String, Map<String, Value>)> {
    ensure!(
        !logical.is_empty() && logical.len() <= 8192 && !logical.chars().any(char::is_control),
        "invalid logical filename"
    );
    // Reserve this prefix for encoded paths, including upstream filenames that
    // themselves start with it. Keep the original suffix for archive validation.
    let name = if !logical.starts_with("path~") && adapter::segment(logical).is_ok() {
        logical.to_owned()
    } else {
        let basename = logical
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty() && !matches!(*s, "." | ".."))
            .context("invalid logical path")?;
        let suffix: String = basename
            .chars()
            .rev()
            .scan(0usize, |bytes, c| {
                *bytes += c.len_utf8();
                (*bytes <= 180).then_some(c)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!(
            "path~{}~{suffix}",
            hex::encode(Sha256::digest(logical.as_bytes()))
        )
    };
    let mut metadata = Map::new();
    if logical != name {
        metadata.insert("logical_path".into(), Value::String(logical.into()));
    }
    Ok((adapter::segment(&name)?, metadata))
}

type Match<'a> = (&'a Value, Option<&'a str>);
fn charge(steps: &mut usize, n: usize) -> Result<()> {
    *steps = steps.saturating_add(n);
    bound(
        *steps <= MAX_STEPS,
        "nested manifest exceeds selection work limit",
    )
}
fn matches<'a>(
    root: &'a Value,
    path: &str,
    required: bool,
    steps: &mut usize,
) -> Result<Vec<Match<'a>>> {
    let mut current = vec![(root, None)];
    for token in path.split('/').skip(1) {
        let decoded = token.replace("~1", "/").replace("~0", "~");
        let mut next = Vec::new();
        let field = format!("/{token}");
        for (v, key) in current {
            charge(steps, 1)?;
            let before = next.len();
            if token == "*" {
                match v {
                    Value::Array(a) => {
                        bound(
                            a.len() <= adapter::MAX_ITEMS - next.len(),
                            "too many selected descriptors",
                        )?;
                        charge(steps, a.len())?;
                        next.extend(a.iter().map(|x| (x, key)));
                    }
                    Value::Object(m) => {
                        bound(
                            m.len() <= adapter::MAX_ITEMS - next.len(),
                            "too many selected descriptors",
                        )?;
                        charge(steps, m.len())?;
                        next.extend(m.iter().map(|(k, x)| (x, Some(k.as_str()))));
                    }
                    Value::Null if !required => (),
                    _ => bail!("wildcard expects an array or map"),
                }
            } else if let Some(value) = v.pointer(&field).filter(|v| !v.is_null()) {
                next.push((value, key));
            } else if !v.is_object() && !v.is_array() && !v.is_null() {
                bail!("expected a collection while selecting {decoded}");
            }
            ensure!(
                !required || next.len() > before,
                "required field {decoded} is missing or empty"
            );
            bound(
                next.len() <= adapter::MAX_ITEMS,
                "too many selected descriptors",
            )?;
        }
        current = next;
    }
    charge(steps, current.len())?;
    Ok(current)
}

fn selected(filter: &Filter, row: &Value, key: Option<&str>) -> bool {
    if filter.path == "@key" {
        return key.is_some_and(|k| filter.values.iter().any(|v| v.as_str() == Some(k)));
    }
    row.pointer(&filter.path).is_some_and(|value| {
        if filter.values.contains(value) {
            return true;
        }
        if value.is_boolean() || value.is_number() {
            let text = value.to_string();
            return filter
                .values
                .iter()
                .any(|v| v.as_str() == Some(text.as_str()));
        }
        false
    })
}
pub(super) async fn execute(
    rules: &Nested,
    url: Option<&Url>,
    docs: &mut impl Documents,
    strict: bool,
) -> Result<(Catalog, BTreeMap<i64, Catalog>)> {
    rules.validate()?;
    let mut graph = Graph {
        docs,
        cache: HashMap::new(),
        requests: 0,
        bytes: 0,
        steps: 0,
        descriptor_bytes: 0,
        leaf_count: 0,
        inventory: BTreeMap::new(),
    };
    let root = graph.load(url).await?;
    let list = root
        .json()?
        .pointer(&rules.path)
        .context("selected release collection is missing")?;
    let (rows, count): (Box<dyn Iterator<Item = Match<'_>> + Send + '_>, usize) = match list {
        Value::Array(a) => (Box::new(a.iter().map(|v| (v, None))), a.len()),
        Value::Object(m) => (
            Box::new(m.iter().map(|(k, v)| (v, Some(k.as_str())))),
            m.len(),
        ),
        _ => bail!("release collection must be an array or map"),
    };
    super::feed::check_nested_root(
        (!rules.path.is_empty()).then_some(root.json()?),
        root.link.as_deref(),
        count,
    )?;
    let mut catalog = Catalog::new();
    let ancestors: Vec<_> = url.cloned().into_iter().chain(root.base.clone()).collect();
    let mut count = 0;
    for (row, key) in rows {
        charge(
            &mut graph.steps,
            1 + rules.filters.iter().map(|f| f.values.len()).sum::<usize>(),
        )?;
        if !rules.filters.iter().all(|f| selected(f, row, key)) {
            continue;
        }
        count += 1;
        ensure!(count <= adapter::MAX_ITEMS, "too many releases");
        let version = match rules.version_path.as_deref() {
            None | Some("@key") => adapter::segment(
                key.context("release identifier needs a map key or version pointer")?,
            )?,
            Some(path) => match row.pointer(path) {
                Some(Value::String(s)) => adapter::segment(s)?,
                Some(Value::Number(n)) => adapter::segment(&n.to_string())?,
                _ => bail!("selected release identifier is missing or invalid"),
            },
        };
        if !catalog.release_errors.contains_key(&version) {
            let metadata = row
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(k, v)| {
                    [
                        "title",
                        "name",
                        "channel",
                        "published_at",
                        "date",
                        "prerelease",
                    ]
                    .contains(&k.as_str())
                        && (v.is_string() || v.is_number() || v.is_boolean())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            let result = async {
                adapter::add_version(&mut catalog, &version, &metadata)?;
                graph
                    .rules(&version, row, &root, &rules.rules, &ancestors, &mut catalog)
                    .await
            }
            .await;
            if let Err(error) = result {
                if error.is::<GraphError>() {
                    return Err(error);
                }
                catalog.check_limits()?;
                catalog.reject(version, format!("{error:#}"));
            }
        }
        if rules.limit == Some(count) {
            break;
        }
    }
    catalog.check_limits()?;
    ensure!(
        !strict || !catalog.assets.is_empty() || !catalog.release_errors.is_empty(),
        "nested selection matched no files"
    );
    for inventory in graph.inventory.values_mut() {
        inventory
            .assets
            .retain(|(release, _), _| !catalog.release_errors.contains_key(release));
    }
    Ok((catalog, graph.inventory))
}

#[cfg(test)]
#[path = "nested_tests.rs"]
mod tests;
