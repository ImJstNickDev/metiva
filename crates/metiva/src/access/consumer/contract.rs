use super::{
    AVAILABILITY, DOWNLOAD, FILES, MANIFEST, PROJECTS, RELEASES, RUNTIMES, SPEC, UPSTREAM,
};
use serde_json::{Map, Value, json};

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
    schemas["Artifact"]["properties"]["logical_path"] = json!({"type":"string","description":"Original logical name when the mirror uses an encoded filename"});
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
    let artifact = json!({
        "filename":"bundle.bin","size":6,
        "url":"https://mirror.example.org/p/toolkit/r/portable/files/winter-01/bundle.bin",
        "sha256":"5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
    });
    let release = json!({"version":"winter-01","artifacts":[artifact]});
    let mut paths = Map::new();
    for (path, id, tag, summary, description, schema, example) in [
        (
            PROJECTS,
            "listProjects",
            "Discovery",
            "List readable projects",
            "Returns projects available to the caller, ordered by slug. An empty list means no projects are visible.",
            array("Project"),
            Some(json!([{"slug":"toolkit","name":"Toolkit","description":"Portable tools"}])),
        ),
        (
            RUNTIMES,
            "listRuntimes",
            "Discovery",
            "List project runtimes",
            "Use the project slug returned by discovery. The result includes only runtimes readable with the current credentials.",
            array("Runtime"),
            Some(json!([{"slug":"portable","name":"Portable","last_success":1767225600}])),
        ),
        (
            AVAILABILITY,
            "getAvailability",
            "Discovery",
            "Check publication availability",
            "Read whether the runtime has a current publication and the Unix time of its last successful sync. An available runtime can continue serving its previous publication during a source outage.",
            reference("Availability"),
            Some(json!({"available":true,"last_success":1767225600})),
        ),
        (
            MANIFEST,
            "getManifest",
            "Publications",
            "Read the normalized manifest",
            "Returns the current published releases and verified artifacts, including retained files. Each artifact has an absolute mirror URL, byte count and SHA-256 digest.",
            reference("Manifest"),
            Some(
                json!({"schema_version":1,"project":"toolkit","runtime":"portable","releases":[release]}),
            ),
        ),
        (
            UPSTREAM,
            "getUpstreamManifest",
            "Publications",
            "Read the upstream-shaped manifest",
            "Read a compatible source document with its artifact URLs rewritten. This representation is available only when one source document can describe the complete publication. Its fields depend on that source.",
            json!({}),
            Some(
                json!({"releases":[{"version":"winter-01","artifacts":[{"filename":"bundle.bin","url":artifact["url"]}]}]}),
            ),
        ),
        (
            RELEASES,
            "listReleases",
            "Publications",
            "List published releases",
            "Each release includes its original version identifier and published artifacts. Version identifiers are strings and can use any upstream versioning convention.",
            array("Release"),
            Some(json!([release])),
        ),
        (
            FILES,
            "listArtifacts",
            "Artifacts",
            "List files in a release",
            "Read filenames, sizes, checksums and mirror URLs for one release. Use a returned URL to download the corresponding bytes.",
            array("Artifact"),
            Some(json!([artifact])),
        ),
        (
            DOWNLOAD,
            "downloadArtifact",
            "Artifacts",
            "Download an artifact",
            "Use the URL from the publication. Byte-range and conditional requests use the same visibility and key scope as a full download.",
            json!({"type":"string","format":"binary"}),
            None,
        ),
        (
            SPEC,
            "getOpenApi",
            "Specification",
            "Read the OpenAPI document",
            "Read the machine-readable consumer API contract served by this installation.",
            json!({"type":"object"}),
            None,
        ),
    ] {
        let params = [
            ("project", "Project slug from the project list.", "toolkit"),
            ("runtime", "Runtime slug within that project.", "portable"),
            (
                "release",
                "Original version identifier from the publication.",
                "winter-01",
            ),
            (
                "artifact",
                "Published filename from the artifact list.",
                "bundle.bin",
            ),
        ]
        .into_iter()
        .filter(|(name, _, _)| path.contains(&format!("{{{name}}}")))
        .map(|(name, description, example)| {
            json!({
                "name":name,"in":"path","required":true,"description":description,
                "schema":{"type":"string"},"example":example
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
        if let Some(example) = example {
            responses["200"]["content"][mime]["example"] = example;
        }
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
        let head_id = format!("head{}{}", id[..1].to_ascii_uppercase(), &id[1..]);
        let mut operations = json!({
            "parameters":parameters,
            "get":{"operationId":id,"tags":[tag],"summary":summary,"description":description,"responses":responses},
            "head":{"operationId":head_id,"tags":[tag],"summary":format!("{summary} (headers only)"),
                "description":"Returns the status and headers of the corresponding GET request with an empty body.",
                "responses":head_responses}
        });
        if path == SPEC {
            operations["get"]["security"] = json!([]);
            operations["head"]["security"] = json!([]);
        }
        paths.insert(path.into(), operations);
    }
    json!({"openapi":"3.1.0","info":{"title":"Metiva Consumer API","version":env!("CARGO_PKG_VERSION"),
        "description":"Discover published content anonymously or use a read-only bearer key for private content. Project and runtime scopes restrict private reads. An invalid bearer key returns 401."},
        "security":[{},{"bearerKey":[]}],"paths":paths,
        "components":{"securitySchemes":{"bearerKey":{"type":"http","scheme":"bearer","description":"Read-only mtv_ key sent in the Authorization header"}},"schemas":schemas}})
}
