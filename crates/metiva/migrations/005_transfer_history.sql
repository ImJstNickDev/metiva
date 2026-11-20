CREATE TABLE transfer_events (
    id INTEGER PRIMARY KEY,
    transfer_id INTEGER NOT NULL REFERENCES transfers(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT(unixepoch()),
    state TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    retries INTEGER NOT NULL,
    error TEXT
);
CREATE INDEX events_by_transfer ON transfer_events(transfer_id,id DESC);

-- Record the known state on upgrade. Earlier transitions cannot be reconstructed.
INSERT INTO transfer_events(transfer_id,state,bytes,retries,error)
SELECT id,state,bytes,retries,error FROM transfers;

CREATE TRIGGER transfer_created AFTER INSERT ON transfers
BEGIN
    INSERT INTO transfer_events(transfer_id,state,bytes,retries,error)
    VALUES(NEW.id,NEW.state,NEW.bytes,NEW.retries,NEW.error);
END;

CREATE TRIGGER transfer_changed AFTER UPDATE OF state ON transfers
WHEN OLD.state <> NEW.state
BEGIN
    INSERT INTO transfer_events(transfer_id,state,bytes,retries,error)
    VALUES(NEW.id,NEW.state,NEW.bytes,NEW.retries,NEW.error);
END;
