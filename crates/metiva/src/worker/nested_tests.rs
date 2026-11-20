use super::*;
use serde_json::json;

struct Fixture {
    pages: BTreeMap<String, Value>,
    requests: Vec<String>,
}
impl Fixture {
    fn new(pages: impl IntoIterator<Item = (&'static str, Value)>) -> Self {
        Self {
            pages: pages.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
            requests: Vec::new(),
        }
    }
    async fn run(&mut self, rules: Value) -> Result<Catalog> {
        let rules = serde_json::from_value(rules)?;
        let base = Url::parse("https://example.org/root")?;
        Ok(execute(&rules, Some(&base), self, false).await?.0)
    }
}
impl Documents for Fixture {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        let url = url.unwrap();
        self.requests.push(url.path().to_owned());
        Ok(self
            .pages
            .get(url.path())
            .map(|v| {
                Document::new(
                    serde_json::to_vec(v).unwrap(),
                    Some(url.clone()),
                    Some(self.requests.len() as i64),
                )
            })
            .ok_or_else(|| "upstream returned 404".to_owned()))
    }
}

#[tokio::test]
async fn filtered_multilevel_graph_reuses_documents_and_keeps_explicit_json_leaves() {
    let hash = "00112233445566778899aabbccddeeff00112233";
    let directory = json!({"objects":{"dir/a.json":{"hash":hash,"size":3},"another/a.json":{"hash":hash,"size":3}}});
    let bytes = serde_json::to_vec(&directory).unwrap();
    let checksum = hex::encode(sha1::Sha1::digest(&bytes));
    let branch = json!({"files":[{"artifact":{"url":"leaf.json"}},{"elsewhere":{"url":"ignored"}}],
        "index":{"url":"../shared","sha1":checksum,"size":bytes.len()}});
    let mut fixture = Fixture::new([
        (
            "/root",
            json!({"ordered":[{"v":9,"type":"other","url":"/skip"},
            {"v":5,"type":"candidate","flag":true,"url":"docs/five"},
            {"v":3,"type":"candidate","flag":true,"url":"docs/three"},
            {"v":8,"type":"candidate","flag":true,"url":"/beyond-limit"}]}),
        ),
        ("/docs/five", branch.clone()),
        ("/docs/three", branch),
        ("/shared", directory),
    ]);
    let out=fixture.run(json!({"path":"/ordered","version_path":"/v","filters":[{"path":"/type","values":["candidate"]},{"path":"/flag","values":["true"]}],"limit":2,
        "rules":[{"path":"","kind":"document","rules":[{"path":"/files/*/artifact","required":false,"kind":"file"},
            {"path":"/index","kind":"document","rules":[{"path":"/objects/*","kind":"file","name_path":"@key","digest_path":"/hash","algorithm":"sha1","url_template":"https://files.example/{prefix}/{digest}"}]}]}]})).await.unwrap();
    assert!(out.release_errors.is_empty());
    assert_eq!(
        fixture.requests,
        ["/root", "/docs/five", "/shared", "/docs/three"]
    );
    assert_eq!(out.assets.len(), 6);
    assert_eq!(
        out.assets[&("5".into(), "leaf.json".into())].url.path(),
        "/docs/leaf.json"
    );
    let files = out
        .assets
        .iter()
        .filter(|((v, n), _)| v == "5" && n.ends_with("~a.json"))
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 2);
    assert_ne!(files[0].0, files[1].0);
    for (_, file) in files {
        assert_eq!(file.hashes["sha1"], hash);
        assert_eq!(
            file.url.as_str(),
            format!("https://files.example/00/{hash}")
        );
        assert_eq!(file.size, Some(3));
    }
}

