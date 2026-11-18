//! Error pages in the site's design.
//!
//! [`HtmlError`] is built by extractors that only see a [`RequestError`], so it
//! cannot reach the templates. It marks its response with [`ErrorPageInfo`]
//! instead, and [`error_pages`], which has the state, swaps the minimal body
//! for `error.html`.

use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use tower_cookies::Cookies;
use web_mev_axum::RequestError;

use crate::state::AppState;

use super::{HtmlError, anon_context, base_context, current_user, is_htmx};

/// The status and public message of an error page, attached to the response
/// for [`error_pages`] to render.
#[derive(Debug, Clone)]
pub(super) struct ErrorPageInfo {
    pub(super) status: StatusCode,
    pub(super) message: String,
}

/// What `error.html` shows.
#[derive(Debug, Serialize)]
struct ErrorPageView {
    status: u16,
    icon: &'static str,
    title: String,
    lead: String,
    detail: String,
}

/// Render marked error responses with `error.html`, keeping their status and
/// headers. htmx requests keep the minimal body: the page shows those errors
/// in a toast rather than swapping in a whole document.
pub(crate) async fn error_pages(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let cookies = request.extensions().get::<Cookies>().cloned();
    let from_htmx = is_htmx(request.headers());
    let response = next.run(request).await;
    let (Some(cookies), false) = (cookies, from_htmx) else {
        return response;
    };
    let Some(info) = response.extensions().get::<ErrorPageInfo>().cloned() else {
        return response;
    };
    let Some(html) = render_error_page(&state, &cookies, &info).await else {
        return response;
    };
    let (mut parts, _) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    parts.headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Response::from_parts(parts, html.into())
}

/// `error.html` for `info`, with the full navigation for a signed-in viewer, or
/// `None` when the template fails (already logged).
async fn render_error_page(
    state: &AppState,
    cookies: &Cookies,
    info: &ErrorPageInfo,
) -> Option<String> {
    let (mut ctx, locale) = if let Some(viewer) = current_user(state, cookies).await {
        (base_context(state, &viewer, ""), viewer.locale.clone())
    } else {
        let locale = state.i18n.resolve(None);
        (anon_context(state, &locale), locale)
    };
    let kind = match info.status.as_u16() {
        code @ (400 | 403 | 404 | 413 | 429) => code.to_string(),
        _ => "other".to_owned(),
    };
    let tr = state.i18n.tr(&locale);
    let view = ErrorPageView {
        status: info.status.as_u16(),
        icon: match info.status {
            StatusCode::FORBIDDEN => "shield",
            StatusCode::NOT_FOUND => "search",
            _ => "flag",
        },
        title: tr.t(&format!("error_title_{kind}")),
        lead: tr.t(&format!("error_lead_{kind}")),
        detail: info.message.clone(),
    };
    ctx.insert("failure", &view);
    match state.templates.render("error.html", &ctx) {
        Ok(html) => Some(html),
        Err(err) => {
            tracing::error!(error = %err, "failed to render the error page");
            None
        }
    }
}

/// The fallback for unknown paths: the 404 page for browsers, and a bare 404,
/// as before, for the sync API, OPDS readers and other clients.
pub(crate) async fn not_found(headers: HeaderMap) -> Response {
    let wants_html = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"));
    if wants_html {
        HtmlError::from(RequestError::not_found()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}
