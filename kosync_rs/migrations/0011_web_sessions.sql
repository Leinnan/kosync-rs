-- Browser sessions, CSRF tokens, login throttling and an audit log, as
-- web_mev_auth expects them (copied from its sql/auth_tables.sql). The old
-- username-in-a-cookie sessions are not carried over: everyone signs in again.
--
-- `web_password_hash` is an Argon2id hash for signing in to the web UI. The MD5
-- `password_hash` stays, because KOReader sends that MD5 itself. Accounts start
-- without one and get it on their next web sign-in.
ALTER TABLE users ADD COLUMN web_password_hash TEXT;

CREATE TABLE sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- SHA-256 of the cookie's token. The token itself is never stored.
    token_hash TEXT NOT NULL UNIQUE,
    user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX sessions_user_idx ON sessions(user_id);
CREATE INDEX sessions_expiry_idx ON sessions(expires_at);

CREATE TABLE login_throttle (
    -- SHA-256 of the normalized login name, so the raw name never rests here.
    identity_hash TEXT PRIMARY KEY,
    failures INTEGER NOT NULL,
    locked_until INTEGER,
    updated_at INTEGER NOT NULL
);

-- A single-row guard that makes first-run setup safe under concurrent requests.
CREATE TABLE bootstrap_lock (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    touched_at INTEGER NOT NULL
);

CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    details_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);
CREATE INDEX audit_log_created_idx ON audit_log(created_at DESC);
