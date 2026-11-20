-- Exact response bytes are shared across observations, including repeated checks.
CREATE TABLE raw_documents(digest TEXT PRIMARY KEY, body BLOB NOT NULL);
CREATE TABLE source_observations(id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES sources(id), job_id INTEGER NOT NULL REFERENCES jobs(id), created_at INTEGER NOT NULL DEFAULT(unixepoch()), page INTEGER NOT NULL, url TEXT, digest TEXT NOT NULL REFERENCES raw_documents(digest), etag TEXT, last_modified TEXT);
CREATE INDEX observations_by_source ON source_observations(source_id,job_id,page);
CREATE TABLE publication_sources(publication_id INTEGER NOT NULL REFERENCES publications(id), observation_id INTEGER NOT NULL REFERENCES source_observations(id), PRIMARY KEY(publication_id,observation_id));
ALTER TABLE transfers ADD COLUMN release TEXT;
ALTER TABLE transfers ADD COLUMN source_url TEXT;
ALTER TABLE transfers ADD COLUMN expected_digest TEXT;
ALTER TABLE transfers ADD COLUMN expected_size INTEGER;
ALTER TABLE transfers ADD COLUMN validator TEXT;
ALTER TABLE transfers ADD COLUMN effective_url TEXT;
ALTER TABLE transfers ADD COLUMN digest TEXT;
CREATE UNIQUE INDEX transfer_identity ON transfers(job_id,release,filename);
ALTER TABLE source_observations ADD COLUMN request_url TEXT;
ALTER TABLE source_observations ADD COLUMN next_link TEXT;
CREATE INDEX observation_cache ON source_observations(source_id,request_url,id DESC);
CREATE TABLE observation_artifacts(observation_id INTEGER NOT NULL REFERENCES source_observations(id), release TEXT NOT NULL, filename TEXT NOT NULL, source_url TEXT NOT NULL, metadata TEXT NOT NULL, PRIMARY KEY(observation_id,release,filename));
CREATE INDEX observation_artifact_identity ON observation_artifacts(release,filename,observation_id);
CREATE TABLE origin_cooldowns(host TEXT PRIMARY KEY, retry_after INTEGER NOT NULL);
