//! Tests for the management API endpoints.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{JSON, app, auth_headers, md5, register, send};

#[tokio::test]
async fn admin_is_seeded_and_can_list_users() {
    let app = app(false).await;

    let admin_key = md5("admin");
    let headers = vec![("x-auth-user", "admin"), ("x-auth-key", admin_key.as_str())];

    let (status, body) = send(&app, "GET", "/manage/users", &headers, "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    assert_eq!(body[0]["username"], "admin");
    assert_eq!(body[0]["is_administrator"], true);
}

#[tokio::test]
async fn non_admin_cannot_list_users() {
    let app = app(false).await;
    register(&app).await;
    let (status, _) = send(&app, "GET", "/manage/users", &auth_headers(), "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn user_can_list_own_documents() {
    let app = app(false).await;
    register(&app).await;

    let mut headers = auth_headers();
    headers.push((JSON, "application/json"));
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &headers,
        r#"{"document":"doc1","progress":"/body/p[1]","percentage":0.32,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;

    let (status, body) = send(
        &app,
        "GET",
        "/manage/users/documents?username=alice",
        &auth_headers(),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    assert_eq!(body[0]["document_hash"], "doc1");
}

#[tokio::test]
async fn admin_can_toggle_upload_permission() {
    let app = app(false).await;
    register(&app).await;

    let admin_key = md5("admin");
    let headers = vec![("x-auth-user", "admin"), ("x-auth-key", admin_key.as_str())];

    // Accounts are allowed to upload by default.
    let (status, body) = send(&app, "GET", "/manage/users", &headers, "").await;
    assert_eq!(status, StatusCode::OK);
    let alice = body
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["username"] == "alice")
        .unwrap();
    assert_eq!(alice["can_upload"], true);

    let (status, _) = send(
        &app,
        "PUT",
        "/manage/users/can-upload?username=alice",
        &headers,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&app, "GET", "/manage/users", &headers, "").await;
    assert_eq!(status, StatusCode::OK);
    let alice = body
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["username"] == "alice")
        .unwrap();
    assert_eq!(alice["can_upload"], false);
}

#[tokio::test]
async fn admin_cannot_toggle_own_upload_permission() {
    let app = app(false).await;
    let admin_key = md5("admin");
    let headers = vec![("x-auth-user", "admin"), ("x-auth-key", admin_key.as_str())];

    let (status, _) = send(
        &app,
        "PUT",
        "/manage/users/can-upload?username=admin",
        &headers,
        "",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_can_create_user_without_upload_permission() {
    let app = app(false).await;
    let admin_key = md5("admin");
    let headers = vec![
        ("x-auth-user", "admin"),
        ("x-auth-key", admin_key.as_str()),
        (JSON, "application/json"),
    ];

    let (status, _) = send(
        &app,
        "POST",
        "/manage/users",
        &headers,
        r#"{"username":"bob","password":"secret","can_upload":false}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &app,
        "GET",
        "/manage/users",
        &[("x-auth-user", "admin"), ("x-auth-key", admin_key.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let bob = body
        .as_array()
        .unwrap()
        .iter()
        .find(|user| user["username"] == "bob")
        .unwrap();
    assert_eq!(bob["can_upload"], false);
}
