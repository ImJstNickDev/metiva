ALTER TABLE projects ADD COLUMN parked_at INTEGER;
ALTER TABLE runtimes ADD COLUMN parked_at INTEGER;
CREATE VIEW available_runtimes AS
 SELECT r.* FROM runtimes r JOIN projects p ON p.id=r.project_id
 WHERE r.parked_at IS NULL AND p.parked_at IS NULL;
