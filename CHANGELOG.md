# Changelog

## v0.1.2

- Follow linked JSON documents within a release. Inspect a sample at each level, select which references to open and which files to mirror, and filter the releases before fetching child documents.
- Preserve hierarchical resource names in the normalized manifest. Verify declared sizes and digests for child documents and fetch a shared child document once per sync. An interrupted sync can replay its accepted graph from the original document bytes.
- Reuse equivalent parsed artifact inventories across checks. Historical observations, source bytes and publication references survive the upgrade.
- Deploy prebuilt images with a standalone `compose-prod.yaml`. The ingress publishes host port 8088, configurable through `METIVA_PORT`.
