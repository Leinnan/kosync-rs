//! Server-side-rendered web frontend.

mod admin;
mod auth;
mod books;
mod document;
mod progress;
mod search;
mod settings;
mod upload;

use std::collections::HashMap;

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tera::{Tera, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tower_cookies::Cookies;

use crate::{state::AppState, store};

/// Name of the session cookie.
pub(crate) const AUTH_COOKIE: &str = "auth";

/// Build the frontend router.
pub(crate) fn router(max_upload_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/login", get(auth::login_form).post(auth::login_submit))
        .route("/logout", get(auth::logout))
        .route("/help", get(help))
        .route("/settings", get(settings::form))
        .route("/settings/password", post(settings::change_own))
        .route("/settings/user-password", post(settings::change_user))
        .route("/", get(progress::progress_list))
        .route("/search", get(search::search))
        .route("/documents/{document_hash}", get(document::document_detail))
        .route("/books", get(books::list))
        .route("/books/upload", get(upload::form))
        .route(
            "/books/upload",
            post(upload::submit).layer(axum::extract::DefaultBodyLimit::max(max_upload_bytes)),
        )
        .route("/books/{document_hash}", get(books::detail))
        .route("/books/{document_hash}/cover", get(books::cover))
        .route("/books/{document_hash}/thumbnail", get(books::thumbnail))
        .route("/books/{document_hash}/download", get(books::download))
        .route("/books/{document_hash}/delete", post(books::delete))
        .route("/admin/users", get(admin::list))
        .route("/admin/users/create", post(admin::create))
        .route("/admin/users/{username}/active", post(admin::toggle_active))
        .route(
            "/admin/users/{username}/can-upload",
            post(admin::toggle_can_upload),
        )
        .route(
            "/admin/users/{username}/password",
            post(admin::reset_password),
        )
        .route("/admin/users/{username}/delete", post(admin::delete))
        .route("/static/pico.sand.min.css", get(css))
        .route("/static/app.css", get(app_css))
        .route("/static/htmx.min.js", get(js))
        .route("/static/app.js", get(app_js))
}

/// Load the embedded templates into a [`Tera`] engine.
pub(crate) fn templates() -> Tera {
    let mut tera = Tera::default();
    tera.add_raw_templates([
        ("icons.html", include_str!("../../templates/icons.html")),
        ("cards.html", include_str!("../../templates/cards.html")),
        ("base.html", include_str!("../../templates/base.html")),
        ("login.html", include_str!("../../templates/login.html")),
        (
            "progress.html",
            include_str!("../../templates/progress.html"),
        ),
        (
            "document.html",
            include_str!("../../templates/document.html"),
        ),
        ("books.html", include_str!("../../templates/books.html")),
        ("book.html", include_str!("../../templates/book.html")),
        ("help.html", include_str!("../../templates/help.html")),
        ("upload.html", include_str!("../../templates/upload.html")),
        (
            "books_grid.html",
            include_str!("../../templates/books_grid.html"),
        ),
        (
            "progress_table.html",
            include_str!("../../templates/progress_table.html"),
        ),
        (
            "upload_results.html",
            include_str!("../../templates/upload_results.html"),
        ),
        ("users.html", include_str!("../../templates/users.html")),
        (
            "settings.html",
            include_str!("../../templates/settings.html"),
        ),
        ("search.html", include_str!("../../templates/search.html")),
        (
            "search_results.html",
            include_str!("../../templates/search_results.html"),
        ),
    ])
    .expect("register embedded templates");
    tera.register_filter("timeago", timeago_filter);
    tera.register_filter("datefmt", datefmt_filter);
    tera.register_filter("pct", pct_filter);
    tera.register_function("static_url", static_url_function());
    tera
}

/// Embedded Pico CSS stylesheet.
const PICO_CSS: &str = include_str!("../../static/pico.sand.min.css");
/// Embedded application stylesheet.
const APP_CSS: &str = include_str!("../../static/app.css");
/// Embedded progressive-enhancement script.
const APP_JS: &str = include_str!("../../static/app.js");
/// Embedded htmx script.
const HTMX_JS: &str = include_str!("../../static/htmx.min.js");

/// Content hash of an embedded asset, used for `ETag`s and cache busting.
fn content_hash(body: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

/// Build the `static_url(name=…)` Tera function, which returns a
/// content-versioned URL so browsers never keep a stale cached asset after an
/// upgrade.
fn static_url_function() -> impl tera::Function {
    let urls: HashMap<&'static str, String> = [
        ("pico.sand.min.css", PICO_CSS),
        ("app.css", APP_CSS),
        ("app.js", APP_JS),
        ("htmx.min.js", HTMX_JS),
    ]
    .into_iter()
    .map(|(name, body)| (name, format!("/static/{name}?v={}", content_hash(body))))
    .collect();

    move |args: &HashMap<String, Value>| -> tera::Result<Value> {
        let name = args.get("name").and_then(Value::as_str).unwrap_or_default();
        urls.get(name)
            .map(|url| Value::String(url.clone()))
            .ok_or_else(|| tera::Error::msg(format!("unknown static asset `{name}`")))
    }
}

/// Serve an embedded static asset with a content type, cache headers, and a
/// content-derived `ETag`.
fn asset(content_type: &'static str, body: &'static str) -> Response {
    let etag = format!("\"{}\"", content_hash(body));

    (
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::CACHE_CONTROL, "public, max-age=3600".to_owned()),
            (header::ETAG, etag),
        ],
        body,
    )
        .into_response()
}

