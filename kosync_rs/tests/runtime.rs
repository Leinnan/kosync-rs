//! Health endpoints and response security headers.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn health_and_ready_respond() {
    let app = common::app(true).await;
    for path in ["/health", "/ready", "/healthcheck"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
    }
}

#[tokio::test]
async fn pages_carry_security_headers() {
    let app = common::app(true).await;
    let response = app
        .oneshot(Request::get("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let headers = response.headers();
    let csp = headers["content-security-policy"].to_str().unwrap();
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["cache-control"], "private, no-store");
}

#[tokio::test]
async fn static_files_are_versioned_and_compressed() {
    let app = common::app(true).await;
    let page = common::send_html(&app, "GET", "/login", &[], "").await.2;
    let start = page.find("/static/app.css?v=").unwrap();
    let end = start + page[start..].find('"').unwrap();
    let url = &page[start..end];

    let response = app
        .clone()
        .oneshot(
            Request::get(url)
                .header("accept-encoding", "br, gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-encoding"], "br");
    let cache = response.headers()["cache-control"].to_str().unwrap();
    assert!(cache.contains("immutable"), "{cache}");

    let missing = app
        .oneshot(
            Request::get("/static/nope.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}
