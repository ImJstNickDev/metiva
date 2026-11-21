# Operating a mirror

## Standalone production deployment

Download `compose-prod.yaml` from the release assets and follow the [quick start](../README.md#run-a-mirror). Compose creates a project bridge. Only the ingress publishes a host port (`8088:80` by default). The API and worker share the bridge and can connect to upstream sources. Docker assigns its subnet. If the host has exhausted its default address pools, set `METIVA_SUBNET` to a free local CIDR before startup. Bind mounts are relative to the Compose file unless you set an absolute `METIVA_DATA_DIR`.

Local administration at `http://localhost:8088` uses the browser's localhost exception for secure cookies. For remote administration use an HTTPS reverse proxy and set `METIVA_PUBLIC_URL` to the exact origin shown in the browser. A reverse proxy on the same host can connect to a loopback-only binding. A proxy on another host or bridge needs a reachable host address. Set `METIVA_PORT` and `METIVA_BIND_ADDRESS` in `.env` to match your routing.

During an outage an open dashboard keeps its last snapshot in memory and pauses actions until it can validate the session and refresh the page. After a previous visit over HTTPS or localhost, browsers with service workers can also reload the dashboard shell while the stack is down. A reload clears the private snapshot. The offline cache contains only static dashboard files.

## Storage and process ownership

Set `METIVA_DATA_DIR` to a local filesystem on the Docker host. The directory holds the database, its WAL and shared-memory files, the object store, temporary downloads and the initial bootstrap secret. Keep staging and artifacts on one filesystem for atomic promotion. Keep the parent private to the deployment account. Allow the ingress worker to read completed objects and traverse its read-only `artifacts` mount.

Start all three services with `up -d --wait`. A targeted `up metiva` starts only Nginx. In both Compose files, the API starts after Nginx so it can send a final dashboard event through the ingress during `compose down`. The API waits up to 25 seconds for in-flight responses to finish.

The API initializes the schema before the worker starts. A shared migration lock also serializes concurrent command-line starts. Both processes use the same numeric UID and GID and enable foreign keys, full synchronous commits, WAL and a 15-second busy timeout. A filesystem lock admits one worker per data directory. A unique partial index admits one queued or running sync per runtime. Artifact bodies stay outside SQLite. The database contains original source bytes, release records and publication membership.

For the source-build `compose.yaml`, the backend bridge defaults to `10.243.62.0/28`. Set `METIVA_BACKEND_SUBNET` to a free local subnet if it overlaps another route. The bridge permits outbound source inspection and downloads. Only the ingress joins `prod-frontend`. Configure the external reverse proxy for HTTPS and disable response caching on mirror and API paths. The API uses the configured origin to construct URLs. It does not derive trust or scheme from forwarded headers.

## Back up and restore

Stop the three Metiva services and take a consistent backup of the entire data directory, preserving ownership and permissions. Include `.env` separately. After a standalone backup, restart with `docker compose -f compose-prod.yaml up -d --wait`. For a source deployment, use `docker compose up -d --wait`. Treat the backup as private. It includes administrator credential verifiers, sessions and preserved private content.

Restore into an empty local data directory at the configured path with the same UID/GID. Start the matching tagged code first and check service health with `docker compose -f compose-prod.yaml ps` (or `docker compose ps` for source deployments), the active publication and an authenticated download before upgrading.

## Upgrade

Back up the deployment. For a standalone install, replace the production Compose file with the one from the desired release, run `docker compose -f compose-prod.yaml pull`, then `docker compose -f compose-prod.yaml up -d --wait`. Keep your existing data directory, origin and ownership settings. For source builds, check out the desired release and run `docker compose up -d --build`. The API applies embedded, ordered schema migrations before reporting healthy. To roll back an incompatible migration, restore both the matching backup and the previous release.

When upgrading from v0.1.4 or earlier with an explicit `METIVA_SUBNET`, stop the deployment with `docker compose -f compose-prod.yaml down` **before** replacing the old Compose file. This frees the CIDR for the new network. The bind-mounted data directory persists across this operation. Deployments using automatic subnet allocation can follow the normal upgrade procedure.

## Administrator recovery

From a standalone deployment directory, run

```sh
docker compose -f compose-prod.yaml exec metiva-api metiva recover
```

Enter the new password twice at the protected prompt. Recovery invalidates existing dashboard sessions. Consumer keys can be rotated independently in **Access Keys**. Rotating creates a replacement with the same scope. Revoke the original after updating your consumers.

## Inspect a failed sync

Use the runtime's sync history and source status to identify the failed request or artifact. Inspect the transfer for size, checksum and retry information. A failed source fetch keeps the previous publication active. If a complete input contains a bad release, the history reports verified and failed release counts. Previously served files remain available. Disk usage grows as new artifacts arrive. Reserve space for active downloads, retained content and backups.

Set `METIVA_CONNECT_TIMEOUT_SECS` for the connection budget, `METIVA_IDLE_TIMEOUT_SECS` for idle reads and `METIVA_MAX_ARTIFACT_BYTES` for the maximum transfer size. Defaults are 15 seconds, 45 seconds and 32 GiB. Set these values in the deployment's `.env` and recreate the API and worker. Configure engine, concurrency, retries and scheduling in **Settings**.

## Versioned images

For registry deployment, set these variables in `.env` along with the public origin and bind mount settings:

```dotenv
METIVA_INGRESS_IMAGE=ghcr.io/imjstnickdev/metiva
METIVA_SERVICE_IMAGE=ghcr.io/imjstnickdev/metiva-service
METIVA_IMAGE_TAG=v0.1.5
```

To pull from Docker Hub, set `METIVA_INGRESS_IMAGE=docker.io/imjstnick/metiva` and `METIVA_SERVICE_IMAGE=docker.io/imjstnick/metiva-service` with the same tag.

Run `docker compose -f compose-prod.yaml pull` followed by `docker compose -f compose-prod.yaml up -d --wait`. The API and worker use different commands from the same service image. The release image workflow targets Linux amd64. Build from the tagged source on other architectures.

The v0.1.1 migration starts each cumulative library with its active v0.1.0 publication. Earlier copies of a current file appear in its timeline. Open **Sync history → Publication history** for a previous manifest and its original source documents. Files that were already administrator-only remain accessible under **Publication → Past files**.

### Pin images by digest

Starting with v0.1.5, `image-digests.txt` in the release assets lists the two verified image digests on GHCR and Docker Hub. Its `METIVA_INGRESS_REF` and `METIVA_SERVICE_REF` assignments select the exact GHCR manifests. Copy both assignments into the deployment's `.env` next to the new `compose-prod.yaml`, then use the usual pull and start commands. These full references take precedence over repository and tag settings. Replace both pins together on each upgrade.

### Publishing a release

1. Merge the intended changes into `main` through pull requests. Prepare the package versions, production Compose defaults and the matching `## vX.Y.Z` section in `CHANGELOG.md` in a release preparation PR. That section supplies the public release notes.
2. On GitHub, run **Actions → Release → Run workflow** on `main`, with the new version as the tag. Select the publication checkbox. The CLI equivalent is `gh workflow run images.yml --ref main -f tag=vX.Y.Z -F publish=true` with the prepared version. GitHub fixes the commit at dispatch time.
3. The workflow waits for the successful `Verify` run on that exact push to `main`. It builds the two Linux amd64 candidates on GHCR, records their digests and starts and recreates all three services using those digests and the release's standalone Compose file.
4. It copies the tested digests to the version tags on GHCR and Docker Hub, checks all four references, and prepares the offline Docker archive, Compose file, image pins and SHA-256 checksums. The final job checks GitHub's stored asset digests, creates the annotated Git tag and publishes the draft. Versions in the 0.x series are marked as development pre-releases.

To exercise the build on GitHub before releasing, leave the publication checkbox clear and use the version currently declared on `main`. This stages the two candidates on GHCR under the commit SHA and tests them on a fresh runner. Version tags, Docker Hub and GitHub Releases are only updated by a publishing run.

Keep GitHub release immutability enabled in the repository settings. Set the Actions variable `DOCKERHUB_USERNAME` and secret `DOCKERHUB_TOKEN` with push access to `metiva` and `metiva-service`. For an organization namespace, also set `DOCKERHUB_NAMESPACE`. The workflow uses its job token for GHCR and the GitHub Release.

If a job fails, inspect its log and rerun the failed jobs on the original commit. The workflow reuses staged images, checks existing version tags against the tested digests, and resumes its draft. A conflicting version tag stops promotion. Corrections to a published release get a new version number.
