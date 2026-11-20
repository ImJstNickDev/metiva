-- Freeze the independently accepted pages before any transfer can consume them.
CREATE TABLE job_plans(job_id INTEGER PRIMARY KEY REFERENCES jobs(id), previous_publication INTEGER REFERENCES publications(id), observation_ids TEXT NOT NULL);
-- Older in-flight jobs cannot prove which input produced an already-downloaded hashless
-- object. A fresh job can safely resume validated partials and keeps the old job's audit trail.
CREATE TEMP TABLE legacy_jobs AS SELECT id,runtime_id FROM jobs WHERE state IN('queued','running') AND EXISTS(SELECT 1 FROM transfers WHERE job_id=jobs.id);
UPDATE jobs SET state='failed',finished_at=unixepoch(),error='Restarted with immutable input tracking' WHERE id IN(SELECT id FROM legacy_jobs);
UPDATE transfers SET state='cancelled',command=NULL,retry_at=NULL,speed=0 WHERE job_id IN(SELECT id FROM legacy_jobs) AND state NOT IN('failed','cancelled','completed');
INSERT OR IGNORE INTO jobs(runtime_id) SELECT runtime_id FROM legacy_jobs;
DROP TABLE legacy_jobs;
