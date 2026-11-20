<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/wordmark-dark.svg">
    <img src="assets/brand/wordmark-light.svg" width="225" alt="metiva">
  </picture>
</p>
<p align="center"><strong>Manifests in. Mirrors out.</strong></p>

Metiva mirrors artifacts from JSON release feeds. Inspect a feed, start a sync, and give consumers a stable manifest on your own domain. Each runtime keeps a cumulative library. When an upstream drops a version, its verified files stay available at their existing mirror URLs.

![Metiva dashboard showing active transfers and real publication history](assets/dashboard.png)

## Run a mirror

Download the production Compose file into a new directory on a Linux amd64 host. It runs three services from two versioned images and publishes the dashboard and mirror on port **8088**. Run the commands as the account that owns the data.

```sh
mkdir metiva && cd metiva
curl --fail --location --output compose-prod.yaml \
  https://github.com/ImJstNickDev/metiva/releases/download/v0.1.4/compose-prod.yaml
install -d -m 700 data
install -d -m 755 data/artifacts
printf 'METIVA_UID=%s\nMETIVA_GID=%s\n' "$(id -u)" "$(id -g)" > .env
docker compose -f compose-prod.yaml pull
docker compose -f compose-prod.yaml up -d --wait
cat data/bootstrap-secret
```

Open `http://localhost:8088` on the host, enter the bootstrap secret and choose an administrator password. For remote access, set `METIVA_PUBLIC_URL` to the public HTTPS origin in `.env`, forward your reverse proxy to the host's HTTP port 8088, and recreate the services. Set `METIVA_PORT` to use another port or `METIVA_BIND_ADDRESS=127.0.0.1` to bind only to loopback.

For source builds and deployments on an existing external proxy network, use the repository's `compose.yaml`. [Deployment and upgrades](docs/deployment.md)

## Publish a runtime

1. In **Projects**, create a project and a runtime. Choose visibility explicitly when you want anonymous downloads.
2. Import a manifest URL or upload JSON. Select a release list, filter records and inspect a sample. Follow linked documents to the files you want, or use the linked-index picker for channel and platform maps. For a nested source, review a sample of up to five releases and its known file sizes. Leave the sync limit blank to mirror the full matching history.
3. Add the source and start synchronization. **Transfers** shows observed bytes, verification and retries.
4. Copy the published endpoint from the runtime. Consumers can fetch the manifest at `/p/{project}/r/{runtime}/manifest.json`. A compatible single feed can also provide `upstream.json`.
5. Select a filename in **Publication** for its revision timeline. A verified correction takes over its existing URL. An administrator can download earlier bytes from the timeline.
6. Park a runtime or project to take its endpoints offline. Restore it from **Archive** to serve its preserved publication again.

For a private runtime, create a read key in **Access Keys**. Select the narrowest scope your consumer needs. The dashboard shows the secret once. Supply it in an HTTP header.

```sh
curl --fail --header "Authorization: Bearer ${METIVA_KEY}" \
  "${METIVA_PUBLIC_URL}/p/atlas/r/linux/manifest.json"
```

The consumer contract is available from your instance at `/api/v1/openapi.json`. See [API and manifest formats](docs/api.md) for a sample source, mapping and authorization rules.

## Operate and develop

The API and worker share SQLite and a content-addressed artifact store. Nginx streams authorized files with Range support. Container recreation preserves bind-mounted data. [Storage, recovery and upgrades](docs/deployment.md)

Use the pinned Rust toolchain and Node.js 22 or newer.

```sh
cargo test --locked --workspace
(cd apps/web && npm ci && npm test && npm run build)
python3 tests/compose/run.py
```

The Compose test creates a temporary data directory, test origin and TLS proxy. It exercises publication, interrupted transfer recovery, archive access and credential rotation end to end. It requires Docker with Compose v2.24+ and OpenSSL.

Versioned Linux amd64 ingress and service images are published to GHCR and Docker Hub. [Deploy prebuilt images](docs/deployment.md#versioned-images) · [Changelog](CHANGELOG.md)

The [contributor guide](CONTRIBUTING.md) covers local API, worker and dashboard development.

## License

Metiva is available under the [MIT license](LICENSE).
