CREATE TABLE IF NOT EXISTS users (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    username        TEXT    NOT NULL UNIQUE,
    password_hash   TEXT    NOT NULL,
    is_active       INTEGER NOT NULL DEFAULT 1,
    is_administrator INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS documents (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    document_hash TEXT    NOT NULL,
    progress      TEXT    NOT NULL,
    percentage    REAL    NOT NULL,
    device        TEXT    NOT NULL,
    device_id     TEXT,
    timestamp     INTEGER NOT NULL,
    UNIQUE (user_id, document_hash)
);

CREATE INDEX IF NOT EXISTS idx_documents_user_id ON documents (user_id);
