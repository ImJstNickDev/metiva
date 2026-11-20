use super::{
    adapter::{self, Catalog},
    feed,
};
use crate::AppState;
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

const MAX_OPTIONS: usize = 512;
const MAX_POINTER: usize = 2048;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub(super) struct Dimension {
    path: String,
    keys: BTreeSet<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Index {
    path: String,
    link_path: String,
    keys: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub index: Option<Index>,
    path: String,
    version_path: String,
    groups: Vec<Dimension>,
    channels: Option<Dimension>,
}

fn check_pointer(path: &str) -> Result<()> {
    ensure!(
        path.len() <= MAX_POINTER
            && (path.is_empty() || path.starts_with('/'))
            && !path.chars().any(char::is_control)
            && path
                .split('~')
                .skip(1)
                .all(|s| s.starts_with('0') || s.starts_with('1')),
        "invalid selection JSON pointer"
    );
    Ok(())
}
fn check_keys(keys: &BTreeSet<String>) -> Result<()> {
    ensure!(
        keys.len() <= MAX_OPTIONS
            && keys
                .iter()
                .all(|k| !k.is_empty() && k.len() <= 512 && !k.chars().any(char::is_control)),
        "invalid selection keys"
    );
    Ok(())
}
fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", adapter::escaped(key))
}
impl Selection {
    pub fn validate(&self) -> Result<()> {
        check_pointer(&self.path)?;
        check_pointer(&self.version_path)?;
        ensure!(
            !self.groups.is_empty() && self.groups.len() <= MAX_OPTIONS,
            "select at least one artifact group"
        );
        ensure!(
            self.groups
                .iter()
                .map(|g| g.keys.len().max(1))
                .sum::<usize>()
                <= MAX_OPTIONS,
            "too many selected variants"
        );
        let mut paths = BTreeSet::new();
        for group in &self.groups {
            check_pointer(&group.path)?;
            check_keys(&group.keys)?;
            ensure!(paths.insert(&group.path), "duplicate artifact group");
        }
        if let Some(ch) = &self.channels {
            check_pointer(&ch.path)?;
            check_keys(&ch.keys)?;
            ensure!(!ch.keys.is_empty(), "select at least one channel");
        }
        Ok(())
    }
}

pub(super) struct Branch {
    pub key: String,
    pub url: Url,
    pub pages: Option<u64>,
    pub count: Option<u64>,
}
impl Index {
    pub fn branches(&self, root: &Value, base: Option<&Url>, strict: bool) -> Result<Vec<Branch>> {
        check_pointer(&self.path)?;
        check_pointer(&self.link_path)?;
        check_keys(&self.keys)?;
        ensure!(!self.keys.is_empty(), "select at least one index key");
        let map = root
            .pointer(&self.path)
            .and_then(Value::as_object)
            .context("selected index is missing")?;
        self.keys
            .iter()
            .filter(|key| strict || map.contains_key(key.as_str()))
            .map(|key| {
                let entry = map
                    .get(key)
                    .with_context(|| format!("selected index key {key} is missing"))?;
                let link = entry
                    .pointer(&self.link_path)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .context("selected index link is missing")?;
                Ok(Branch {
                    key: key.clone(),
                    url: adapter::parse_url(link, base)?,
                    pages: number(entry, "pages")?,
                    count: number(entry, "count")?,
                })
            })
            .collect()
    }
}
pub(super) fn number(v: &Value, field: &str) -> Result<Option<u64>> {
    v.get(field)
        .map(|n| {
            n.as_u64()
                .with_context(|| format!("invalid pagination field {field}"))
        })
        .transpose()
}

// Only suggest strings that resemble explicit artifact locations. Descriptive
// text and release-note fields must never be treated as download targets.
fn file(v: &Value) -> bool {
    match v {
        Value::String(s) => {
            s.starts_with("https://")
                || s.starts_with("http://")
                || s.starts_with("./")
                || s.starts_with('/')
        }
        Value::Object(_) => adapter::URLS
            .iter()
            .any(|k| v.get(*k).is_some_and(Value::is_string)),
        _ => false,
    }
}
fn group(v: &Value) -> Option<BTreeSet<String>> {
    if v.is_object() && file(v)
        || v.as_array()
            .is_some_and(|a| !a.is_empty() && a.iter().all(file))
    {
        return Some(BTreeSet::new());
    }
    let o = v.as_object()?;
    (!o.is_empty() && o.values().all(file)).then(|| o.keys().cloned().collect())
}
fn groups(v: &Value) -> Vec<Dimension> {
    if v.is_object() && file(v) {
        return vec![Dimension {
            path: String::new(),
            keys: BTreeSet::new(),
        }];
    }
    let mut out = Vec::new();
    fn walk(v: &Value, path: &str, out: &mut Vec<Dimension>) {
        if let Some(keys) = group(v) {
            out.push(Dimension {
                path: path.into(),
                keys,
            });
        } else if let Some(o) = v.as_object() {
            for (k, val) in o {
                walk(val, &child(path, k), out);
            }
        }
    }
    if let Some(o) = v.as_object() {
        for (k, val) in o {
            walk(val, &child("", k), &mut out);
        }
    }
    out
}
fn version_path(v: &Value) -> Option<String> {
    ["version", "tag_name", "release", "build", "id", "name"]
        .iter()
        .find(|k| v.get(**k).is_some_and(|v| v.is_string() || v.is_number()))
        .map(|k| child("", k))
}

