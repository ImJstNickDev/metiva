-- Keep observations as the identity of checks, HTTP validators and snapshots.
-- Each successfully parsed page can point at an immutable set of artifact rows.
CREATE TABLE parsed_inventories(
    id INTEGER PRIMARY KEY,
    digest BLOB NOT NULL UNIQUE
);
ALTER TABLE source_observations ADD COLUMN inventory_id INTEGER REFERENCES parsed_inventories(id);
CREATE INDEX observations_by_inventory ON source_observations(inventory_id) WHERE inventory_id IS NOT NULL;

CREATE TABLE inventory_artifacts(
    inventory_id INTEGER NOT NULL REFERENCES parsed_inventories(id),
    release TEXT NOT NULL,
    filename TEXT NOT NULL,
    source_url TEXT NOT NULL,
    metadata TEXT NOT NULL,
    PRIMARY KEY(inventory_id,release,filename)
) WITHOUT ROWID;
CREATE INDEX inventory_artifact_identity ON inventory_artifacts(release,filename);

-- Startup migrates these rows under the database migration lock using the same
-- ordered SHA-256 input as the worker. Raw responses may have been parsed with
-- different mappings or effective URLs, so their digest cannot identify a set.
ALTER TABLE observation_artifacts RENAME TO legacy_observation_artifacts;
CREATE VIEW observation_artifacts AS
    SELECT o.id AS observation_id,a.release,a.filename,a.source_url,a.metadata
    FROM source_observations o JOIN inventory_artifacts a ON a.inventory_id=o.inventory_id;
