//! Tests for the self-service settings page.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{app, login, register_plaintext, send_html};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";

#[tokio::test]
async fn settings_requires_login() {
    let app = app(false).await;
    let (status, _, _) = send_html(&app, "GET", "/settings", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn user_can_change_own_password() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/settings/password",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "current_password=secret&new_password=newpass",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers.get("location").unwrap().to_str().unwrap();
    assert!(location.contains("notice=password-changed"), "{location}");

    let new_cookie = login(&app, "alice", "newpass").await;
    assert!(!new_cookie.is_empty());
}

#[tokio::test]
async fn wrong_current_password_is_rejected() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/settings/password",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "current_password=wrong&new_password=newpass",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers.get("location").unwrap().to_str().unwrap();
    assert!(location.contains("error=wrong-current"), "{location}");

    // The old password still works and the rejected one does not.
    let cookie = login(&app, "alice", "secret").await;
    assert!(!cookie.is_empty());
    let (_, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[("content-type", FORM)],
        "username=alice&password=newpass",
    )
    .await;
    assert!(headers.get("set-cookie").is_none());
}

#[tokio::test]
async fn non_admin_cannot_reset_another_user() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    register_plaintext(&app, "bob", "secret").await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/settings/user-password",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "username=bob&password=reset",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Bob's password is unchanged.
    let bob = login(&app, "bob", "secret").await;
    assert!(!bob.is_empty());
}

#[tokio::test]
async fn admin_can_reset_another_user_from_settings() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/settings/user-password",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "username=alice&password=reset",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers.get("location").unwrap().to_str().unwrap();
    assert!(location.contains("notice=password-reset"), "{location}");

    let cookie = login(&app, "alice", "reset").await;
    assert!(!cookie.is_empty());
}

#[tokio::test]
async fn settings_admin_form_cannot_reset_admin() {
    let app = app(false).await;
    let admin = login(&app, "admin", "admin").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/settings/user-password",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "username=admin&password=whatever",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers.get("location").unwrap().to_str().unwrap();
    assert!(location.contains("error=protected"), "{location}");

    let again = login(&app, "admin", "admin").await;
    assert!(!again.is_empty());
}

#[tokio::test]
async fn admin_settings_hides_admin_from_targets() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (status, _, body) =
        send_html(&app, "GET", "/settings", &[("cookie", admin.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Reset another user"));
    assert!(body.contains("alice"));
    assert!(!body.contains("<option value=\"admin\">"));
}
