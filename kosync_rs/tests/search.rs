//! Tests for the global search endpoint backing the command palette.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{
    JSON, app, basic, build_epub, login, md5, multipart, register_plaintext, send, send_bytes,
    send_html,
};

/// Upload the test `EPUB` via OPDS as admin.
async fn upload_test_book(app: &axum::Router) {
    let (body, boundary) = multipart("book.epub", "application/epub+zip", &build_epub(None));
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        app,
        "POST",
        "/opds/upload",
        &[
            ("content-type", content_type.as_str()),
            ("authorization", basic("admin", "admin").as_str()),
        ],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn search_requires_login() {
    let app = app(false).await;
    let (status, headers, _) = send_html(&app, "GET", "/search?q=x", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get("location").unwrap(), "/login");
}

#[tokio::test]
async fn search_finds_books_and_documents() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    upload_test_book(&app).await;
    let key = md5("secret");
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &[
            ("x-auth-user", "alice"),
            ("x-auth-key", key.as_str()),
            (JSON, "application/json"),
        ],
        r#"{"document":"testdoc","progress":"/p[1]","percentage":0.25,"device":"kobo","device_id":"k"}"#,
    )
    .await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        "/search?q=test",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("<!DOCTYPE"));
    assert!(body.contains("Test Book"));
    assert!(body.contains(r#"href="/documents/testdoc""#), "{body}");
}

#[tokio::test]
async fn search_filters_navigation_actions_by_permission() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let alice = login(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/search?q=users",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert!(!body.contains("Manage users"));

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/search?q=users",
        &[("cookie", admin.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert!(body.contains("Manage users"));
}

#[tokio::test]
async fn search_without_htmx_renders_full_page() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        "/search?q=nothing-matches-this",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<!DOCTYPE"));
    assert!(body.contains("No results for"));
}
