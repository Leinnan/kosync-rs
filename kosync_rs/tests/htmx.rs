//! Tests for the htmx fragment/partial rendering and static asset.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{
    app, basic, build_epub, login, multipart, register_plaintext, seed_documents, send_bytes,
    send_html,
};

/// Upload an `EPUB` via the OPDS endpoint as admin and return its document hash.
async fn upload_id(app: &Router, epub: &[u8]) -> String {
    let (body, boundary) = multipart("book.epub", "application/epub+zip", epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, bytes) = send_bytes(
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
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["results"][0]["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn htmx_asset_is_served() {
    let app = app(false).await;
    let (status, headers, bytes) =
        send_bytes(&app, "GET", "/static/htmx.min.js", &[], Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/javascript")
    );
    assert!(!bytes.is_empty());
}

#[tokio::test]
async fn books_renders_fragment_for_htmx() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let epub = build_epub(None);
    upload_id(&app, &epub).await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        "/books",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Test Book"));
    assert!(!body.contains("<!DOCTYPE"));

    let (_, _, full) = send_html(&app, "GET", "/books", &[("cookie", alice.as_str())], "").await;
    assert!(full.contains("<!DOCTYPE"));
}

#[tokio::test]
async fn books_search_fragment_filters_results() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let epub = build_epub(None);
    upload_id(&app, &epub).await;
    let alice = login(&app, "alice", "secret").await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?query=Test",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert!(body.contains("Test Book"));

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?query=zzz",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert!(!body.contains("Test Book"));
}

#[tokio::test]
async fn progress_renders_fragment_for_htmx() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 1).await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        "/",
        &[("cookie", alice.as_str()), ("hx-request", "true")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("doc0"));
    assert!(!body.contains("<!DOCTYPE"));
}

#[tokio::test]
async fn htmx_delete_returns_ok_instead_of_redirect() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    let (body, boundary) = multipart("book.epub", "application/epub+zip", &epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    send_bytes(
        &app,
        "POST",
        "/books/upload",
        &[
            ("content-type", content_type.as_str()),
            ("cookie", admin.as_str()),
        ],
        body,
    )
    .await;
    let document_hash = upload_id(&app, &epub).await;

    let (status, _, _) = send_bytes(
        &app,
        "POST",
        &format!("/books/{document_hash}/delete"),
        &[("cookie", admin.as_str()), ("hx-request", "true")],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send_bytes(
        &app,
        "POST",
        &format!("/books/{document_hash}/delete"),
        &[("cookie", admin.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}
