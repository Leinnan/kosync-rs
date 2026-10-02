//! Route assembly.

mod manage;
mod sync;

use axum::Router;
use tower_cookies::CookieManagerLayer;

use crate::{opds, state::AppState, web};

/// Build the full application router with the given state.
pub(crate) fn router(state: AppState) -> Router {
    let max_upload_bytes = state.config.max_upload_bytes;
    Router::new()
        .merge(sync::router())
        .merge(manage::router())
        .merge(opds::router(max_upload_bytes))
        .merge(web::router(max_upload_bytes))
        .with_state(state)
        .layer(CookieManagerLayer::new())
}
