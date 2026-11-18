-- The language a user picked for the web UI and OPDS feeds, such as `pl`.
-- NULL means automatic: the language cookie, then the browser's Accept-Language,
-- then KOSYNC_RS_DEFAULT_LANGUAGE.
ALTER TABLE users ADD COLUMN locale TEXT;