#[tokio::test]
async fn each_parent_edge_checks_integrity_even_after_a_cache_hit() {
    let child = json!({"leaf":{"url":"good.bin"}});
    let bytes = serde_json::to_vec(&child).unwrap();
    let sha = hex::encode(Sha256::digest(&bytes));
    let rules = json!({"path":"","rules":[{"path":"","kind":"document","rules":[{"path":"/leaf","kind":"file"}]}]});
    let mut fixture = Fixture::new([
        (
            "/root",
            json!({
        "a-good":{"url":"shared","sha256":sha},"b-hash":{"url":"shared","sha256":"1".repeat(64)},
        "c-size":{"url":"shared","size":999},"d-missing":{"url":"absent"},"e-cycle":{"url":"root"},
        "f-malformed":{"url":"shared","sha1":"wrong"},"g-shape":{"url":"empty"},
        "h-good":{"url":"shared","hashes":{"sha256":sha}}}),
        ),
        ("/shared", child),
        ("/empty", json!({})),
    ]);
    let out = fixture.run(rules).await.unwrap();
    assert_eq!(
        out.versions.keys().collect::<Vec<_>>(),
        ["a-good", "h-good"]
    );
    assert_eq!(out.release_errors.len(), 6);
    for (v, why) in [
        ("b-hash", "sha256 mismatch"),
        ("c-size", "size mismatch"),
        ("d-missing", "404"),
        ("e-cycle", "cycle"),
        ("f-malformed", "digest"),
        ("g-shape", "required field"),
    ] {
        assert!(
            out.release_errors[v].contains(why),
            "{}",
            out.release_errors[v]
        );
    }
    assert_eq!(fixture.requests, ["/root", "/shared", "/absent", "/empty"]);
}

#[tokio::test]
async fn map_keys_are_filtered_and_limited_in_stable_order() {
    let mut fixture = Fixture::new([(
        "/root",
        json!({"b":{"file":["bbb"]},"z":{"file":["zzz"]},"a":{"file":["aaa"]}}),
    )]);
    let out = fixture
        .run(
            json!({"path":"","filters":[{"path":"@key","values":["z","a"]}],"limit":1,
        "rules":[{"path":"/file/*","kind":"file"}]}),
        )
        .await
        .unwrap();
    assert_eq!(out.versions.keys().collect::<Vec<_>>(), ["a"]);
    assert_eq!(out.assets[&("a".into(), "aaa".into())].url.path(), "/aaa");
}

#[tokio::test]
async fn bounded_graph_and_unsafe_references_fail_closed() {
    let children: Vec<_> = (0..128).map(|n| format!("/{n}")).collect();
    let mut fixture = Fixture::new([
        (
            "/root",
            json!({"rows":[
                {"id":"huge","children":children},{"id":"small","children":["/good"]}
            ]}),
        ),
        ("/good", json!({"leaf":"keep.bin"})),
    ]);
    for url in children {
        fixture.pages.insert(url, json!({"leaf":"file.bin"}));
    }
    let selection = json!({"path":"/rows","version_path":"/id","rules":[
        {"path":"/children/*","kind":"document","rules":[{"path":"/leaf","kind":"file"}]}]});
    let out = fixture.run(selection.clone()).await.unwrap();
    assert!(out.release_errors["huge"].contains("document limit"));
    assert_eq!(
        out.assets.keys().collect::<Vec<_>>(),
        [&("small".to_owned(), "keep.bin".to_owned())]
    );
    assert_eq!(fixture.requests.len(), 129);
    assert!(!fixture.requests.iter().any(|path| path == "/127"));

    fixture.requests.clear();
    let base = Url::parse("https://example.org/root").unwrap();
    let error = execute(
        &serde_json::from_value(selection).unwrap(),
        Some(&base),
        &mut fixture,
        true,
    )
    .await
    .err()
    .unwrap();
    assert!(error.is::<GraphError>());
    assert!(format!("{error:#}").contains("nested preview exceeds document limit"));
    assert_eq!(fixture.requests.len(), 128);
    let rule = json!({"path":"/rows","version_path":"/id","rules":[{"path":"","kind":"document","rules":[{"path":"/leaf","kind":"file"}]}]});
    let mut fixture = Fixture::new([(
        "/root",
        json!({"rows":[{"id":"bad","url":"http://127.0.0.1/document"}]}),
    )]);
    let out = fixture.run(rule).await.unwrap();
    assert!(out.release_errors.contains_key("bad"));
    assert_eq!(fixture.requests, ["/root"]);
}

