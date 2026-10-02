//! Tests for the `KOReader` sync protocol endpoints.

#![allow(clippy::unwrap_used)]

mod common;

use axum::http::StatusCode;

use common::{JSON, app, auth_headers, register, send};

#[tokio::test]
async fn healthcheck_returns_ok() {
    let app = app(false).await;
    let (status, body) = send(&app, "GET", "/healthcheck", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "OK");
}

#[tokio::test]
async fn create_user_returns_201() {
    let app = app(false).await;
    let (status, body) = send(
        &app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        r#"{"username":"bob","password":"secret"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["username"], "bob");
}

#[tokio::test]
async fn create_duplicate_user_returns_402() {
    let app = app(false).await;
    register(&app).await;
    let (status, body) = send(
        &app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        r#"{"username":"alice","password":"other"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["code"], 2002);
}

#[tokio::test]
async fn create_user_rejects_invalid_username() {
    let app = app(false).await;
    let (status, body) = send(
        &app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        r#"{"username":"has:colon","password":"secret"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], 2003);
}

#[tokio::test]
async fn registration_disabled_returns_402() {
    let app = app(true).await;
    let (status, body) = send(
        &app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        r#"{"username":"bob","password":"secret"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(body["code"], 2005);
}

#[tokio::test]
async fn auth_succeeds_with_correct_key() {
    let app = app(false).await;
    register(&app).await;
    let (status, body) = send(&app, "GET", "/users/auth", &auth_headers(), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authorized"], "OK");
}

#[tokio::test]
async fn auth_rejects_wrong_key() {
    let app = app(false).await;
    register(&app).await;
    let headers = vec![("x-auth-user", "alice"), ("x-auth-key", "wrong")];
    let (status, body) = send(&app, "GET", "/users/auth", &headers, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], 2001);
}

#[tokio::test]
async fn update_progress_requires_auth() {
    let app = app(false).await;
    register(&app).await;
    let headers = vec![
        (JSON, "application/json"),
        ("x-auth-user", "alice"),
        ("x-auth-key", "wrong"),
    ];
    let (status, _) = send(
        &app,
        "PUT",
        "/syncs/progress",
        &headers,
        r#"{"document":"doc1","progress":"/body/p[1]","percentage":0.32,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn update_and_get_progress_roundtrip() {
    let app = app(false).await;
    register(&app).await;

    let mut headers = auth_headers();
    headers.push((JSON, "application/json"));
    let (status, body) = send(
        &app,
        "PUT",
        "/syncs/progress",
        &headers,
        r#"{"document":"doc1","progress":"/body/p[1]","percentage":0.32,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["document"], "doc1");
    assert!(body["timestamp"].is_i64());

    let (status, body) = send(&app, "GET", "/syncs/progress/doc1", &auth_headers(), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["document"], "doc1");
    assert_eq!(body["progress"], "/body/p[1]");
    assert_eq!(body["percentage"], 0.32);
    assert_eq!(body["device"], "kindle");
    assert_eq!(body["device_id"], "dev1");
}

#[tokio::test]
async fn get_missing_document_returns_empty_object() {
    let app = app(false).await;
    register(&app).await;
    let (status, body) = send(&app, "GET", "/syncs/progress/nope", &auth_headers(), "").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));
}

#[tokio::test]
async fn latest_progress_wins() {
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
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &headers,
        r#"{"document":"doc1","progress":"/body/p[2]","percentage":0.55,"device":"kobo","device_id":"dev2"}"#,
    )
    .await;

    let (_, body) = send(&app, "GET", "/syncs/progress/doc1", &auth_headers(), "").await;
    assert_eq!(body["progress"], "/body/p[2]");
    assert_eq!(body["percentage"], 0.55);
    assert_eq!(body["device"], "kobo");
}
