# Operating a mirror

## Storage and process ownership

Set `METIVA_DATA_DIR` to a local filesystem on the Docker host. The directory holds the database, its WAL and shared-memory files, the object store, temporary downloads and the initial bootstrap secret. Keep staging and artifacts on one filesystem for atomic promotion. Keep the parent private to the deployment account. Allow the ingress worker to read completed objects and traverse its read-only `artifacts` mount.

The API initializes the schema before the worker starts. A shared migration lock also serializes concurrent command-line starts. Both processes use the same numeric UID and GID and enable foreign keys, full synchronous commits, WAL and a 15-second busy timeout. A filesystem lock admits one worker per data directory. A unique partial index admits one queued or running sync per runtime. Artifact bodies stay outside SQLite. The database contains original source bytes, release records and publication membership.

The backend bridge defaults to `10.243.62.0/28`. Set `METIVA_BACKEND_SUBNET` to a free local subnet if it overlaps another route. The bridge permits outbound source inspection and downloads. Only the ingress joins `prod-frontend`. Configure the external reverse proxy for HTTPS and disable response caching on mirror and API paths. The API uses the configured origin to construct URLs. It does not derive trust or scheme from forwarded headers.

## Back up and restore

Stop the three Metiva services and take a consistent backup of the entire data directory, preserving ownership and permissions. Include `.env` separately. Restart with `docker compose up -d` after the backup completes. Treat the backup as private. It includes administrator credential verifiers, sessions and preserved private content.

Restore into an empty local data directory at the configured path with the same UID/GID. Start the matching tagged code first and check `docker compose ps`, the active publication and an authenticated download before upgrading.

## Upgrade

Back up the deployment, check out the desired release and run `docker compose up -d --build`. The API applies embedded, ordered schema migrations before reporting healthy. To roll back an incompatible migration, restore both the matching backup and the previous release.

## Administrator recovery

From the deployment host, run:

```sh
docker compose exec metiva-api metiva recover
```

Enter the new password twice at the protected prompt. Recovery invalidates existing dashboard sessions. Consumer keys can be rotated independently in **Access Keys**. Rotating creates a replacement with the same scope. Revoke the original after updating your consumers.

## Inspect a failed sync

Use the runtime's sync history and source status to identify the failed request or artifact. Inspect the transfer for size, checksum and retry information. A failed preparation leaves the previous publication active. Disk usage grows as new artifacts arrive. Reserve space for active downloads, retained content and backups.

Set `METIVA_CONNECT_TIMEOUT_SECS` for the connection budget, `METIVA_IDLE_TIMEOUT_SECS` for idle reads and `METIVA_MAX_ARTIFACT_BYTES` for the maximum transfer size. Defaults are 15 seconds, 45 seconds and 32 GiB. Set these values in the deployment's `.env` and recreate the API and worker. Configure engine, concurrency, retries and scheduling in **Settings**.
