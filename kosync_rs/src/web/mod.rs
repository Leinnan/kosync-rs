//! Server-side-rendered web frontend.

mod admin;
mod auth;
mod books;
mod document;
mod error_page;
mod format;
mod progress;
mod search;
mod settings;
mod upload;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use include_dir::{Dir, include_dir};
use serde::Serialize;
use tera::{Kwargs, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tower_cookies::Cookies;
use web_mev_assets::Assets;
use web_mev_auth::{CSRF_HEADER, Session};
use web_mev_axum::{
    Flash, FlashKind, RequestError, WebError, html_error_response, json_error_response,
};
use web_mev_i18n::LocaleCode;
use web_mev_templates::{
    FilterOptions, TemplateError, TemplateSource, Templates, register_filters, register_static_url,
};

use crate::{
    i18n::{I18n, Tr, template_locale},
    state::AppState,
    store,
};

use error_page::ErrorPageInfo;
pub(crate) use error_page::{error_pages, not_found};

/// Build the frontend router.
pub(crate) fn router(max_upload_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/login", get(auth::login_form).post(auth::login_submit))
        .route("/logout", post(auth::logout))
        .route("/help", get(help))
        .route("/locale/{code}", get(switch_locale))
        .route("/settings/language", post(settings::change_language))
        .route("/settings", get(settings::form))
        .route("/settings/password", post(settings::change_own))
        .route("/settings/user-password", post(settings::change_user))
        .route("/", get(progress::progress_list))
        .route("/search", get(search::search))
        .route("/documents/{document_hash}", get(document::document_detail))
        .route("/documents/{document_hash}/link", post(document::link))
        .route("/books", get(books::list))
        .route("/books/upload", get(upload::form))
        .route(
            "/books/upload",
            post(upload::submit).layer(axum::extract::DefaultBodyLimit::max(max_upload_bytes)),
        )
        .route("/books/{document_hash}", get(books::detail))
        .route(
            "/books/{document_hash}/cover",
            get(books::cover)
                .post(books::replace_cover)
                .layer(axum::extract::DefaultBodyLimit::max(max_upload_bytes)),
        )
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
        .route("/static/{*path}", get(static_file))
        .route("/favicon.ico", get(favicon))
}

/// Page templates, compiled into the binary.
static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/templates");
/// Stylesheets and scripts, compiled into the binary.
static STATIC: Dir = include_dir!("$CARGO_MANIFEST_DIR/src/static");

/// The static files: embedded, or read from `directory` during development.
pub(crate) fn assets(directory: Option<PathBuf>) -> Assets {
    match directory {
        Some(directory) => Assets::directory(directory, "/static"),
        None => Assets::embedded(&STATIC, "/static"),
    }
}

/// Load the page templates, with the filters and functions they use.
///
/// Templates come from the binary unless `directory` is given. `static_url`
/// returns content-versioned URLs from `assets`, so browsers never keep a stale
/// cached file after an upgrade.
///
/// # Errors
///
/// Returns an error when a template does not compile or `base.html` is missing.
pub(crate) fn templates(
    assets: &Assets,
    directory: Option<PathBuf>,
    i18n: Arc<I18n>,
) -> Result<Templates, TemplateError> {
    let source = directory.map_or(
        TemplateSource::Embedded(&TEMPLATES),
        TemplateSource::Directory,
    );
    let assets = assets.clone();
    Templates::new(source, &["base.html"], move |tera| {
        register_filters(tera, &FilterOptions::default());
        let assets = assets.clone();
        register_static_url(tera, move |name| assets.url(name));
        crate::i18n::register(tera, i18n.clone());
        let filters = i18n.clone();
        tera.register_filter(
            "timeago",
            move |value: &Value, _: Kwargs, state: &tera::State| {
                let tr = filters.tr(&template_locale(&filters, state));
                parse_rfc3339(value).map_or_else(String::new, |dt| format::relative_time(&tr, dt))
            },
        );
        let filters = i18n.clone();
        tera.register_filter(
            "datefmt",
            move |value: &Value, _: Kwargs, state: &tera::State| {
                let tr = filters.tr(&template_locale(&filters, state));
                parse_rfc3339(value).map_or_else(String::new, |dt| format::human_date(&tr, dt))
            },
        );
        let filters = i18n.clone();
        tera.register_filter(
            "pct",
            move |value: &Value, _: Kwargs, state: &tera::State| {
                let tr = filters.tr(&template_locale(&filters, state));
                format::pct(&tr, value.as_f64().unwrap_or(0.0))
            },
        );
    })
}

/// Serve a file from `/static/`, compressed and cached by content version.
async fn static_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    match state.assets.respond(&path, uri.query(), &headers).await {
        Some(response) => response,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Serve the site icon at `/favicon.ico`, which browsers and feed readers
/// request without reading the page's `<link rel="icon">`.
async fn favicon(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match state.assets.respond("favicon.svg", None, &headers).await {
        Some(response) => response,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Render the e-reader connection help page (public; shows navigation when
/// signed in).
async fn help(State(state): State<AppState>, cookies: Cookies) -> Response {
    let ctx = match current_user(&state, &cookies).await {
        Some(user) => base_context(&state, &user, "help"),
        None => anon_context(&state, &state.i18n.resolve(None)),
    };
    render(&state, "help.html", &ctx)
}

/// Whether the request was made by htmx (i.e. expects an HTML fragment).
pub(super) fn is_htmx(headers: &HeaderMap) -> bool {
    web_mev_axum::is_htmx(headers)
}

/// Render a template into an HTML response, or a 500 on failure.
pub(crate) fn render(state: &AppState, name: &str, ctx: &tera::Context) -> Response {
    match state.templates.render(name, ctx) {
        Ok(html) => ([(header::CACHE_CONTROL, "private, no-store")], Html(html)).into_response(),
        Err(err) => {
            tracing::error!(error = %err, template = name, "failed to render template");
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error").into_response()
        }
    }
}

/// The signed-in user of a page, with their browser session.
///
/// It dereferences to the [`store::User`].
#[derive(Debug, Clone)]
pub(crate) struct Viewer {
    /// The account.
    pub(crate) user: store::User,
    /// The id of the browser session, kept when the user changes their password.
    pub(crate) session_id: i64,
    /// The session's CSRF token, which every form posts back.
    pub(crate) csrf_token: String,
    /// The page language: the user's saved choice, else the request's.
    pub(crate) locale: LocaleCode,
}

impl Viewer {
    /// The viewer behind a loaded session.
    pub(crate) fn new(state: &AppState, session: Session<store::User>) -> Self {
        let locale = state.i18n.resolve(session.user.locale.as_deref());
        Self {
            user: session.user,
            session_id: session.id,
            csrf_token: session.csrf_token,
            locale,
        }
    }

    /// Lookups in the viewer's language.
    pub(crate) fn tr<'a>(&self, state: &'a AppState) -> Tr<'a> {
        state.i18n.tr(&self.locale)
    }
}

impl std::ops::Deref for Viewer {
    type Target = store::User;

    fn deref(&self) -> &store::User {
        &self.user
    }
}

/// Resolve the signed-in user from the session cookie. Sessions of deactivated
/// accounts do not count.
pub(crate) async fn current_user(state: &AppState, cookies: &Cookies) -> Option<Viewer> {
    match state.sessions.load::<store::User>(cookies).await {
        Ok(session) => session.map(|session| Viewer::new(state, session)),
        Err(error) => {
            tracing::error!(%error, "session lookup failed");
            None
        }
    }
}

/// Whether a multipart request carries the session's CSRF token. htmx sends it
/// in a header; a plain form sends it as the `csrf_token` field.
pub(super) fn csrf_header_ok(viewer: &Viewer, headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|token| web_mev_core::secure_eq(&viewer.csrf_token, token))
}

/// A language the page can switch to.
#[derive(Debug, Serialize)]
pub(super) struct LanguageChoice {
    pub(super) code: String,
    pub(super) name: String,
}

/// Remember `locale` in this browser's language cookie for a year.
pub(super) fn remember_locale(state: &AppState, cookies: &Cookies, locale: &LocaleCode) {
    cookies.add(
        tower_cookies::Cookie::build((crate::i18n::LOCALE_COOKIE, locale.code()))
            .path("/")
            .http_only(true)
            .same_site(tower_cookies::cookie::SameSite::Lax)
            .secure(state.config.cookie_secure)
            .max_age(tower_cookies::cookie::time::Duration::days(365))
            .build(),
    );
}

/// Query of the language switch.
#[derive(Debug, serde::Deserialize)]
pub(super) struct SwitchQuery {
    /// Where to return to; only a local path is followed.
    next: Option<String>,
}

/// Switch the page language: remember it in this browser and, for a signed-in
/// user, in their account. Then return to the page the switch was clicked on.
async fn switch_locale(
    State(state): State<AppState>,
    cookies: Cookies,
    headers: HeaderMap,
    Path(code): Path<String>,
    axum::extract::Query(query): axum::extract::Query<SwitchQuery>,
) -> Response {
    let Some(locale) = state.i18n.parse(&code) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    remember_locale(&state, &cookies, &locale);
    if let Some(user) = current_user(&state, &cookies).await
        && let Err(err) = store::set_locale(&state.pool, user.id, Some(&locale.code())).await
    {
        err.log();
    }
    // Without `next`, return to the referring page when it is on this server.
    let referer = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(local_path);
    let target = web_mev_i18n::safe_return_path(query.next.as_deref().or(referer.as_deref()));
    axum::response::Redirect::to(&target).into_response()
}

/// The path and query of an absolute URL, for returning to the referring page.
fn local_path(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    rest.find('/').map(|slash| rest[slash..].to_owned())
}

/// A template context for a page anyone can see: its language and the
/// languages it can switch to.
pub(super) fn anon_context(state: &AppState, locale: &LocaleCode) -> tera::Context {
    let mut ctx = tera::Context::new();
    ctx.insert("locale", &locale.code());
    let languages: Vec<LanguageChoice> = state
        .i18n
        .supported()
        .iter()
        .filter(|candidate| *candidate != locale)
        .map(|candidate| LanguageChoice {
            code: candidate.code(),
            name: state.i18n.tr(candidate).t("language_name"),
        })
        .collect();
    ctx.insert("languages", &languages);
    ctx.insert("js_strings", &js_strings_json(&state.i18n.tr(locale)));
    ctx
}

/// The strings `app.js` shows, keyed without their `js_` prefix, as JSON safe
/// to print inside a `<script type="application/json">` element.
fn js_strings_json(tr: &Tr<'_>) -> String {
    let strings: std::collections::BTreeMap<&str, String> = JS_KEYS
        .iter()
        .map(|key| (key.trim_start_matches("js_"), tr.t(key)))
        .collect();
    serde_json::to_string(&strings)
        .unwrap_or_else(|_| "{}".to_owned())
        .replace('<', "\\u003c")
}

/// The translation keys `app.js` reads from the page.
const JS_KEYS: &[&str] = &[
    "js_theme_auto",
    "js_theme_light",
    "js_theme_dark",
    "js_error_status",
    "js_error_generic",
    "js_error_network",
    "js_confirm_title",
    "js_confirm_label",
    "js_copied",
    "js_password_show",
    "js_password_hide",
    "js_remove_file",
    "js_upload",
    "js_upload_files_one",
    "js_upload_files_few",
    "js_upload_files_many",
    "js_upload_files_other",
    "js_decimal",
];

/// Build a template context seeded with the shared navigation variables.
pub(super) fn base_context(state: &AppState, viewer: &Viewer, active: &str) -> tera::Context {
    let mut ctx = anon_context(state, &viewer.locale);
    ctx.insert("username", &viewer.username);
    ctx.insert("is_admin", &viewer.is_administrator);
    ctx.insert("can_upload", &viewer.may_upload());
    ctx.insert("csrf_token", &viewer.csrf_token);
    ctx.insert("active", active);
    ctx
}

/// Show `message` on the next page, as an error or a success notice.
pub(super) fn flash_next(state: &AppState, cookies: &Cookies, error: bool, message: &str) {
    let flash: Flash = if error {
        Flash::danger(message)
    } else {
        Flash::success(message)
    };
    state.flash.set(cookies, &flash);
}

/// Move a pending flash message into the page's `error` or `notice`.
pub(super) fn take_flash(state: &AppState, cookies: &Cookies, ctx: &mut tera::Context) {
    if let Some(flash) = state.flash.take::<()>(cookies) {
        let key = if flash.kind == FlashKind::Danger {
            "error"
        } else {
            "notice"
        };
        ctx.insert(key, &flash.message);
    }
}

/// A request rejected before its handler ran, as an HTML page.
#[derive(Debug)]
pub struct HtmlError(RequestError);

impl From<RequestError> for HtmlError {
    fn from(error: RequestError) -> Self {
        Self(error)
    }
}

impl IntoResponse for HtmlError {
    /// The minimal page, marked for [`error_pages`] to replace with
    /// `error.html`.
    fn into_response(self) -> Response {
        let mut response = html_error_response(&self.0, |page| page.minimal_html("kosync"));
        response.extensions_mut().insert(ErrorPageInfo {
            status: self.0.status(),
            message: self.0.public_message().into_owned(),
        });
        response
    }
}

/// A request rejected before its handler ran, as JSON.
#[derive(Debug)]
pub struct JsonError(RequestError);

impl From<RequestError> for JsonError {
    fn from(error: RequestError) -> Self {
        Self(error)
    }
}

impl IntoResponse for JsonError {
    fn into_response(self) -> Response {
        json_error_response(&self.0)
    }
}

/// A minimal book reference for linking a synced document to a publication.
#[derive(Debug, Serialize)]
pub(super) struct BookLinkView {
    document_hash: String,
    title: String,
    author: String,
    has_thumb: bool,
    /// Tint colour of the cover: generated covers and hero panels use it.
    tint: String,
}

/// Build a [`BookLinkView`] from a publication.
pub(super) fn book_link(publication: &store::Publication) -> BookLinkView {
    BookLinkView {
        document_hash: publication.document_hash.clone(),
        title: publication.title.clone(),
        author: authors_string(&publication.authors),
        has_thumb: publication.thumb_path.is_some(),
        tint: publication_tint(publication),
    }
}

/// The tint of a publication's cover: the colour taken from its thumbnail, or
/// one derived from its title when there is none yet.
pub(super) fn publication_tint(publication: &store::Publication) -> String {
    publication
        .cover_tint
        .clone()
        .unwrap_or_else(|| title_tint(&publication.title))
}

/// A tint derived from a title, for documents without a cover picture.
pub(super) fn title_tint(title: &str) -> String {
    format!("hsl({} 38% 34%)", cover_hue(title))
}

/// Derive a stable hue (0–359) from a title for generated placeholder covers.
fn cover_hue(title: &str) -> u16 {
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
/// Parse an RFC 3339 timestamp string from a template value, if present.
fn parse_rfc3339(value: &Value) -> Option<OffsetDateTime> {
    let raw = value.as_str().filter(|s| !s.is_empty())?;
    OffsetDateTime::parse(raw, &Rfc3339).ok()
}
