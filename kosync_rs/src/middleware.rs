//! Request logging middleware with trusted-proxy support.

use axum::{
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::Response,
};
use std::net::SocketAddr;

use crate::state::AppState;

/// Log one line per request, resolving the client IP via trusted proxies.
pub(crate) async fn access_log(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let ip = resolve_client_ip(&state, addr, request.headers());

    let response = next.run(request).await;

    tracing::info!(
        ip = %ip,
        method = %method,
        uri = %uri,
        status = %response.status().as_u16(),
        "request"
    );

    response
}

/// Resolve the real client IP, honouring `X-Forwarded-For` from trusted proxies.
fn resolve_client_ip(
    state: &AppState,
    addr: SocketAddr,
    headers: &axum::http::HeaderMap,
) -> String {
    let direct = addr.ip().to_string();

    if state.trusted_proxies.contains(&addr.ip())
        && let Some(first) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|xff| xff.split(',').next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return first.to_owned();
    }

    direct
}
