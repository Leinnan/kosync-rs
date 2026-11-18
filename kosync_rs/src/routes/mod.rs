//! Route assembly.

mod manage;
mod sync;

use axum::{Router, middleware::from_fn_with_state};
use tower_cookies::CookieManagerLayer;
use web_mev_axum::{AccessLog, Csp, SecurityHeaders, access_log, health_routes, security_headers};

use crate::{opds, state::AppState, web};

/// The Content-Security-Policy for every response: same-origin scripts only.
///
/// Inline styles stay allowed, because templates set per-book cover tints and
/// progress widths with `style` attributes, and `base.html` declares the
/// digest-versioned web fonts in a `<style>` block. Images may be `data:` URIs,
/// which the stylesheet uses for the select arrow.
fn content_security_policy() -> Csp {
    Csp::strict()
        .set("style-src", "'self' 'unsafe-inline'")
        .set("img-src", "'self' data:")
}

/// Build the full application router with the given state.
pub(crate) fn router(state: AppState) -> Router {
    let max_upload_bytes = state.config.max_upload_bytes;
    let pool = state.pool.clone();
    let access = AccessLog::new(state.trusted_proxies.clone());
    let locale_state = state.clone();
    #[allow(clippy::expect_used)]
    let headers = SecurityHeaders::new(&content_security_policy())
        .expect("the Content-Security-Policy is a valid header value");
    Router::new()
        .merge(sync::router())
        .merge(manage::router())
        .merge(opds::router(max_upload_bytes))
        .merge(web::router(max_upload_bytes))
        .fallback(web::not_found)
        .with_state(state.clone())
        // Inside the locale layer, so error pages render in the request's language.
        .layer(from_fn_with_state(state, web::error_pages))
        .layer(from_fn_with_state(
            locale_state,
            crate::i18n::negotiate_locale,
        ))
        .merge(health_routes(move || {
            let pool = pool.clone();
            async move { web_mev_sqlite::ready(&pool).await }
        }))
        .layer(CookieManagerLayer::new())
        .layer(from_fn_with_state(headers, security_headers))
        .layer(from_fn_with_state(access, access_log))
}
