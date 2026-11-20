use super::http::validate_url;
use anyhow::{Context, Result, bail, ensure};
use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use url::Url;

const MAX_ITEMS: usize = 100_000;
const RELEASES: &[&str] = &["releases", "versions", "items", "data"];
const VERSION: &[&str] = &["version", "tag_name", "release", "name", "id"];
const ASSETS: &[&str] = &["artifacts", "assets", "files", "downloads", "platforms"];
const URLS: &[&str] = &[
    "browser_download_url",
    "download_url",
    "url",
    "href",
    "link",
];
const NAMES: &[&str] = &["filename", "name"];
const SIZES: &[&str] = &["size", "bytes"];

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Mapping {
    releases_path: Option<String>,
    version_path: Option<String>,
    artifacts_path: Option<String>,
    url_path: Option<String>,
    filename_path: Option<String>,
    size_path: Option<String>,
    sha256_path: Option<String>,
    pub next_path: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Artifact {
    pub url: Url,
    pub size: Option<u64>,
    pub sha256: Option<String>,
    pub hashes: BTreeMap<String, String>,
    pub metadata: Map<String, Value>,
}
#[derive(Default, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    pub assets: BTreeMap<(String, String), Artifact>,
    pub versions: BTreeMap<String, Map<String, Value>>,
}
impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }
}

