ALTER TABLE jobs ADD COLUMN verified_releases INTEGER;
ALTER TABLE jobs ADD COLUMN failed_releases INTEGER;
CREATE INDEX publications_by_runtime ON publications(runtime_id,id DESC);

-- Only a change in verified bytes creates a revision. Snapshot copies share the
-- revision that introduced those bytes, even if source metadata later changes.
CREATE TABLE file_revisions(
    file_id INTEGER PRIMARY KEY REFERENCES files(id),
    runtime_id INTEGER NOT NULL REFERENCES runtimes(id),
    release TEXT NOT NULL,
    filename TEXT NOT NULL,
    job_id INTEGER REFERENCES jobs(id)
);
CREATE INDEX file_revision_history ON file_revisions(runtime_id,release,filename,file_id DESC);

-- Seed history for currently available identities. Never add old snapshot
-- members to the active publication during an upgrade.
WITH history AS (
    SELECT f.id,p.runtime_id,f.release,f.filename,f.digest,
           lag(f.digest) OVER (PARTITION BY p.runtime_id,f.release,f.filename ORDER BY p.id) prior_digest
    FROM files f JOIN publications p ON p.id=f.publication_id
    JOIN runtimes r ON r.id=p.runtime_id
    WHERE EXISTS (
        SELECT 1 FROM files active WHERE active.publication_id=r.active_publication
        AND active.release=f.release AND active.filename=f.filename
    )
)
INSERT INTO file_revisions(file_id,runtime_id,release,filename)
SELECT id,runtime_id,release,filename FROM history WHERE digest IS NOT prior_digest;

-- Parking and download callbacks run on separate database connections. Once a
-- job is cancelled, a late callback must not revive or complete its transfers.
CREATE TRIGGER keep_cancelled_transfer BEFORE UPDATE ON transfers
WHEN OLD.state='cancelled' AND NEW.state!='cancelled'
 AND EXISTS(SELECT 1 FROM jobs WHERE id=OLD.job_id AND state='cancelled')
BEGIN
    SELECT RAISE(IGNORE);
END;
