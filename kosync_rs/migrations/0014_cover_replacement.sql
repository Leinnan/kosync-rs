-- Replacing a book's cover rewrites its EPUB, which changes the KOReader
-- partial MD5 digest. Every digest a publication was ever stored under stays
-- here, so readers holding an older copy (KOReader caches the digest next to
-- the book) still sync against the same publication and progress.
CREATE TABLE publication_aliases (
    hash           TEXT    PRIMARY KEY,
    publication_id INTEGER NOT NULL REFERENCES publications (id) ON DELETE CASCADE,
    created_at     INTEGER NOT NULL
);

CREATE INDEX idx_publication_aliases_publication ON publication_aliases (publication_id);

-- The account that uploaded a book, which may replace its cover. NULL for
-- books uploaded before this was recorded; only administrators can edit those.
ALTER TABLE publications ADD COLUMN uploaded_by INTEGER REFERENCES users (id) ON DELETE SET NULL;