// Generate each child when requested, as a remote origin would. The fixture
// itself holds just the root, one padding string, and the request paths.
struct History {
    root: Value,
    padding: String,
    requests: Vec<String>,
    bytes: usize,
}

impl Documents for History {
    async fn load(&mut self, url: Option<&Url>) -> Result<Result<Document, String>> {
        let url = url.unwrap();
        self.requests.push(url.path().to_owned());
        let bytes = if url.path() == "/root" {
            serde_json::to_vec(&self.root)?
        } else {
            ensure!(
                url.path().ends_with("/manifest.json"),
                "unexpected file request"
            );
            serde_json::to_vec(&json!({"downloads":{"client":{"url":"client.jar"}},
                "description": self.padding, "id":url.path()}))?
        };
        self.bytes += bytes.len();
        Ok(Ok(Document::new(bytes, Some(url.clone()), None)))
    }
}

#[tokio::test]
async fn full_history_and_filtered_previews_have_independent_request_counts() {
    let base = Url::parse("https://example.org/root").unwrap();
    let rows: Vec<_> = (0..925)
        .map(|i| {
            json!({
                "id":format!("v{i}"), "type":if i % 2 == 1 {"release"} else {"snapshot"},
                "url":format!("/versions/{i}/manifest.json")
            })
        })
        .collect();
    let mut history = History {
        root: json!({"versions":rows}),
        padding: "x".repeat(40 << 10),
        requests: Vec::new(),
        bytes: 0,
    };
    let mut selection = json!({"path":"/versions","version_path":"/id","rules":[
        {"path":"","kind":"document","rules":[{"path":"/downloads/client","kind":"file"}]}]});
    let (out, _, counts) = execute(
        &serde_json::from_value(selection.clone()).unwrap(),
        Some(&base),
        &mut history,
        false,
    )
    .await
    .unwrap();
    assert_eq!(counts.selected, 925);
    assert!(out.release_errors.is_empty());
    assert_eq!(out.assets.len(), 925);
    assert_eq!(
        out.assets[&("v924".into(), "client.jar".into())].url.path(),
        "/versions/924/client.jar"
    );
    assert_eq!(history.requests.len(), 926);
    assert!(history.bytes > super::super::feed::SOURCE_BYTES);

    selection["filters"] = json!([{"path":"/type","values":["release"]}]);
    for (limit, selected, previewed) in [(None, 462, 5), (Some(9), 9, 5), (Some(2), 2, 2)] {
        selection["limit"] = json!(limit);
        history.requests.clear();
        let (out, _, counts) = execute(
            &serde_json::from_value(selection.clone()).unwrap(),
            Some(&base),
            &mut history,
            true,
        )
        .await
        .unwrap();
        assert_eq!((counts.selected, counts.previewed), (selected, previewed));
        assert_eq!(out.assets.len(), previewed);
        let mut expected = vec!["/root".to_owned()];
        expected.extend((0..previewed).map(|i| format!("/versions/{}/manifest.json", 2 * i + 1)));
        assert_eq!(history.requests, expected);
    }
}

