CREATE TABLE IF NOT EXISTS progress_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    document_hash TEXT    NOT NULL,
    progress      TEXT    NOT NULL,
    percentage    REAL    NOT NULL,
    device        TEXT    NOT NULL,
    device_id     TEXT,
    timestamp     INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_progress_events_user_doc
    ON progress_events (user_id, document_hash, timestamp);

INSERT INTO progress_events (user_id, document_hash, progress, percentage, device, device_id, timestamp)
SELECT user_id, document_hash, progress, percentage, device, device_id, timestamp FROM documents;
