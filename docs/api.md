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

In the import dialog, choose **Choose release lists, channels and platforms** to inspect the document. A linked index lets you follow explicit channel keys to paginated release lists. A direct list reads releases from the current document. Select the file groups to mirror and preview their filenames before saving. Metiva persists the paths and selected keys. Future entries within those keys are collected automatically. Extend the selection explicitly for new keys. Manual mapping remains available for layouts outside discovery.

Sources merge in a stable order. Conflicting entries for the same release and filename hold that release back. Once Metiva can identify the releases in a complete input, independently verified releases can publish while another release needs attention. An unreadable source or incomplete page chain keeps the previous publication in place. The administrator can inspect original source bytes separately from the normalized and rewritten manifests.

Metiva calculates a SHA-256 content identifier for each object. It can also check upstream MD5, SHA-1, SHA-256, SHA-384 and SHA-512 metadata. For ZIP, gzip and tar-family formats that the worker recognizes, it reads and checks the archive stream without extracting members. A single unsigned document can have an upstream-shaped publication when it represents the complete published inventory. Multi-source, selected, paginated and signed inputs publish the normalized format and retain their original bytes.

## Retention and corrections

The normalized manifest includes releases and files retained from earlier syncs. Removing a source or narrowing a selection stops future collection from it. Its verified files keep their URLs and existing access controls. A change in verified bytes for the same runtime, release identifier and filename updates that URL. Only an administrator can retrieve the earlier bytes from the file timeline.

To withdraw a runtime, park it. Restore it to resume serving the same URLs with the existing visibility and key scopes. A runtime parked individually stays parked across a park and restore of its parent project.