#[tokio::test]
async fn counting_preview_rows_includes_failed_and_empty_releases() {
    let base = Url::parse("https://example.org/root").unwrap();
    let mut fixture = Fixture::new([
        (
            "/root",
            json!({"a":"missing","b":"empty","c":"empty",
        "d":"missing","e":"missing","f":"beyond-sample","g":"beyond-sample"}),
        ),
        ("/empty", json!({})),
    ]);
    let rule = serde_json::from_value(json!({"path":"","rules":[{"path":"","kind":"document",
        "rules":[{"path":"/leaf","kind":"file","required":false}]}]}))
    .unwrap();
    let (out, _, counts) = execute(&rule, Some(&base), &mut fixture, true)
        .await
        .unwrap();
    assert_eq!((counts.selected, counts.previewed), (7, 5));
    assert!(out.assets.is_empty());
    assert_eq!(
        out.release_errors.keys().collect::<Vec<_>>(),
        ["a", "d", "e"]
    );
    assert_eq!(fixture.requests, ["/root", "/missing", "/empty"]);

    let rule = serde_json::from_value(
        json!({"path":"","filters":[{"path":"@key","values":["unmatched"]}],
        "rules":[{"path":"","kind":"file"}]}),
    )
    .unwrap();
    let (_, _, counts) = execute(&rule, Some(&base), &mut fixture, true)
        .await
        .unwrap();
    assert_eq!((counts.selected, counts.previewed), (0, 0));
}

#[tokio::test]
async fn child_cache_releases_raw_and_parsed_documents_at_release_boundaries() {
    let base = Url::parse("https://example.org/root").unwrap();
    let mut history = History {
        root: Value::Null,
        padding: "x".repeat(3 << 20),
        requests: Vec::new(),
        bytes: 0,
    };
    let mut graph = Graph::new(&mut history, false);
    let mut documents = Vec::new();
    for i in 0..11 {
        graph.begin_release(100);
        let doc = graph
            .load(&base.join(&format!("{i}/manifest.json")).unwrap())
            .await
            .unwrap();
        doc.linked_json().unwrap();
        documents.push(Arc::downgrade(&doc));
        assert!(documents.iter().filter(|d| d.strong_count() > 0).count() <= 3);
    }
    // Reuse of a hot document still consumes this release's byte allowance.
    graph.begin_release(100);
    for i in 0..10 {
        graph
            .load(&base.join(&format!("{i}/manifest.json")).unwrap())
            .await
            .unwrap();
    }
    let error = graph
        .load(&base.join("10/manifest.json").unwrap())
        .await
        .err()
        .unwrap();
    assert!(!error.is::<GraphError>());
    assert!(format!("{error:#}").contains("byte limit"));
}

