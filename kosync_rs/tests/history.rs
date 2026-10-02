//! Tests for the document progress history feature.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{JSON, app, login, md5, register_plaintext, send, send_html};

async fn put_progress(app: &Router, key: &str, body: &str) {
    let headers = vec![
        ("x-auth-user", "alice"),
        ("x-auth-key", key),
        (JSON, "application/json"),
    ];
    let (status, _) = send(app, "PUT", "/syncs/progress", &headers, body).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn history_records_events_and_attributes_per_device_contributions() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let key = md5("secret");

    put_progress(
        &app,
        &key,
        r#"{"document":"book","progress":"/p[1]","percentage":0.4,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;
    put_progress(
        &app,
        &key,
        r#"{"document":"book","progress":"/p[2]","percentage":0.7,"device":"kobo","device_id":"dev2"}"#,
    )
    .await;
    put_progress(
        &app,
        &key,
        r#"{"document":"book","progress":"/p[3]","percentage":0.5,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;

    let (_, body) = send(
        &app,
        "GET",
        "/syncs/progress/book",
        &[("x-auth-user", "alice"), ("x-auth-key", key.as_str())],
        "",
    )
    .await;
    assert_eq!(body["percentage"], 0.5);

    let cookie = login(&app, "alice", "secret").await;
    let (status, _, text) = send_html(
        &app,
        "GET",
        "/documents/book",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("3 sync events"));
    assert!(text.contains("kindle"));
    assert!(text.contains("kobo"));
    assert!(text.contains("40.00%"));
    assert!(text.contains("30.00%"));
    assert!(text.contains(r#"<svg class="chart""#));
    assert!(text.contains("Recent syncs"));
    // The kindle regression from 70% to 50% is shown as a negative delta.
    assert!(text.contains("−20.0%"));
}

#[tokio::test]
async fn document_detail_redirects_for_unknown_document() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let cookie = login(&app, "alice", "secret").await;
    let (status, headers, _) = send_html(
        &app,
        "GET",
        "/documents/nope",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").and_then(|v| v.to_str().ok()),
        Some("/")
    );
}
