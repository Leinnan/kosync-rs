//! Tests for how absolute OPDS links pick their scheme and host.

#![allow(clippy::unwrap_used)]

mod common;

use std::net::SocketAddr;

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

use common::{basic, test_config};
use kosync_rs::{Config, build_state, init_db, router};

/// Build an application from a customized test configuration.
async fn app_with(configure: impl FnOnce(&mut Config)) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);

    let mut config = test_config(&url, &books_dir, false);
    configure(&mut config);
    let pool = init_db(&config).await.unwrap();
    router(build_state(&config, pool))
}

/// The `self` link of the OPDS 2.0 root feed, requested from `peer` (if any)
/// with the given extra headers.
async fn self_link(app: &Router, peer: Option<&str>, headers: &[(&str, &str)]) -> String {
    let mut builder = Request::builder()
        .uri("/opds/v2/")
        .header("authorization", basic("admin", "admin"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    if let Some(peer) = peer {
        let addr: SocketAddr = peer.parse().unwrap();
        builder = builder.extension(ConnectInfo(addr));
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["links"][0]["href"].as_str().unwrap().to_owned()
}

const SPOOFED: [(&str, &str); 3] = [
    ("host", "books.lan:8090"),
    ("x-forwarded-proto", "https"),
    ("x-forwarded-host", "evil.example"),
];

#[tokio::test]
async fn forwarded_headers_from_untrusted_clients_are_ignored() {
    let app = app_with(|_| {}).await;
    assert_eq!(
        self_link(&app, Some("203.0.113.9:5555"), &SPOOFED).await,
        "http://books.lan:8090/opds/v2/"
    );
    assert_eq!(
        self_link(&app, None, &SPOOFED).await,
        "http://books.lan:8090/opds/v2/"
    );
}

#[tokio::test]
async fn forwarded_headers_from_trusted_proxies_are_used() {
    let app = app_with(|config| config.trusted_proxies = "127.0.0.1".to_owned()).await;
    let headers = [
        ("host", "127.0.0.1:8090"),
        ("x-forwarded-proto", "HTTPS"),
        ("x-forwarded-host", "books.example.com, inner.lan"),
    ];
    assert_eq!(
        self_link(&app, Some("127.0.0.1:40000"), &headers).await,
        "https://books.example.com/opds/v2/"
    );
    // A different peer is not trusted.
    assert_eq!(
        self_link(&app, Some("10.0.0.2:40000"), &headers).await,
        "http://127.0.0.1:8090/opds/v2/"
    );
}

#[tokio::test]
async fn configured_public_url_wins() {
    let app = app_with(|config| {
        config.public_url = Some("https://books.example.com/".to_owned());
    })
    .await;
    assert_eq!(
        self_link(&app, None, &SPOOFED).await,
        "https://books.example.com/opds/v2/"
    );
}

#[tokio::test]
async fn malformed_hosts_fall_back_to_localhost() {
    let app = app_with(|_| {}).await;
    assert_eq!(
        self_link(&app, None, &[("host", "a\"b<c>")]).await,
        "http://localhost/opds/v2/"
    );
}