#[test]
fn tree_and_digest_rules_validate_before_fetch() {
    let mut branch = json!({"path":"","kind":"file"});
    for _ in 0..8 {
        branch = json!({"path":"","kind":"document","rules":[branch]});
    }
    for rule in [
        json!({"path":"","rules":[branch]}),
        json!({"path":"","rules":[]}),
        json!({"path":"relative","rules":[{"path":"","kind":"file"}]}),
        json!({"path":"","rules":[{"path":"","kind":"file","url_template":"https://host/fixed"}]}),
        json!({"path":"","rules":[{"path":"","kind":"file","rules":[{"path":"","kind":"file"}]}]}),
        json!({"path":"","limit":0,"rules":[{"path":"","kind":"file"}]}),
    ] {
        assert!(
            serde_json::from_value::<Nested>(rule)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let checksum = hex::encode(sha1::Sha1::digest(b"abc"));
    let rule: Rule = serde_json::from_value(
        json!({"path":"","kind":"file","digest_path":"/digest","algorithm":"sha1"}),
    )
    .unwrap();
    assert_eq!(
        Integrity::from(&json!({"digest":checksum}), Some(&rule))
            .unwrap()
            .hashes["sha1"],
        checksum
    );
    assert!(verify_inspection(b"abc", &json!({"size":"3","checksums":{"sha1":checksum}})).is_ok());
    assert!(verify_inspection(b"abcd", &json!({"size":"3"})).is_err());
}

#[test]
fn encoded_logical_names_are_stable_and_disjoint_from_plain_names() {
    let (encoded, meta) = filename("components/a.jar").unwrap();
    assert_eq!(meta["logical_path"], "components/a.jar");
    assert!(encoded.ends_with("~a.jar"));
    assert_ne!(encoded, filename(&encoded).unwrap().0);
    assert_ne!(encoded, filename("other/a.jar").unwrap().0);
    assert_eq!(filename("core.jar").unwrap().0, "core.jar");
    assert!(filename("parent/").is_err());
    let (long, _) = filename(&format!("x/{}.zip", "🗂".repeat(300))).unwrap();
    assert!(long.len() <= 255 && long.ends_with(".zip"));
}

#[tokio::test]
async fn paginated_roots_are_refused_before_following_children() {
    let selection = json!({"path":"/records","version_path":"/id","limit":1,
        "rules":[{"path":"","kind":"document","rules":[{"path":"/leaf","kind":"file"}]}]});
    for markers in [
        json!({"next_page":"next.json"}),
        json!({"pagination":{"pages":2}}),
        json!({"has_more":true}),
        json!({"count":2}),
        json!({"total_count":9}),
    ] {
        let mut root = markers;
        root["records"] = json!([{"url":"/child", "id":"first"}]);
        let mut fixture = Fixture::new([("/root", root)]);
        let error = fixture.run(selection.clone()).await.err().unwrap();
        assert!(format!("{error:#}").contains("unpaginated"));
        assert_eq!(fixture.requests, ["/root"]);
    }
    let mut fixture = Fixture::new([
        (
            "/root",
            json!({"count":1,"records":[{"url":"/child","id":"first"}]}),
        ),
        (
            "/child",
            json!({"next":"metadata-for-a-different-purpose","leaf":"file.json"}),
        ),
    ]);
    assert_eq!(fixture.run(selection).await.unwrap().assets.len(), 1);
    // These words are ordinary identifiers when the root itself is the map.
    let mut fixture = Fixture::new([("/root", json!({"next":{"url":"file.json"}}))]);
    assert_eq!(
        fixture
            .run(json!({"path":"","rules":[{"path":"","kind":"file"}]}))
            .await
            .unwrap()
            .assets
            .len(),
        1
    );
}

#[tokio::test]
async fn incomplete_child_isolates_its_release_and_json_files_are_terminal() {
    for marker in [
        json!({"pagination":{"next_url":"after.json"}}),
        json!({"pagination":{"has_more":true}}),
        json!({"pageInfo":{"hasNextPage":true}}),
        json!({"page":1,"pages":2}),
        json!({"links":{"next":{"href":"after.json"}}}),
    ] {
        let mut incomplete = marker;
        incomplete["leaf"] = json!("bad.json");
        let mut fixture = Fixture::new([
            (
                "/root",
                json!({"one":{"url":"incomplete"},"two":{"url":"complete"}}),
            ),
            ("/incomplete", incomplete),
            (
                "/complete",
                json!({"next":"ordinary domain data","count":99,"pages":44,
                "pagination":{"next":false,"has_more":false,"pages":1,"count":8000},"leaf":"keep.json"}),
            ),
            (
                "/keep.json",
                json!({"pagination":{"next":"a-page-inside-the-file"}}),
            ),
        ]);
        let out = fixture
            .run(json!({"path":"","rules":[{"kind":"document","path":"",
            "rules":[{"kind":"file","path":"/leaf"}]}]}))
            .await
            .unwrap();
        assert!(out.release_errors["one"].contains("advertises pagination"));
        assert_eq!(
            out.assets.keys().collect::<Vec<_>>(),
            [&("two".to_owned(), "keep.json".to_owned())]
        );
        assert_eq!(fixture.requests, ["/root", "/incomplete", "/complete"]);
    }
}

#[tokio::test]
async fn wildcard_expansion_budget_is_fatal_to_the_whole_source() {
    let mut fixture = Fixture::new([(
        "/root",
        json!({"good":{"files":["keep.bin"]},
        "large":{"files":vec![0;adapter::MAX_ITEMS+1]}}),
    )]);
    let error = fixture
        .run(json!({"path":"","rules":[{"kind":"file","path":"/files/*"}]}))
        .await
        .err()
        .unwrap();
    assert!(error.is::<GraphError>());
    assert!(format!("{error:#}").contains("too many selected descriptors"));
}
