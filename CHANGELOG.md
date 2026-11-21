# Changelog

## v0.1.6

- Open **API reference** in the dashboard or visit `/docs/` for a first-download guide and links to each consumer API operation. The reference reads the OpenAPI document served by your installation.
- Idle dashboard pages now detect a lost connection. An orderly shutdown announces itself before the API closes its connections. A lightweight heartbeat covers crashes and silent network interruptions. The dashboard checks the session and refreshes data when service returns.

## v0.1.5

- Keep the last dashboard view visible through an outage. Metiva pauses actions, checks the session and refreshes data when the server returns. A previously loaded dashboard can reopen its reconnecting screen during downtime. Private API responses stay out of the offline cache.
- Open Active or Queued transfers directly from Overview. The selected filter follows live changes in transfer state.
- Return to the top of long lists with a keyboard-accessible scroll control that respects reduced-motion preferences.
- Read long URLs in publication history without scrolling sideways. The JSON keeps its original indentation when copied.
- Start standalone Compose with automatic subnet allocation or an explicit `METIVA_SUBNET`. For upgrades using an explicit subnet, follow the [network migration instructions](https://github.com/ImJstNickDev/metiva/blob/v0.1.5/docs/deployment.md#upgrade).
- Pin the release images using `image-digests.txt` from the release assets. The same ingress and service images are available on GHCR and Docker Hub.

## v0.1.4

- Let long-running syncs finish writing inventory while the scheduler checks for work. This prevents large syncs from stalling on SQLite writer contention.

## v0.1.3

- Mirror every matching release in a nested catalogue. New selections include the whole history by default. Existing saved release limits keep their values.
- Preview up to five matching records and see the full selection count before saving. A missing file or unavailable child appears beside the affected sample.
- Resume interrupted nested discovery from the source documents already captured for the job. Sync history shows how many documents the worker has read before downloads begin.

## v0.1.2

- Follow linked JSON documents within a release. Inspect a sample at each level, select which references to open and which files to mirror, and filter the releases before fetching child documents.
- Preserve hierarchical resource names in the normalized manifest. Verify declared sizes and digests for child documents and fetch a shared child document once per sync. An interrupted sync can replay its accepted graph from the original document bytes.
- Reuse equivalent parsed artifact inventories across checks. Historical observations, source bytes and publication references survive the upgrade.
- Deploy prebuilt images with a standalone `compose-prod.yaml`. The ingress publishes host port 8088, configurable through `METIVA_PORT`.
