# kosync-rs

A self-hostable [KOReader](https://koreader.rocks/) sync server written in
Rust. It is a drop-in replacement for the official
[koreader-sync-server](https://github.com/koreader/koreader-sync-server) and
implements the same versioned sync protocol, storing all data in a single
SQLite database file (no external services required).

Users of KOReader can register on this server and use the built-in *Progress
sync* plugin to keep reading progress synchronized between devices.

## Features

- Implements the full KOReader sync protocol v1 (`/users/create`, `/users/auth`,
  `/syncs/progress`, `/healthcheck`).
- Single-file SQLite storage with automatic migrations.
- Append-only progress history: every sync is recorded, so each document's
  first/last sync time and per-device contribution can be inspected.
- Admin user (`admin`) seeded on first start, plus a `/manage/*` management API
  and a web admin panel at `/admin/users` for creating accounts, activating or
  deactivating users, resetting passwords, and granting or revoking upload
  permission.
- Optional registration disable flag (off by default: accounts are created by an
  administrator), trusted-proxy-aware request logging, and single-line logging
  mode.
- A shared `EPUB` library served over **OPDS 2.0** (JSON-LD) and **OPDS 1.x**
  (Atom): upload books as an administrator or any user granted the upload
  permission, generate covers, and match them to each user's synced reading
  progress. Every account keeps its own reading progress while the book library
  is shared.

## Protocol overview

Authentication uses two request headers set by the KOReader client:

- `x-auth-user` — the username.
- `x-auth-key` — the MD5 hash of the password (the client hashes it).

| Endpoint | Method | Description |
| --- | --- | --- |
| `/healthcheck` | GET | Returns `{"state":"OK"}`. |
| `/users/create` | POST | Register a user with `{"username","password"}`. |
| `/users/auth` | GET | Validate credentials. Returns `{"authorized":"OK"}`. |
| `/syncs/progress` | PUT | Store reading progress. |
| `/syncs/progress/{document}` | GET | Fetch reading progress. |

Errors are returned as `{"code": <int>, "message": <string>}` with the same
codes as the official server (e.g. `2001` unauthorized, `2002` username taken,
`2005` registration disabled). A missing document returns an empty object
`{}` with status `200`, matching the official behaviour.

## Running

### With Docker (recommended)

```bash
docker compose up -d --build
```

This stores the SQLite database in `./data` on the host.

### With Cargo

```bash
cargo run --release
```

## Configuration

Configuration is read from environment variables, optionally falling back to a
`.env` file. Process environment variables take precedence over `.env`, and a
missing `.env` is ignored, so running with variables set only in the process
environment (for example under Docker Compose) works without one. See
[`.env.example`](.env.example) for the full list.

| Variable | Default | Description |
| --- | --- | --- |
| `KOSYNC_RS_HOST` | `0.0.0.0` | Bind address. |
| `KOSYNC_RS_PORT` | `8090` | Listen port. |
| `KOSYNC_RS_DATABASE_URL` | `sqlite://data/kosync.db` | SQLite connection string. |
| `KOSYNC_RS_ADMIN_PASSWORD` | `admin` | Admin user password (MD5-hashed). |
| `KOSYNC_RS_REGISTRATION_DISABLED` | `true` | Reject new registrations. Accounts are created by an administrator; set to `false` to allow public sign-up. |
| `KOSYNC_RS_TRUSTED_PROXIES` | *(empty)* | Comma-separated proxy IPs for `X-Forwarded-For`. |
| `KOSYNC_RS_SINGLE_LINE_LOGGING` | `false` | Emit single-line log output. |
| `KOSYNC_RS_BOOKS_DIR` | `data/books` | Directory for uploaded `EPUB` files and covers. |
| `KOSYNC_RS_MAX_UPLOAD_BYTES` | `209715200` | Maximum upload size in bytes (200 MiB). |
| `KOSYNC_RS_METADATA_ENRICHMENT` | `false` | Opt in to background Google Books/Open Library enrichment by ISBN. This sends uploaded-book ISBNs to those APIs. |

The `admin` user is created on first start with the configured password and
is used to access the management API and the web admin panel. Logging respects
the `RUST_LOG` environment variable (defaults to `info`).

## Web UI

The server renders a small HTML frontend at `/` (progress dashboard, `/books`
for the shared library, `/books/upload`, and `/help`). Log in at `/login` with a
username and password.

Administrators additionally get a user-management page at `/admin/users` for:

- creating accounts (optionally granting upload permission),
- activating and deactivating accounts,
- resetting passwords,
- granting or revoking the ability to upload books,
- deleting accounts.

The built-in `admin` account is protected from deactivation, password changes,
and deletion. New accounts are allowed to upload by default; revoke the
permission per user to restrict who can add books to the shared library.

## Management API

Admin-only endpoints (unless noted). Authenticate using the same headers as
the sync protocol, with the admin credentials:

```
x-auth-user: admin
x-auth-key: <MD5 hash of ADMIN_PASSWORD>
```

| Endpoint | Method | Description |
| --- | --- | --- |
| `/manage/users` | GET | List all users. |
| `/manage/users` | POST | Create a user (plain password, hashed server-side; optional boolean `can_upload`, default `true`). |
| `/manage/users?username=` | DELETE | Delete a user (admin or self). |
| `/manage/users/documents?username=` | GET | List a user's documents (admin or self). |
| `/manage/users/documents?username=&documentHash=` | DELETE | Delete a document (admin or self). |
| `/manage/users/active?username=` | PUT | Toggle a user's active status. |
| `/manage/users/can-upload?username=` | PUT | Toggle a user's permission to upload books. |
| `/manage/users/password?username=` | PUT | Change a user's password. |

## OPDS library

A shared `EPUB` library is exposed over **OPDS 2.0** (JSON-LD) and **OPDS 1.x**
(Atom). Administrators and users granted the upload permission add books; every
authenticated user can browse and download them. Endpoints are protected with
**HTTP Basic** auth (username and plaintext password, which the server
MD5-hashes before comparison).

### Connecting an e-reader

Most e-readers and older OPDS clients (KOReader's OPDS browser, Calibre's
content server clients, KOBO-style readers) only speak **OPDS 1.x (Atom)**.
Point those devices at:

```
http://<host>:8090/opds/
```

Use the **OPDS 2.0** endpoints (`/opds/v2/…`) only for modern Readium-based
clients. The bare `http://<host>:8090/` root serves the web UI (HTML), not OPDS,
so pointing an e-reader at the bare host/root will fail to parse. Configure the
same username/password in the device as you registered on the server (Basic auth).

> **Note:** HTTP Basic auth sends credentials in cleartext. Expose the server
> behind a TLS reverse proxy (e.g. Caddy, nginx, Traefik) when accessed over the
> internet.

Uploaded books are identified by KOReader's *partial MD5* document digest — the
same value sent as the `document` field in progress-sync requests. This means
the library automatically matches each book to a user's synced reading
progress, which is exposed as a `kosync` field in the OPDS 2.0 publication
metadata (and via the filename-hash fallback for KOReader's "Filename"
matching mode).

| Endpoint | Method | Auth | Description |
| --- | --- | --- | --- |
| `/opds/` | GET | Basic | OPDS 1.x (Atom) navigation feed. |
| `/opds/publications?page=&query=` | GET | Basic | OPDS 1.x (Atom) acquisition feed. |
| `/opds/{authors,genres,series}` | GET | Basic | OPDS 1.x navigation feeds, grouped with publication counts. |
| `/opds/{authors,genres,series}/{name}?page=` | GET | Basic | OPDS 1.x grouped acquisition feeds. |
| `/opds/v2/` | GET | Basic | OPDS 2.0 navigation feed. |
| `/opds/v2/publications?page=&query=` | GET | Basic | Paginated OPDS 2.0 publication feed. |
| `/opds/v2/{authors,genres,series}` | GET | Basic | OPDS 2.0 navigation feeds, grouped with publication counts. |
| `/opds/v2/{authors,genres,series}/{name}?page=` | GET | Basic | OPDS 2.0 grouped acquisition feeds. |
| `/opds/v2/publications/{hash}` | GET | Basic | Single publication manifest. |
| `/opds/v2/publications/{hash}` | DELETE | admin | Remove a publication and its files. |
| `/opds/v2/publications/{hash}/file` | GET | Basic | Download the `EPUB` file. |
| `/opds/v2/publications/{hash}/cover` | GET | Basic | Cover image. |
| `/opds/v2/publications/{hash}/thumbnail` | GET | Basic | Generated cover thumbnail. |
| `/opds/upload` | POST | uploader | Multipart `EPUB` upload (field `file`). Administrators and users granted upload permission. |

Upload responses report each file as `imported`, `duplicate`, or `error`.

Ingestion records embedded Dublin Core subjects as genres, normalizes ISBN-10
and ISBN-13 identifiers, and records calibre/EPUB series metadata. With
`KOSYNC_RS_METADATA_ENRICHMENT=true`, sparse genre metadata is enriched in the
background from Google Books and Open Library; a downloaded remote cover is
stored locally only when the EPUB has no embedded cover.

## Development

```bash
cargo test      # run the test suite
cargo clippy    # lint
cargo fmt       # format
```

## License

Licensed under the AGPL-3.0, matching the upstream KOReader sync server.
