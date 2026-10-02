//! Tests for the server-side-rendered web frontend.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{JSON, app, login, md5, register_plaintext, seed_documents, send, send_html};

#[tokio::test]
async fn web_login_page_renders() {
    let app = app(false).await;
    let (status, _, text) = send_html(&app, "GET", "/login", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("Sign in"));
}

#[tokio::test]
async fn web_dashboard_redirects_when_unauthenticated() {
    let app = app(false).await;
    let (status, headers, _) = send_html(&app, "GET", "/", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").and_then(|v| v.to_str().ok()),
        Some("/login")
    );
}

#[tokio::test]
async fn web_login_rejects_wrong_password() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let (status, _, text) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, "application/x-www-form-urlencoded")],
        "username=alice&password=wrong",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("Invalid username or password"));
}

#[tokio::test]
async fn web_login_sets_cookie_and_redirects() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, "application/x-www-form-urlencoded")],
        "username=alice&password=secret",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers.get("location").and_then(|v| v.to_str().ok()),
        Some("/")
    );
    assert!(headers.get("set-cookie").is_some());
}

#[tokio::test]
async fn web_dashboard_shows_paginated_progress() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 25).await;

    let (_, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, "application/x-www-form-urlencoded")],
        "username=alice&password=secret",
    )
    .await;
    let cookie = headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let (status, _, text) = send_html(&app, "GET", "/", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("doc0"));
    assert!(text.contains("Page 1 of 2"));
    assert!(text.contains(r#"href="/documents/doc0""#));
    assert!(text.contains("<time datetime="));

    let (status, _, text) =
        send_html(&app, "GET", "/?page=2", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("doc20"));
}

#[tokio::test]
async fn web_logout_clears_session() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let (_, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, "application/x-www-form-urlencoded")],
        "username=alice&password=secret",
    )
    .await;
    let cookie = headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let (status, _, _) =
        send_html(&app, "GET", "/logout", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn web_dashboard_search_filters_documents() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 25).await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, text) = send_html(
        &app,
        "GET",
        "/?query=doc1",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("doc1"));
    assert!(!text.contains("doc0"));
}

#[tokio::test]
async fn web_dashboard_status_filter() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 3).await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, text) = send_html(
        &app,
        "GET",
        "/?status=finished",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!text.contains("doc0"));

    let (status, _, text) = send_html(
        &app,
        "GET",
        "/?status=in-progress",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("doc0"));
}

#[tokio::test]
async fn web_help_page_is_public() {
    let app = app(false).await;
    let (status, _, text) = send_html(&app, "GET", "/help", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("OPDS"));
}

#[tokio::test]
async fn web_dashboard_shows_reading_stats() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 3).await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, text) = send_html(&app, "GET", "/", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("Reading statistics"));
    assert!(text.contains("Sync activity"));
    assert!(text.contains(r#"class="sparkline""#));
}

#[tokio::test]
async fn web_dashboard_hides_widgets_when_filtering() {
    let app = app(false).await;
    seed_documents(&app, "alice", "secret", 3).await;
    let cookie = login(&app, "alice", "secret").await;

    let (_, _, text) = send_html(
        &app,
        "GET",
        "/?query=doc1",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert!(!text.contains("Reading statistics"));
}

#[tokio::test]
async fn web_continue_reading_is_independent_of_the_listing_page() {
    let app = app(false).await;
    // 21 untitled documents fill the first page (20 rows) when sorted by
    // title, pushing the titled document onto page 2. The total stays within
    // the rail's scan window, since every sync lands in the same second.
    seed_documents(&app, "alice", "secret", 21).await;
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
        r#"{"document":"zebra","progress":"/p[1]","percentage":0.3,"device":"kobo","device_id":"k1","metadata":{"title":"Zebra Tales","authors":"Z. Author"}}"#,
    )
    .await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, text) = send_html(
        &app,
        "GET",
        "/?sort=title",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.contains("Continue reading"));
    assert!(text.contains("Zebra Tales"));
    assert!(text.contains(r#"href="/documents/zebra""#));
}
