-- Pixel sizes of the stored cover and thumbnail, advertised in the OPDS 2.0
-- `images` collection. NULL until measured; the startup backfill fills in
-- publications imported before these columns existed.

ALTER TABLE publications ADD COLUMN cover_width INTEGER;
ALTER TABLE publications ADD COLUMN cover_height INTEGER;
ALTER TABLE publications ADD COLUMN thumb_width INTEGER;
ALTER TABLE publications ADD COLUMN thumb_height INTEGER;
