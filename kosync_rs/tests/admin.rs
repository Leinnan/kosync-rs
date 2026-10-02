//! Tests for the administrator user-management web pages.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{app, login, register_plaintext, send_html};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";

#[tokio::test]
async fn admin_panel_lists_and_creates_users() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        "/admin/users",
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Create account"));
    assert!(body.contains("alice"));

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/admin/users/create",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "username=bob&password=secret&can_upload=on",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // The new account can log in with the password set by the admin.
    let bob = login(&app, "bob", "secret").await;
    assert!(!bob.is_empty());
}

#[tokio::test]
async fn non_admin_cannot_access_admin_panel() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, _) = send_html(
        &app,
        "GET",
        "/admin/users",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Unauthenticated visitors are redirected too.
    let (status, _, _) = send_html(&app, "GET", "/admin/users", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn admin_account_is_protected() {
    let app = app(false).await;
    let admin = login(&app, "admin", "admin").await;

    for action in ["active", "can-upload", "password", "delete"] {
        let (status, headers, _) = send_html(
            &app,
            "POST",
            &format!("/admin/users/admin/{action}"),
            &[("cookie", admin.as_str()), ("content-type", FORM)],
            "password=whatever",
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = headers.get("location").unwrap().to_str().unwrap();
        assert!(
            location.contains("error=protected"),
            "{action} should protect the admin account, got {location}"
        );
    }

    let again = login(&app, "admin", "admin").await;
    assert!(!again.is_empty());
}

#[tokio::test]
async fn admin_can_revoke_and_restore_upload_permission() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;
    let alice = login(&app, "alice", "secret").await;

    let (status, _, _) = send_html(
        &app,
        "GET",
        "/books/upload",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/admin/users/alice/can-upload",
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, _, _) = send_html(
        &app,
        "GET",
        "/books/upload",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn admin_can_reset_password() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/admin/users/alice/password",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "password=newpass",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let cookie = login(&app, "alice", "newpass").await;
    assert!(!cookie.is_empty());
}

#[tokio::test]
async fn admin_can_delete_user() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/admin/users/alice/delete",
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[("content-type", FORM)],
        "username=alice&password=secret",
    )
    .await;
    assert!(headers.get("set-cookie").is_none());
}