#[derive(Default)]
pub(super) struct Discovery {
    indexes: BTreeMap<(String, String), BTreeSet<String>>,
    collections: BTreeMap<(String, String), Options>,
}
#[derive(Default)]
struct Options {
    groups: BTreeMap<String, BTreeSet<String>>,
    channels: BTreeSet<String>,
}
impl Discovery {
    fn release(&mut self, path: &str, identity: &str, row: &Value) -> Result<()> {
        let descriptors = groups(row);
        if descriptors.is_empty() {
            return Ok(());
        }
        let opts = self
            .collections
            .entry((path.into(), identity.into()))
            .or_default();
        for g in descriptors {
            let keys = opts.groups.entry(g.path).or_default();
            keys.extend(g.keys);
            check_keys(keys)?;
        }
        ensure!(opts.groups.len() <= MAX_OPTIONS, "too many artifact groups");
        if let Some(ch) = row.get("channel").and_then(Value::as_str) {
            opts.channels.insert(ch.into());
            check_keys(&opts.channels)?;
        }
        Ok(())
    }
    pub fn scan(&mut self, v: &Value, path: &str, indexes: bool) -> Result<()> {
        check_pointer(path)?;
        ensure!(
            self.indexes.len() + self.collections.len() <= MAX_OPTIONS,
            "too many manifest collections"
        );
        if let Some(rows) = v.as_array() {
            for row in rows {
                if let Some(ver) = version_path(row) {
                    self.release(path, &ver, row)?;
                }
            }
            return Ok(());
        }
        let Some(obj) = v.as_object() else {
            return Ok(());
        };
        if let Some(ver) = version_path(v)
            && !groups(v).is_empty()
        {
            return self.release(path, &ver, v);
        }
        if file(v) {
            return Ok(());
        }
        if indexes {
            let string_map = obj.values().all(Value::is_string);
            for (key, row) in obj {
                let mut offer = |field: &str, link: &str| -> Result<()> {
                    let json_link = link
                        .split(['?', '#'])
                        .next()
                        .is_some_and(|s| s.to_ascii_lowercase().ends_with(".json"));
                    let relation = matches!(
                        field,
                        "first_page" | "first_page_url" | "first" | "manifest_url"
                    );
                    if !link.is_empty()
                        && (relation
                            || json_link
                                && row.get("size").is_none()
                                && version_path(row).is_none())
                    {
                        let link_path = if field.is_empty() {
                            String::new()
                        } else {
                            child("", field)
                        };
                        let keys = self.indexes.entry((path.into(), link_path)).or_default();
                        keys.insert(key.clone());
                        check_keys(keys)?;
                    }
                    Ok(())
                };
                if string_map && let Some(link) = row.as_str() {
                    offer("", link)?;
                } else if let Some(fields) = row.as_object() {
                    for (field, value) in fields {
                        if let Some(link) = value.as_str() {
                            offer(field, link)?;
                        }
                    }
                }
            }
        }
        if !obj.is_empty()
            && obj
                .values()
                .all(|r| r.is_object() && !file(r) && !groups(r).is_empty())
        {
            for row in obj.values() {
                let ver = version_path(row).unwrap_or_default();
                self.release(path, &ver, row)?;
            }
            return Ok(());
        }
        for (k, val) in obj {
            if val.is_object() || val.is_array() {
                self.scan(val, &child(path, k), indexes)?;
            }
        }
        Ok(())
    }
    pub fn json(self) -> Value {
        let indexes = self
            .indexes
            .into_iter()
            .map(|((path, link_path), keys)| Index {
                path,
                link_path,
                keys,
            })
            .collect::<Vec<_>>();
        let collections = self
            .collections
            .into_iter()
            .map(|((path, version_path), opt)| {
                let groups = opt
                    .groups
                    .into_iter()
                    .map(|(path, keys)| Dimension { path, keys })
                    .collect::<Vec<_>>();
                let mut collection =
                    json!({ "path":path, "version_path":version_path, "groups":groups });
                if !opt.channels.is_empty() {
                    collection["channels"] = json!(Dimension {
                        path: "/channel".into(),
                        keys: opt.channels
                    });
                }
                collection
            })
            .collect::<Vec<_>>();
        json!({"indexes":indexes,"collections":collections})
    }
}

