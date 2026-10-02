-- Per-user permission controlling who may add books to the shared library.
-- Defaults to 1 so existing and newly created accounts can upload; an
-- administrator can revoke it per user from the admin panel or management API.

ALTER TABLE users ADD COLUMN can_upload INTEGER NOT NULL DEFAULT 1;
