-- Track a stable digest of the *original* uploaded bytes so that re-uploading
-- the same source file (e.g. a PDF that converts to a non-deterministic EPUB)
-- is still detected as a duplicate. For direct EPUB uploads this equals
-- document_hash; for PDF uploads it is the digest of the source PDF.

ALTER TABLE publications ADD COLUMN source_hash TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_publications_source_hash
    ON publications (source_hash) WHERE source_hash IS NOT NULL;
