-- PDF conversion was removed, so source_hash always equals document_hash.

DROP INDEX IF EXISTS idx_publications_source_hash;

ALTER TABLE publications DROP COLUMN source_hash;
