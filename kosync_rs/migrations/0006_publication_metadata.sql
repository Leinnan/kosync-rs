-- Rich metadata for OPDS grouping and enrichment: embedded genres
-- (dc:subject), a normalized ISBN for external lookups, and series info
-- (calibre:series / EPUB 3 collections). `genres` follows the `authors`
-- pattern: a JSON array string.

ALTER TABLE publications ADD COLUMN genres TEXT NOT NULL DEFAULT '[]';
ALTER TABLE publications ADD COLUMN isbn TEXT;
ALTER TABLE publications ADD COLUMN series TEXT;
ALTER TABLE publications ADD COLUMN series_index REAL;

CREATE INDEX IF NOT EXISTS idx_publications_isbn ON publications (isbn);
CREATE INDEX IF NOT EXISTS idx_publications_series ON publications (series);
