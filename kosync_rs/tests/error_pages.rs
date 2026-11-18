//! Error pages: browser errors render in the site's design, in the visitor's
//! language, while API clients keep their bare responses.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::{StatusCode, header};

use common::{JSON, login, send_html};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";
/// What a browser sends in `Accept` for a page.
const BROWSER: &str = "text/html,application/xhtml+xml,*/*;q=0.8";

#[tokio::test]
async fn a_rejected_login_shows_the_styled_403_page() {
    let app = common::app(false).await;
    let (status, headers, body) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, FORM)],
        "username=admin&password=admin",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
    assert!(body.contains("app.css"), "{body}");
    assert!(body.contains("auth-card error-card"), "{body}");
    assert!(body.contains("Access denied"), "{body}");
    assert!(body.contains("invalid CSRF token"), "{body}");
    assert!(!body.contains("Return home"), "{body}");
}

#[tokio::test]
async fn unknown_pages_show_the_styled_404_page_to_browsers() {
    let app = common::app(false).await;
    let (status, _, body) =
        send_html(&app, "GET", "/no-such-page", &[("accept", BROWSER)], "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Page not found"), "{body}");
    assert!(body.contains("data-history-back"), "{body}");
}

#[tokio::test]
async fn unknown_paths_stay_bare_for_api_clients() {
    let app = common::app(false).await;
    for accept in [
        "application/vnd.koreader.v1+json",
        "application/json",
        "*/*",
    ] {
        let (status, _, body) =
            send_html(&app, "GET", "/no-such-page", &[("accept", accept)], "").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{accept}");
        assert!(body.is_empty(), "{accept}: {body}");
    }
}

#[tokio::test]
async fn a_signed_in_error_page_keeps_the_navigation() {
    let app = common::app(false).await;
    let cookie = login(&app, "admin", "admin").await;
    let (status, _, body) = send_html(
        &app,
        "GET",
        "/no-such-page",
        &[("accept", BROWSER), ("cookie", cookie.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("primary-nav"), "{body}");
    assert!(body.contains("Page not found"), "{body}");
}

#[tokio::test]
async fn error_pages_follow_the_visitor_language() {
    let app = common::app(false).await;
    let (status, _, body) = send_html(
        &app,
        "GET",
        "/no-such-page",
        &[("accept", BROWSER), ("accept-language", "pl")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("Nie znaleziono strony"), "{body}");
    assert!(body.contains(r#"lang="pl""#), "{body}");
}
