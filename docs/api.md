# Consumer API

Fetch `GET /api/v1/openapi.json` from the running service for the machine-readable contract. Dashboard requests use a separate session and CSRF token under `/api/admin/`.

| Request | Result |
| --- | --- |
| `GET /api/v1/projects` | Projects visible to the caller |
| `GET /api/v1/projects/{project}/runtimes` | Visible runtimes within a project |
| `GET /api/v1/projects/{project}/runtimes/{runtime}/availability` | Publication availability and last successful sync |
| `GET /api/v1/projects/{project}/runtimes/{runtime}/releases` | Published releases |
| `GET /api/v1/projects/{project}/runtimes/{runtime}/releases/{release}/files` | Artifact metadata |
| `GET /p/{project}/r/{runtime}/manifest.json` | Current normalized publication |
| `GET /p/{project}/r/{runtime}/upstream.json` | Rewritten compatible source, when available |
| `GET /p/{project}/r/{runtime}/files/{release}/{artifact}` | Artifact bytes, with HTTP Range support |

Public projects can contain private runtimes. A private project restricts every runtime within it. List endpoints filter by effective visibility and credential scope. A private resource requires a valid bearer key. Missing, invalid and revoked credentials return 401. A valid key outside its scope receives 403.

Parking a project takes every child runtime offline. Parking one runtime leaves its siblings available. Consumer discovery, manifests, release metadata and byte-range requests all respect parking, including requests with a global key.

A global key includes future projects. A project key includes future runtimes within that project. A runtime key covers just the selected runtime. Each key remains valid until revoked. Store the key as a secret in the consuming service.

## A source feed

```json
{
  "releases": [
    {
      "version": "winter-01",
      "artifacts": [
        {
          "filename": "bundle.bin",
          "url": "https://downloads.example.org/builds/bundle.bin"
        }
      ]
    }
  ]
}
```

Add `size` in bytes and `sha256` in hexadecimal when provided by the upstream. Versions are source identifiers. Relative artifact references resolve against the final page URL. Uploads require absolute URLs. The runtime's normalized manifest uses mirror URLs and includes calculated SHA-256 and actual byte length.

For a different source layout, specify `releases_path`, `version_path`, `artifacts_path`, `url_path`, `filename_path`, `size_path`, `sha256_path` or `next_path`. Release and artifact paths are relative to their respective records. Dot-separated paths and JSON Pointer are supported.

The Generic JSON adapter accepts up to 128 pages per source, with an 8 MiB page limit and a 32 MiB aggregate limit. The dashboard accepts uploads within its 5 MiB request-body limit. Pagination can follow a next-page field or HTTP Link header. The worker validates each new destination and redirect against the outbound network policy.

In the import dialog, choose **Choose release lists, channels and platforms** to inspect the document. A linked index lets you follow explicit channel keys to paginated release lists. A direct list reads releases from the current document. Select the file groups to mirror and preview their filenames before saving. Metiva persists the paths and selected keys. Future entries within those keys are collected automatically. Extend the selection explicitly for new keys. Linked release lists may span multiple pages. The root link index must fit in a single document. Manual mapping remains available for layouts outside discovery.

Sources merge in a stable order. Conflicting entries for the same release and filename hold that release back. Once Metiva can identify the releases in a complete input, independently verified releases can publish while another release needs attention. An unreadable source or incomplete page chain keeps the previous publication in place. The administrator can inspect original source bytes separately from the normalized and rewritten manifests.

Metiva calculates a SHA-256 content identifier for each object. It can also check upstream MD5, SHA-1, SHA-256, SHA-384 and SHA-512 metadata. For ZIP, gzip and tar-family formats that the worker recognizes, it reads and checks the archive stream without extracting members. A single unsigned document can have an upstream-shaped publication when it represents the complete published inventory. Multi-source, selected, paginated and signed inputs publish the normalized format and retain their original bytes.

## Retention and corrections

The normalized manifest includes releases and files retained from earlier syncs. Removing a source or narrowing a selection stops future collection from it. Its verified files keep their URLs and existing access controls. A change in verified bytes for the same runtime, release identifier and filename updates that URL. Only an administrator can retrieve the earlier bytes from the file timeline.

To withdraw a runtime, park it. Restore it to resume serving the same URLs with the existing visibility and key scopes. A runtime parked individually stays parked across a park and restore of its parent project.

## Nested documents

Under **Import source**, inspect a document, choose its release collection, then inspect one matching record. Filter by a scalar field such as a channel or ID. Leave the sync limit blank for all matches, or set a maximum after filtering. An explicit limit in an existing source stays in effect after upgrading. Arrays use upstream order and keyed collections use lexicographic key order. In the sample, choose a reference to mirror as a file or open it as a document and select its children. A JSON file can serve either role.

A nested source stores these choices in `mapping.nested`. For example, the following selection follows the descriptor of each matching release, mirrors the portable package, and follows a second document containing content-addressed resources.

```json
{
  "nested": {
    "path": "/records",
    "version_path": "/version",
    "filters": [{"path": "/track", "values": ["stable"]}],
    "limit": 5,
    "rules": [{
      "kind": "document", "path": "", "url_path": "/url",
      "rules": [
        {"kind": "file", "path": "/packages/portable"},
        {"kind": "document", "path": "/inventory", "rules": [{
          "kind": "file", "path": "/objects/*", "name_path": "@key",
          "digest_path": "/hash", "algorithm": "sha1", "size_path": "/size",
          "url_template": "https://downloads.example.org/objects/{prefix}/{digest}"
        }]}
      ]
    }]
  }
}
```

Pointers are relative to the current document or release record. A `*` segment selects members of an array or object. For an object, `@key` gives the member's original key. The template substitutes the verified-format hexadecimal digest and its first two characters. Metiva checks the resolved destination on each fetch, including redirects, and verifies the bytes against the expected digest.

Set `required` to `false` to allow an absent branch in layouts that vary by release. An advertised child that fails to load or verify still holds back its release. A shared child can contribute files to several releases. Nested sources publish the normalized manifest. Its optional `logical_path` field preserves directory-qualified resource names alongside URL-safe mirror filenames. Exact input documents are available in the runtime's publication history.

The nested release collection must fit in one root document. An advertised next page or incomplete page count stops the sync before any child requests. A linked document that advertises additional pages through an HTTP Link header or pagination metadata holds back its release. For paginated release lists, use the release-list picker or a manual pagination mapping.

Preview counts the selected root records and opens at most five of them. Its file counts and sizes describe the sample. The saved mapping applies to the entire selection, including records outside the preview. Already published files stay available when a version moves outside an explicit limit.

A nested sync holds the root inventory fixed and traverses releases in order. Each release can use up to 128 documents including the root, up to 32 MiB of document bytes, and eight rule levels. Each document must fit within 8 MiB. The preview shares a single 128-document, 32 MiB budget across its sample. The worker retains original documents on disk so a restart can resume the same job with the root and children already observed, even if the upstream changes during the interruption.

A source can select up to 100,000 records and resolve up to 100,000 file references per run. A synchronization also caps discovery at 100,000 distinct document requests and 4 GiB of document bytes per nested source. These safeguards apply to manifest processing. Artifact bodies stream through the download manager with their own size limits.