/// Serve the embedded Pico CSS stylesheet.
async fn css() -> Response {
    asset("text/css", PICO_CSS)
}

/// Serve the embedded application stylesheet.
async fn app_css() -> Response {
    asset("text/css", APP_CSS)
}

/// Serve the embedded progressive-enhancement script.
async fn app_js() -> Response {
    asset("application/javascript", APP_JS)
}

/// Serve the embedded htmx script.
async fn js() -> Response {
    asset("application/javascript", HTMX_JS)
}

/// Render the e-reader connection help page (public; shows navigation when
/// signed in).
async fn help(State(state): State<AppState>, cookies: Cookies) -> Response {
    let ctx = match current_user(&state, &cookies).await {
        Some(user) => base_context(&user, "help"),
        None => tera::Context::new(),
    };
    render(&state, "help.html", &ctx)
}

/// Whether the request was made by htmx (i.e. expects an HTML fragment).
pub(super) fn is_htmx(headers: &HeaderMap) -> bool {
    headers.get("hx-request").is_some()
}

/// Render a template into an HTML response, or a 500 on failure.
pub(crate) fn render(state: &AppState, name: &str, ctx: &tera::Context) -> Response {
    match state.tera.render(name, ctx) {
        Ok(html) => Html(html).into_response(),
        Err(err) => {
            tracing::error!(error = %err, template = name, "failed to render template");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
        }
    }
}

/// Resolve the currently logged-in user from the session cookie.
pub(crate) async fn current_user(state: &AppState, cookies: &Cookies) -> Option<store::User> {
    let username = cookies
        .private(&state.cookie_key)
        .get(AUTH_COOKIE)?
        .value()
        .to_owned();

    match store::get_user(&state.pool, &username).await {
        Ok(Some(user)) if user.is_active => Some(user),
        _ => None,
    }
}

/// Build a template context seeded with the shared navigation variables.
pub(super) fn base_context(user: &store::User, active: &str) -> tera::Context {
    let mut ctx = tera::Context::new();
    ctx.insert("username", &user.username);
    ctx.insert("is_admin", &user.is_administrator);
    ctx.insert("can_upload", &user.may_upload());
    ctx.insert("active", active);
    ctx
}

/// A minimal book reference for linking a synced document to a publication.
#[derive(Debug, Serialize)]
pub(super) struct BookLinkView {
    document_hash: String,
    title: String,
    author: String,
    has_thumb: bool,
    /// Hue of the generated placeholder cover.
    hue: u16,
}

/// Build a [`BookLinkView`] from a publication.
pub(super) fn book_link(publication: &store::Publication) -> BookLinkView {
    BookLinkView {
        document_hash: publication.document_hash.clone(),
        title: publication.title.clone(),
        author: authors_string(&publication.authors),
        has_thumb: publication.thumb_path.is_some(),
        hue: cover_hue(&publication.title),
    }
}

/// Derive a stable hue (0–359) from a title for generated placeholder covers.
pub(super) fn cover_hue(title: &str) -> u16 {
    // FNV-1a: tiny, deterministic, and well distributed for short strings.
    let hash = title.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    u16::try_from(hash % 360).unwrap_or(0)
}

/// Parse a stored authors JSON array into a comma-separated string.
pub(super) fn authors_string(authors: &str) -> String {
    serde_json::from_str::<Vec<String>>(authors)
        .unwrap_or_default()
        .join(", ")
}

