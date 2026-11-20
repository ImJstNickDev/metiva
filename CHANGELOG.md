# Changelog

## v0.1.3

- Mirror every matching release in a nested catalogue. New selections include the whole history by default. Existing saved release limits keep their values.
- Preview up to five matching records and see the full selection count before saving. A missing file or unavailable child appears beside the affected sample.
- Resume interrupted nested discovery from the source documents already captured for the job. Sync history shows how many documents the worker has read before downloads begin.

## v0.1.2

- Follow linked JSON documents within a release. Inspect a sample at each level, select which references to open and which files to mirror, and filter the releases before fetching child documents.
- Preserve hierarchical resource names in the normalized manifest. Verify declared sizes and digests for child documents and fetch a shared child document once per sync. An interrupted sync can replay its accepted graph from the original document bytes.
- Reuse equivalent parsed artifact inventories across checks. Historical observations, source bytes and publication references survive the upgrade.
- Deploy prebuilt images with a standalone `compose-prod.yaml`. The ingress publishes host port 8088, configurable through `METIVA_PORT`.
