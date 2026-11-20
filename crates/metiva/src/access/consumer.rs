use super::*;
use axum::{
    extract::{Path, Request, State},
    middleware::{self, Next},
    routing::get,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{FromRow, Row};

const PROJECTS: &str = "/api/v1/projects";
const RUNTIMES: &str = "/api/v1/projects/{project}/runtimes";
const RELEASES: &str = "/api/v1/projects/{project}/runtimes/{runtime}/releases";
const FILES: &str = "/api/v1/projects/{project}/runtimes/{runtime}/releases/{release}/files";
const AVAILABILITY: &str = "/api/v1/projects/{project}/runtimes/{runtime}/availability";
const MANIFEST: &str = "/p/{project}/r/{runtime}/manifest.json";
const UPSTREAM: &str = "/p/{project}/r/{runtime}/upstream.json";
const DOWNLOAD: &str = "/p/{project}/r/{runtime}/files/{release}/{artifact}";
const SPEC: &str = "/api/v1/openapi.json";

pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/api/admin/archive/files/{id}", get(archive))
        .route_layer(middleware::from_fn_with_state(state, require_admin))
        .merge(
            Router::new()
                .route(PROJECTS, get(projects))
                .route(RUNTIMES, get(runtimes))
                .route(RELEASES, get(releases))
                .route(FILES, get(files))
                .route(AVAILABILITY, get(availability))
                .route(MANIFEST, get(manifest))
                .route(UPSTREAM, get(upstream))
                .route(DOWNLOAD, get(download))
                .route_layer(middleware::from_fn(consumer_headers)),
        )
        .route(SPEC, get(|| async { Json(openapi()) }))
}

async fn consumer_headers(req: Request, next: Next) -> Response {
    let mut response = next.run(req).await;
    let unauthorized = response.status() == StatusCode::UNAUTHORIZED;
    let h = response.headers_mut();
    h.insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    h.append(header::VARY, "Authorization".parse().unwrap());
    if unauthorized {
        h.insert(
            header::WWW_AUTHENTICATE,
            "Bearer realm=\"metiva\"".parse().unwrap(),
        );
    }
    response
}

#[derive(FromRow)]
struct Grant {
    scope: String,
    project_id: Option<i64>,
    runtime_id: Option<i64>,
}

struct Reader(Option<Grant>);

impl Reader {
    async fn from_request(state: &AppState, h: &HeaderMap) -> Result<Self> {
        let Some(raw) = bearer(h)? else {
            return Ok(Self(None));
        };
        let grant=sqlx::query_as(
            "UPDATE api_keys SET last_used_at=unixepoch() WHERE digest=? AND revoked_at IS NULL RETURNING scope,project_id,runtime_id")
            .bind(digest(raw)).fetch_optional(&state.db).await?.ok_or(Error(StatusCode::UNAUTHORIZED,"Invalid bearer credential"))?;
        Ok(Self(Some(grant)))
    }
    fn allows(&self, project: i64, runtime: Option<i64>, public: bool) -> bool {
        public
            || self.0.as_ref().is_some_and(|g| {
                g.scope == "global"
                    || (g.project_id == Some(project)
                        && (g.scope == "project"
                            || (g.scope == "runtime"
                                && (runtime.is_none() || g.runtime_id == runtime))))
            })
    }
    fn check(&self, project: i64, runtime: Option<i64>, public: bool) -> Result<()> {
        if self.allows(project, runtime, public) {
            Ok(())
        } else if self.0.is_some() {
            Err(Error(
                StatusCode::FORBIDDEN,
                "Key does not permit access to this resource",
            ))
        } else {
            Err(Error(
                StatusCode::UNAUTHORIZED,
                "Bearer credential required",
            ))
        }
    }
}

#[derive(FromRow, Serialize)]
struct Project {
    #[serde(skip)]
    id: i64,
    name: String,
    slug: String,
    description: String,
    #[serde(skip)]
    visibility: String,
}

