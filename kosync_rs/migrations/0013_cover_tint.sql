-- The colour pages put behind a book's cover, as `#rrggbb`, taken from its
-- thumbnail. NULL until computed; pages then fall back to a hue derived from
-- the title.
ALTER TABLE publications ADD COLUMN cover_tint TEXT;