#[derive(Default)]
pub(super) struct Matches {
    groups: BTreeSet<(String, Option<String>)>,
    channels: BTreeSet<String>,
}
impl Matches {
    pub fn complete(&self, selection: &Selection) -> Result<()> {
        if let Some(ch) = &selection.channels {
            ensure!(
                ch.keys.is_subset(&self.channels),
                "a selected channel is missing from the collection"
            );
        }
        for g in &selection.groups {
            if g.keys.is_empty() {
                ensure!(
                    self.groups.contains(&(g.path.clone(), None)),
                    "selected group {} is missing",
                    g.path
                );
            }
            for k in &g.keys {
                ensure!(
                    self.groups.contains(&(g.path.clone(), Some(k.clone()))),
                    "selected key {k} is missing from {}",
                    g.path
                );
            }
        }
        Ok(())
    }
}
impl Selection {
    pub fn parse(
        &self,
        root: &Value,
        base: Option<&Url>,
        branch: Option<&str>,
        seen: &mut Matches,
        out: &mut Catalog,
    ) -> Result<u64> {
        let collection = root
            .pointer(&self.path)
            .context("selected release collection is missing")?;
        let mut row = |key: Option<&str>, r: &Value| -> Result<()> {
            if let Some(ch) = &self.channels {
                let value = r
                    .pointer(&ch.path)
                    .and_then(Value::as_str)
                    .context("release channel is missing")?;
                if !ch.keys.contains(value) {
                    return Ok(());
                }
                seen.channels.insert(value.into());
            }
            let id = if self.version_path.is_empty() {
                key.context("selected release needs an object key")?.into()
            } else {
                match r.pointer(&self.version_path) {
                    Some(Value::String(v)) => v.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => anyhow::bail!("selected release version is missing"),
                }
            };
            adapter::segment(&id)?;
            let mut artifacts = Vec::new();
            for g in &self.groups {
                let Some(items) = r.pointer(&g.path).filter(|v| !v.is_null()) else {
                    continue;
                };
                if g.keys.is_empty() {
                    // Reject a changed array/descriptor that became a keyed map.
                    ensure!(
                        items.is_array() || file(items),
                        "selected unkeyed group {} changed shape",
                        g.path
                    );
                    seen.groups.insert((g.path.clone(), None));
                    artifacts.push((None, items));
                } else {
                    let map = items
                        .as_object()
                        .context("selected artifact group is no longer a keyed object")?;
                    for k in &g.keys {
                        if let Some(v) = map.get(k) {
                            seen.groups.insert((g.path.clone(), Some(k.clone())));
                            artifacts.push((Some(k.as_str()), v));
                        }
                    }
                }
            }
            if artifacts.is_empty() {
                return Ok(());
            }
            let mut metadata: Map<_, _> = r
                .as_object()
                .context("release is not an object")?
                .iter()
                .filter(|(key, v)| {
                    !v.is_object() && !v.is_array() && self.version_path != child("", key)
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if let Some(channel) = branch {
                metadata.entry("channel").or_insert(json!(channel));
            }
            match adapter::selected_release(&id, &metadata, artifacts.into_iter(), base) {
                Ok(release) => adapter::merge(out, release)?,
                Err(e) => {
                    out.reject(id, format!("{e:#}"));
                }
            }
            Ok(())
        };
        match collection {
            Value::Array(a) => {
                for r in a {
                    row(None, r)?;
                }
                Ok(a.len() as u64)
            }
            Value::Object(_)
                if !self.version_path.is_empty()
                    && collection
                        .pointer(&self.version_path)
                        .is_some_and(|v| v.is_string() || v.is_number()) =>
            {
                row(None, collection)?;
                Ok(1)
            }
            Value::Object(o) => {
                for (k, r) in o {
                    row(Some(k), r)?;
                }
                Ok(o.len() as u64)
            }
            _ => anyhow::bail!("selected path no longer contains a release collection"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiscoverInput {
    url: Option<String>,
    upload: Option<String>,
    index: Option<Index>,
}

async fn discover(
    State(state): State<AppState>,
    Path(runtime): Path<i64>,
    Json(input): Json<DiscoverInput>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM available_runtimes WHERE id=?)")
            .bind(runtime)
            .fetch_one(&state.db)
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":"Database operation failed"})),
                )
            })?;
    if !exists {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"Runtime not found"})),
        ));
    }
    feed::discover(
        &state,
        input.url.as_deref(),
        input.upload,
        input.index.as_ref(),
    )
    .await
    .map(Json)
    .map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":format!("{e:#}")})),
        )
    })
}
pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/api/admin/runtimes/{id}/discover", post(discover))
}
