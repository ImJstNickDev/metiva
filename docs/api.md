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

Sources merge in a stable order. Conflicting entries at the same release and filename stop the publication. The administrator can inspect original source bytes separately from the normalized and rewritten manifests.

Metiva calculates a SHA-256 content identifier for each object. It can also check upstream MD5, SHA-1, SHA-256, SHA-384 and SHA-512 metadata. For ZIP, gzip and tar-family formats that the worker recognizes, it reads and checks the archive stream without extracting members. Single unsigned documents can have an upstream-shaped publication. Multi-source, paginated and signed inputs publish the normalized format and retain their original bytes.