async fn projects(State(state): State<AppState>, h: HeaderMap) -> Result<Json<Vec<Project>>> {
    let reader = Reader::from_request(&state, &h).await?;
    let mut list: Vec<Project> =
        sqlx::query_as("SELECT id,name,slug,description,visibility FROM projects ORDER BY slug")
            .fetch_all(&state.db)
            .await?;
    list.retain(|p| reader.allows(p.id, None, p.visibility == "public"));
    Ok(Json(list))
}

#[derive(FromRow, Serialize)]
struct Runtime {
    #[serde(skip)]
    id: i64,
    #[serde(skip)]
    project_id: i64,
    #[serde(skip)]
    public: bool,
    #[serde(skip)]
    active_publication: Option<i64>,
    name: String,
    slug: String,
    last_success: Option<i64>,
}

macro_rules! runtime_query {
    ($filter:literal) => { concat!(
        "SELECT r.id,r.project_id,(p.visibility='public' AND r.visibility IN ('inherit','public')) public,
        r.active_publication,r.name,r.slug,r.last_success FROM runtimes r JOIN projects p ON p.id=r.project_id WHERE ", $filter
    ) };
}

async fn runtimes(
    State(state): State<AppState>,
    Path(project): Path<String>,
    h: HeaderMap,
) -> Result<Json<Vec<Runtime>>> {
    let reader = Reader::from_request(&state, &h).await?;
    let (id, visibility): (i64, String) =
        sqlx::query_as("SELECT id,visibility FROM projects WHERE slug=?")
            .bind(project)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(missing)?;
    reader.check(id, None, visibility == "public")?;
    let mut list: Vec<Runtime> = sqlx::query_as(runtime_query!("r.project_id=? ORDER BY r.slug"))
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    list.retain(|r| reader.allows(r.project_id, Some(r.id), r.public));
    Ok(Json(list))
}

async fn authorized_runtime(
    state: &AppState,
    h: &HeaderMap,
    project: &str,
    runtime: &str,
) -> Result<Runtime> {
    let reader = Reader::from_request(state, h).await?;
    let r: Runtime = sqlx::query_as(runtime_query!("p.slug=? AND r.slug=?"))
        .bind(project)
        .bind(runtime)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(missing)?;
    reader.check(r.project_id, Some(r.id), r.public)?;
    Ok(r)
}

async fn publication_json(state: &AppState, r: Runtime, original: bool) -> Result<Json<Value>> {
    let sql = if original {
        "SELECT upstream FROM publications WHERE id=? AND runtime_id=?"
    } else {
        "SELECT manifest FROM publications WHERE id=? AND runtime_id=?"
    };
    let s: Option<String> = sqlx::query_scalar(sql)
        .bind(r.active_publication)
        .bind(r.id)
        .fetch_optional(&state.db)
        .await?
        .flatten();
    let body = serde_json::from_str(&s.ok_or_else(missing)?).map_err(|_| {
        Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored manifest is invalid",
        )
    })?;
    Ok(Json(body))
}