fn escaped(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}
fn pointer(path: &str) -> String {
    if path.is_empty() || path == "$" {
        return String::new();
    }
    if path.starts_with('/') {
        return path.to_owned();
    }
    path.trim_start_matches("$.")
        .split('.')
        .map(|k| format!("/{}", escaped(k)))
        .collect()
}
fn select<'a>(
    v: &'a Value,
    path: &Option<String>,
    defaults: &[&str],
) -> Option<(String, &'a Value)> {
    let ptr = match path {
        Some(path) => pointer(path),
        None => format!(
            "/{}",
            escaped(defaults.iter().find(|k| v.get(**k).is_some())?)
        ),
    };
    let selected = v.pointer(&ptr)?;
    Some((ptr, selected))
}
fn segment(v: &str) -> Result<String> {
    ensure!(
        v.len() <= 255
            && !v.is_empty()
            && !matches!(v, "." | "..")
            && !v.chars().any(|c| c.is_control() || matches!(c, '/' | '\\')),
        "invalid release or filename"
    );
    Ok(v.to_owned())
}
fn parse_url(v: &str, base: Option<&Url>) -> Result<Url> {
    let mut url = match base {
        Some(b) => b.join(v)?,
        None => Url::parse(v).context("uploaded feeds require absolute artifact URLs")?,
    };
    validate_url(&url)?;
    url.set_fragment(None);
    Ok(url)
}
fn merge_fields(dest: &mut Map<String, Value>, other: &Map<String, Value>) -> Result<()> {
    for (k, v) in other {
        if let Some(existing) = dest.get(k) {
            ensure!(existing == v, "conflicting metadata for field {k}");
        } else {
            dest.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}
fn add_version(out: &mut Catalog, version: &str, meta: &Map<String, Value>) -> Result<()> {
    merge_fields(out.versions.entry(version.into()).or_default(), meta)
        .with_context(|| format!("release {version}"))?;
    ensure!(out.versions.len() <= MAX_ITEMS, "too many releases");
    Ok(())
}
fn insert(out: &mut Catalog, id: (String, String), item: Artifact) -> Result<()> {
    use std::collections::btree_map::Entry;
    match out.assets.entry(id) {
        Entry::Vacant(e) => {
            e.insert(item);
        }
        Entry::Occupied(mut e) => {
            let (release, name) = e.key().clone();
            let prev = e.get_mut();
            ensure!(
                prev.url == item.url,
                "conflicting source URLs for {release}/{name}"
            );
            ensure!(
                !matches!((prev.size,item.size),(Some(a),Some(b)) if a!=b),
                "conflicting sizes for {release}/{name}"
            );
            prev.size = prev.size.or(item.size);
            for (algorithm, hash) in item.hashes {
                let existing = prev.hashes.entry(algorithm).or_insert_with(|| hash.clone());
                ensure!(
                    existing == &hash,
                    "conflicting checksums for {release}/{name}"
                );
            }
            prev.sha256 = prev.hashes.get("sha256").cloned();
            merge_fields(&mut prev.metadata, &item.metadata)
                .with_context(|| format!("artifact {release}/{name}"))?;
        }
    }
    ensure!(out.assets.len() <= MAX_ITEMS, "too many artifacts");
    Ok(())
}
pub(crate) fn merge(out: &mut Catalog, page: Catalog) -> Result<()> {
    for (v, m) in page.versions {
        add_version(out, &v, &m)?;
    }
    for (k, v) in page.assets {
        insert(out, k, v)?;
    }
    Ok(())
}

fn add_hash(hashes: &mut BTreeMap<String, String>, algorithm: &str, digest: &str) -> Result<()> {
    let name = algorithm.to_ascii_lowercase().replace('-', "");
    let length = match name.as_str() {
        "md5" => 32,
        "sha1" => 40,
        "sha256" => 64,
        "sha384" => 96,
        "sha512" => 128,
        _ => bail!("unsupported hash algorithm {algorithm}"),
    };
    let value = digest.to_ascii_lowercase();
    ensure!(
        value.len() == length && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid {algorithm} digest"
    );
    if let Some(previous) = hashes.insert(name, value.clone()) {
        ensure!(previous == value, "conflicting advertised digests");
    }
    Ok(())
}
fn hashes(v: &Value, m: &Mapping) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for alg in ["sha256", "sha384", "sha512", "sha1", "md5"] {
        if let Some(hash) = v.get(alg).filter(|x| !x.is_null()) {
            add_hash(&mut hashes, alg, hash.as_str().context("invalid hash")?)?;
        }
    }
    if let Some((_, hash)) = select(v, &m.sha256_path, &[]).filter(|(_, v)| !v.is_null()) {
        let h = hash.as_str().context("invalid SHA-256")?;
        add_hash(
            &mut hashes,
            "sha256",
            h.strip_prefix("sha256:").unwrap_or(h),
        )?;
    }
    for key in ["digest", "checksum", "checksums", "hashes"] {
        match v.get(key) {
            None | Some(Value::Null) => (),
            Some(Value::String(s)) => {
                let (alg, h) = s
                    .split_once(':')
                    .or_else(|| (s.len() == 64).then_some(("sha256", s.as_str())))
                    .context("checksum requires an algorithm")?;
                add_hash(&mut hashes, alg, h)?;
            }
            Some(Value::Object(obj)) if obj.contains_key("algorithm") => add_hash(
                &mut hashes,
                obj["algorithm"]
                    .as_str()
                    .context("invalid hash algorithm")?,
                obj.get("value")
                    .or(obj.get("digest"))
                    .and_then(Value::as_str)
                    .context("missing checksum")?,
            )?,
            Some(Value::Object(obj)) => {
                for (alg, h) in obj {
                    add_hash(&mut hashes, alg, h.as_str().context("invalid checksum")?)?;
                }
            }
            _ => bail!("invalid {key}"),
        }
    }
    Ok(hashes)
}

struct Document {
    catalog: Catalog,
    positions: Vec<(String, (String, String))>,
    next: Option<Url>,
}
struct Scan<'a> {
    mapping: &'a Mapping,
    base: Option<&'a Url>,
    doc: Document,
}
impl Scan<'_> {
    fn artifact(
        &mut self,
        item: &Value,
        version: &str,
        path: &str,
        platform: &[String],
    ) -> Result<()> {
        let m = self.mapping;
        let url_field = select(item, &m.url_path, URLS).or_else(|| {
            (m.url_path.is_none() && item.is_string()).then_some((String::new(), item))
        });
        if let Some((relative, raw)) = url_field {
            let url = parse_url(raw.as_str().context("expected artifact URL")?, self.base)?;
            let filename = match select(item, &m.filename_path, NAMES) {
                Some((_, v)) => segment(v.as_str().context("invalid filename")?)?,
                None => segment(
                    &percent_decode_str(
                        url.path_segments()
                            .and_then(|mut p| p.next_back())
                            .context("missing filename")?,
                    )
                    .decode_utf8()?,
                )?,
            };
            let size = select(item, &m.size_path, SIZES)
                .filter(|(_, v)| !v.is_null())
                .map(|(_, v)| {
                    v.as_u64()
                        .or_else(|| v.as_str()?.parse().ok())
                        .filter(|n| *n <= i64::MAX as u64)
                        .context("invalid artifact size")
                })
                .transpose()?;
            let hashes = hashes(item, m)?;
            let mut metadata = item.as_object().cloned().unwrap_or_default();
            for k in URLS.iter().chain(NAMES).chain(SIZES).chain(
                [
                    "sha256",
                    "sha384",
                    "sha512",
                    "md5",
                    "sha1",
                    "checksum",
                    "checksums",
                    "digest",
                    "hashes",
                ]
                .iter(),
            ) {
                metadata.remove(*k);
            }
            for p in [&m.url_path, &m.filename_path, &m.size_path, &m.sha256_path]
                .into_iter()
                .flatten()
            {
                if let Some(first) = pointer(p).split('/').nth(1) {
                    metadata.remove(&first.replace("~1", "/").replace("~0", "~"));
                }
            }
            if !platform.is_empty() {
                let label = Value::String(platform.join("/"));
                metadata.entry("platform".to_string()).or_insert(label);
            }
            let id = (version.to_owned(), filename);
            insert(
                &mut self.doc.catalog,
                id.clone(),
                Artifact {
                    url,
                    size,
                    sha256: hashes.get("sha256").cloned(),
                    hashes,
                    metadata,
                },
            )?;
            self.doc.positions.push((format!("{path}{relative}"), id));
            ensure!(
                self.doc.positions.len() <= MAX_ITEMS,
                "too many artifact references"
            );
        } else if let Some(array) = item.as_array() {
            for (i, v) in array.iter().enumerate() {
                self.artifact(v, version, &format!("{path}/{i}"), platform)?;
            }
        } else if let Some(obj) = item.as_object() {
            ensure!(!obj.is_empty(), "empty artifact descriptor");
            for (k, v) in obj {
                let mut nested = platform.to_vec();
                nested.push(k.clone());
                self.artifact(v, version, &format!("{path}/{}", escaped(k)), &nested)?;
            }
        } else {
            bail!("missing artifact URL");
        }
        Ok(())
    }
    fn release(&mut self, release: &Value, path: &str, key: Option<&str>) -> Result<()> {
        let m = self.mapping;
        let version = match select(release, &m.version_path, VERSION) {
            Some((_, Value::String(v))) => segment(v)?,
            Some((_, Value::Number(n))) => segment(&n.to_string())?,
            None => segment(key.context("release version is missing")?)?,
            _ => bail!("invalid release version"),
        };
        let assets = select(release, &m.artifacts_path, ASSETS);
        let mut meta = Map::new();
        if assets.is_some() {
            meta = release.as_object().cloned().unwrap_or_default();
            for k in VERSION.iter().chain(ASSETS) {
                meta.remove(*k);
            }
            for p in [&m.version_path, &m.artifacts_path].into_iter().flatten() {
                if let Some(first) = pointer(p).split('/').nth(1) {
                    meta.remove(&first.replace("~1", "/").replace("~0", "~"));
                }
            }
        }
        add_version(&mut self.doc.catalog, &version, &meta)?;
        if let Some((ptr, items)) = assets {
            if items.as_object().is_some_and(|o| o.is_empty()) {
                return Ok(());
            }
            self.artifact(items, &version, &format!("{path}{ptr}"), &[])?;
        } else {
            ensure!(
                m.artifacts_path.is_none()
                    && (select(release, &m.url_path, URLS).is_some() || release.is_array()),
                "missing artifacts for release {version}"
            );
            self.artifact(release, &version, path, &[])?;
        }
        Ok(())
    }
}
fn scan(root: &Value, m: &Mapping, base: Option<&Url>) -> Result<Document> {
    let next = select(root, &m.next_path, &["next", "next_url"])
        .filter(|(_, v)| !v.is_null())
        .map(|(_, v)| parse_url(v.as_str().context("next page must be a URL")?, base))
        .transpose()?;
    let mut scan = Scan {
        mapping: m,
        base,
        doc: Document {
            catalog: Catalog::new(),
            positions: Vec::new(),
            next,
        },
    };
    let (ptr, releases) = select(root, &m.releases_path, RELEASES)
        .or_else(|| m.releases_path.is_none().then_some((String::new(), root)))
        .context("release collection is missing")?;
    match releases {
        Value::Array(array) => {
            for (i, r) in array.iter().enumerate() {
                scan.release(r, &format!("{ptr}/{i}"), None)?;
            }
        }
        Value::Object(_)
            if select(releases, &m.version_path, VERSION).is_some()
                && (select(releases, &m.artifacts_path, ASSETS).is_some()
                    || select(releases, &m.url_path, URLS).is_some()) =>
        {
            scan.release(releases, &ptr, None)?
        }
        Value::Object(obj) => {
            ensure!(
                !obj.is_empty() || !ptr.is_empty(),
                "unrecognized empty manifest"
            );
            for (k, r) in obj {
                scan.release(r, &format!("{ptr}/{}", escaped(k)), Some(k))?;
            }
        }
        _ => bail!("expected a release collection"),
    }
    Ok(scan.doc)
}
pub(crate) fn parse(
    bytes: &[u8],
    m: &Mapping,
    base: Option<&Url>,
    out: &mut Catalog,
) -> Result<Option<Url>> {
    let root: Value = serde_json::from_slice(bytes).context("invalid JSON document")?;
    let doc = scan(&root, m, base)?;
    merge(out, doc.catalog)?;
    Ok(doc.next)
}

