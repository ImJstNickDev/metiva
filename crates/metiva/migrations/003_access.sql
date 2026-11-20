CREATE TABLE admin_credential (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    username TEXT NOT NULL,
    password_hash TEXT NOT NULL
);
CREATE TABLE admin_sessions (
    digest TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL
);
CREATE INDEX admin_sessions_expiry ON admin_sessions(expires_at);
CREATE TABLE auth_attempts (
    purpose TEXT PRIMARY KEY,
    since INTEGER NOT NULL,
    attempts INTEGER NOT NULL
);
CREATE TABLE api_keys (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    prefix TEXT NOT NULL,
    digest TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL CHECK(scope IN ('global','project','runtime')),
    project_id INTEGER REFERENCES projects(id) ON DELETE CASCADE,
    runtime_id INTEGER REFERENCES runtimes(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT(unixepoch()),
    last_used_at INTEGER,
    revoked_at INTEGER,
    CHECK((scope='global' AND project_id IS NULL AND runtime_id IS NULL)
       OR (scope='project' AND project_id IS NOT NULL AND runtime_id IS NULL)
       OR (scope='runtime' AND project_id IS NOT NULL AND runtime_id IS NOT NULL))
);
