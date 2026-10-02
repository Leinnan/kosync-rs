CREATE TABLE IF NOT EXISTS publications (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    document_hash    TEXT    NOT NULL UNIQUE,
    file_name_hash   TEXT,
    title            TEXT    NOT NULL,
    authors          TEXT    NOT NULL DEFAULT '[]',
    language         TEXT,
    identifier       TEXT,
    publisher        TEXT,
    published        TEXT,
    description      TEXT,
    file_path        TEXT    NOT NULL,
    file_size        INTEGER NOT NULL,
    cover_path       TEXT,
    cover_media_type TEXT,
    thumb_path       TEXT,
    thumb_media_type TEXT,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_publications_title ON publications (title);
CREATE INDEX IF NOT EXISTS idx_publications_file_name_hash ON publications (file_name_hash);
