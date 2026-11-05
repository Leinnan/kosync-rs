-- Track the digest of the file a converted book was uploaded as. Conversion
-- output can differ between runs and converter versions, so re-uploads of the
-- same source can only be recognized by this digest. NULL for books stored as
-- uploaded, whose document_hash already identifies the source.

ALTER TABLE publications ADD COLUMN source_hash TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_publications_source_hash
    ON publications (source_hash) WHERE source_hash IS NOT NULL;
