-- Retire a feed while retaining the source identity attached to historical observations.
ALTER TABLE sources ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1));
CREATE INDEX active_sources ON sources(runtime_id,id) WHERE enabled=1;