pub(crate) fn releases(catalog: &Catalog) -> Value {
    let mut versions: BTreeMap<&str, Vec<Value>> = catalog
        .versions
        .keys()
        .map(|k| (k.as_str(), Vec::new()))
        .collect();
    for ((version, filename), a) in &catalog.assets {
        let mut v = json!({"filename":filename,"url":a.url,"size":a.size,"sha256":a.sha256});
        for k in ["platform", "os", "arch", "architecture"] {
            if let Some(value) = a.metadata.get(k) {
                v[k] = value.clone();
            }
        }
        versions.entry(version).or_default().push(v);
    }
    json!(
        versions
            .into_iter()
            .map(|(version, artifacts)| json!({"version":version,"artifacts":artifacts}))
            .collect::<Vec<_>>()
    )
}
fn signature(v: &Value) -> bool {
    match v {
        Value::Object(m) => m.iter().any(|(k, v)| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "signature" | "signatures" | "signed" | "_signature" | "jws"
            ) || signature(v)
        }),
        Value::Array(a) => a.iter().any(signature),
        _ => false,
    }
}
pub(crate) fn rewrite(
    bytes: &[u8],
    m: &Mapping,
    base: Option<&Url>,
    mirrored: &Catalog,
) -> Option<Value> {
    let mut root: Value = serde_json::from_slice(bytes).ok()?;
    if signature(&root) {
        return None;
    }
    let doc = scan(&root, m, base).ok()?;
    for (pointer, id) in doc.positions {
        *root.pointer_mut(&pointer)? = Value::String(mirrored.assets.get(&id)?.url.to_string());
    }
    Some(root)
}