async fn manifest(
    State(state): State<AppState>,
    Path((p, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    publication_json(&state, authorized_runtime(&state, &h, &p, &r).await?, false).await
}
async fn upstream(
    State(state): State<AppState>,
    Path((p, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    publication_json(&state, authorized_runtime(&state, &h, &p, &r).await?, true).await
}

async fn releases(
    State(state): State<AppState>,
    Path((p, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    let active = authorized_runtime(&state, &h, &p, &r).await?;
    let Json(body) = publication_json(&state, active, false).await?;
    let releases = body.get("releases").filter(|v| v.is_array()).ok_or(Error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Stored manifest is invalid",
    ))?;
    Ok(Json(releases.clone()))
}

async fn files(
    State(state): State<AppState>,
    Path((p, r, release)): Path<(String, String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    let active = authorized_runtime(&state, &h, &p, &r).await?;
    let Json(body) = publication_json(&state, active, false).await?;
    let files = body["releases"]
        .as_array()
        .and_then(|releases| {
            releases
                .iter()
                .find(|v| v["version"].as_str() == Some(&release))
        })
        .and_then(|v| v.get("artifacts"))
        .filter(|v| v.is_array())
        .ok_or_else(missing)?;
    Ok(Json(files.clone()))
}

async fn availability(
    State(state): State<AppState>,
    Path((p, r)): Path<(String, String)>,
    h: HeaderMap,
) -> Result<Json<Value>> {
    let active = authorized_runtime(&state, &h, &p, &r).await?;
    Ok(Json(
        json!({"available":active.active_publication.is_some(),"last_success":active.last_success}),
    ))
}

fn attachment(digest: String, filename: String) -> Result<Response> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(Error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Stored artifact identifier is invalid",
        ));
    }
    Ok((
        [
            ("x-accel-redirect", format!("/_objects/{digest}")),
            ("x-accel-expires", "0".to_owned()),
            ("content-type", "application/octet-stream".to_owned()),
            (
                "content-disposition",
                format!(
                    "attachment; filename*=UTF-8''{}",
                    utf8_percent_encode(&filename, NON_ALPHANUMERIC)
                ),
            ),
        ],
        (),
    )
        .into_response())
}

async fn download(
    State(state): State<AppState>,
    Path((p, r, release, artifact)): Path<(String, String, String, String)>,
    h: HeaderMap,
) -> Result<Response> {
    let active = authorized_runtime(&state, &h, &p, &r).await?;
    let row = sqlx::query(
        "SELECT filename,digest FROM files WHERE publication_id=? AND release=? AND filename=?",
    )
    .bind(active.active_publication)
    .bind(release)
    .bind(artifact)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(missing)?;
    attachment(row.get("digest"), row.get("filename"))
}

async fn archive(State(state): State<AppState>, Path(id): Path<i64>) -> Result<Response> {
    let row = sqlx::query("SELECT filename,digest FROM files WHERE id=?")
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(missing)?;
    attachment(row.get("digest"), row.get("filename"))
}

fn reference(name: &str) -> Value {
    json!({"$ref":format!("#/components/schemas/{name}")})
}
fn array(name: &str) -> Value {
    json!({"type":"array","items":reference(name)})
}
fn object(fields: &[(&str, Value)]) -> Value {
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(name, v)| (name.to_string(), v.clone()))
        .collect();
    let required: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    json!({"type":"object","properties":properties,"required":required})
}

/// The router and its public contract use the same route constants.
pub fn openapi() -> Value {
    let string = json!({"type":"string"});
    let integer = json!({"type":"integer","format":"int64"});
    let timestamp = json!({"type":["integer","null"],"description":"Unix timestamp in seconds"});
    let mut schemas = Map::new();
    schemas.insert(
        "Project".into(),
        object(&[
            ("slug", string.clone()),
            ("name", string.clone()),
            ("description", string.clone()),
        ]),
    );
    schemas.insert(
        "Runtime".into(),
        object(&[
            ("slug", string.clone()),
            ("name", string.clone()),
            ("last_success", timestamp.clone()),
        ]),
    );
    schemas.insert(
        "Artifact".into(),
        object(&[
            ("filename", string.clone()),
            ("size", integer.clone()),
            ("url", json!({"type":"string","format":"uri"})),
            (
                "sha256",
                json!({"type":"string","pattern":"^[0-9a-f]{64}$"}),
            ),
        ]),
    );
    schemas.insert(
        "Release".into(),
        object(&[
            ("version", string.clone()),
            ("artifacts", array("Artifact")),
        ]),
    );
    schemas.insert(
        "Manifest".into(),
        object(&[
            ("schema_version", json!({"type":"integer","const":1})),
            ("project", string.clone()),
            ("runtime", string.clone()),
            ("releases", array("Release")),
        ]),
    );
    schemas.insert(
        "Availability".into(),
        object(&[
            ("available", json!({"type":"boolean"})),
            ("last_success", timestamp),
        ]),
    );
    schemas.insert("Error".into(), object(&[("error", string)]));
    let mut paths = Map::new();
    for (path, summary, schema) in [
        (PROJECTS, "List readable projects", array("Project")),
        (RUNTIMES, "List readable project runtimes", array("Runtime")),
        (RELEASES, "List published releases", array("Release")),
        (
            FILES,
            "List artifacts in one published release",
            array("Artifact"),
        ),
        (
            AVAILABILITY,
            "Read runtime publication availability",
            reference("Availability"),
        ),
        (
            MANIFEST,
            "Read the current normalized manifest",
            reference("Manifest"),
        ),
        (
            UPSTREAM,
            "Read the current upstream-shaped manifest with mirror URLs",
            json!({}),
        ),
        (
            DOWNLOAD,
            "Download a published artifact",
            json!({"type":"string","format":"binary"}),
        ),
        (
            SPEC,
            "Read this OpenAPI specification",
            json!({"type":"object"}),
        ),
    ] {
        let params = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .map(|name| {
                json!({
                    "name":name,"in":"path","required":true,"schema":{"type":"string"}
                })
            })
            .collect::<Vec<_>>();
        let mime = if path == DOWNLOAD {
            "application/octet-stream"
        } else {
            "application/json"
        };
        let mut responses =
            json!({"200":{"description":"Success","content":{(mime):{"schema":schema}}}});
        if path != SPEC {
            for (code, message) in [
                ("401", "Missing or invalid bearer key"),
                ("403", "Key scope denies access"),
                ("404", "Resource or published representation unavailable"),
            ] {
                if path == PROJECTS && code != "401" {
                    continue;
                }
                responses[code] = json!({
                    "description":message,
                    "content":{"application/json":{"schema":reference("Error")}}
                });
            }
        }
        responses["429"] = json!({"description":"Request rate limit reached"});
        let mut parameters = params;
        if path == DOWNLOAD {
            let metadata = json!({
                "Accept-Ranges":{"schema":{"type":"string","example":"bytes"}},
                "ETag":{"schema":{"type":"string"}},
                "Last-Modified":{"schema":{"type":"string"}},
                "Content-Length":{"schema":{"type":"integer","minimum":0}}
            });
            responses["200"]["headers"] = metadata.clone();
            responses["206"] = json!({"description":"Partial file content","headers":metadata,
                "content":{(mime):{"schema":schema}}});
            responses["206"]["headers"]["Content-Range"] = json!({"schema":{"type":"string"}});
            responses["304"] =
                json!({"description":"The conditional request matched the stored artifact"});
            responses["416"] = json!({"description":"Requested range is unsatisfiable"});
            for (name, description) in [
                ("Range", "Requested byte ranges, for example bytes=0-1023"),
                (
                    "If-Range",
                    "Apply Range when this validator matches the current artifact",
                ),
                (
                    "If-None-Match",
                    "Return 304 when the current entity tag matches",
                ),
                (
                    "If-Modified-Since",
                    "Return 304 when the artifact has not been modified since this date",
                ),
            ] {
                parameters.push(json!({"name":name,"in":"header","description":description,"schema":{"type":"string"}}));
            }
        }
        let mut head_responses = responses.clone();
        for response in head_responses.as_object_mut().unwrap().values_mut() {
            response.as_object_mut().unwrap().remove("content");
        }
        paths.insert(
            path.into(),
            json!({"parameters":parameters,"get":{"summary":summary,"responses":responses},
            "head":{"summary":format!("Read headers for {summary}"),"responses":head_responses}}),
        );
    }
    json!({"openapi":"3.1.0","info":{"title":"Metiva Consumer API","version":env!("CARGO_PKG_VERSION"),
        "description":"Discover published content anonymously or use a read-only bearer key for private content. Project and runtime scopes restrict private reads. An invalid bearer key returns 401."},
        "security":[{},{"bearerKey":[]}],"paths":paths,
        "components":{"securitySchemes":{"bearerKey":{"type":"http","scheme":"bearer","description":"Read-only mtv_ key sent in the Authorization header"}},"schemas":schemas}})
}