/// Format a Unix timestamp as an RFC 3339 UTC string.
pub(super) fn format_timestamp(timestamp: i64) -> String {
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// Month abbreviations used by [`human_date`].
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parse an RFC 3339 timestamp string from a template value, if present.
fn parse_rfc3339(value: &Value) -> Option<OffsetDateTime> {
    let raw = value.as_str().filter(|s| !s.is_empty())?;
    OffsetDateTime::parse(raw, &Rfc3339).ok()
}

/// Format a timestamp as a short absolute date, e.g. `Aug 13, 2026`.
pub(super) fn human_date(dt: OffsetDateTime) -> String {
    let month = MONTHS
        .get(u8::from(dt.month()).saturating_sub(1) as usize)
        .copied()
        .unwrap_or("");
    format!("{month} {}, {}", dt.day(), dt.year())
}

/// Pluralize a duration unit, e.g. `1 minute` vs `2 minutes`.
fn fmt_duration(n: i64, unit: &str) -> String {
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Format a timestamp as a relative phrase, e.g. `2 hours ago`.
fn relative_time(dt: OffsetDateTime) -> String {
    let delta = OffsetDateTime::now_utc() - dt;
    let secs = delta.whole_seconds();

    if secs < 0 {
        let secs = -secs;
        if secs < 60 {
            return "in a few seconds".to_owned();
        }
        if secs < 3600 {
            return format!("in {}", fmt_duration(secs / 60, "minute"));
        }
        if secs < 86_400 {
            return format!("in {}", fmt_duration(secs / 3600, "hour"));
        }
        if secs < 86_400 * 30 {
            return format!("in {}", fmt_duration(secs / 86_400, "day"));
        }
        return human_date(dt);
    }

    if secs < 60 {
        return "just now".to_owned();
    }
    if secs < 3600 {
        return format!("{} ago", fmt_duration(secs / 60, "minute"));
    }
    if secs < 86_400 {
        return format!("{} ago", fmt_duration(secs / 3600, "hour"));
    }
    if secs < 86_400 * 30 {
        return format!("{} ago", fmt_duration(secs / 86_400, "day"));
    }
    human_date(dt)
}

/// Tera filter rendering a relative time phrase from an RFC 3339 string.
#[allow(clippy::unnecessary_wraps)]
fn timeago_filter(value: &Value, _args: &HashMap<String, Value>) -> Result<Value, tera::Error> {
    Ok(Value::String(
        parse_rfc3339(value).map_or_else(String::new, relative_time),
    ))
}

/// Tera filter rendering a short absolute date from an RFC 3339 string.
#[allow(clippy::unnecessary_wraps)]
fn datefmt_filter(value: &Value, _args: &HashMap<String, Value>) -> Result<Value, tera::Error> {
    Ok(Value::String(
        parse_rfc3339(value).map_or_else(String::new, human_date),
    ))
}

/// Format a progress fraction as a whole percentage, e.g. `0.437` → `43%`.
///
/// Values are floored so an almost-finished book never reads `100%`, except
/// at or above the 0.99 threshold the rest of the app treats as finished.
fn format_pct(fraction: f64) -> String {
    if fraction >= 0.99 {
        return "100%".to_owned();
    }
    format!("{:.0}%", (fraction.max(0.0) * 100.0).floor())
}

/// Tera filter rendering a progress fraction as a whole percentage.
#[allow(clippy::unnecessary_wraps)]
fn pct_filter(value: &Value, _args: &HashMap<String, Value>) -> Result<Value, tera::Error> {
    Ok(Value::String(format_pct(value.as_f64().unwrap_or(0.0))))
}

/// Format a byte count as a human-readable size.
pub(super) fn format_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];

    let bytes = u64::try_from(bytes.max(0)).unwrap_or(0);
    let mut value = bytes;
    let mut unit = 0usize;

    while value >= 1000 && unit + 1 < UNITS.len() {
        value /= 1000;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        let divisor = 1000u64.pow(u32::try_from(unit).unwrap_or(0));
        let whole = bytes / divisor;
        let tenths = (bytes % divisor) * 10 / divisor;
        format!("{whole}.{tenths} {}", UNITS[unit])
    }
}

/// Format a count with the correct singular or plural noun.
pub(super) fn pluralize(count: i64, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {plural}")
    }
}
