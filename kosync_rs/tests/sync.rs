//! Tests for the `KOReader` sync protocol endpoints.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{
    JSON, app, auth_headers, basic, build_epub, md5, multipart, register, send, send_bytes,
};

/// Reference `KOReader` partial MD5: samples of 1 KiB at offsets 0, 1024, 4096, ...
fn reference_partial_md5(data: &[u8]) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    let mut offset = 0usize;
    for _ in 0..12 {
        if offset >= data.len() {
            break;
        }
        hasher.update(&data[offset..(offset + 1024).min(data.len())]);
        offset = if offset == 0 { 1024 } else { offset * 4 };
    }
    format!("{:x}", hasher.finalize())
}

/// Upload `data` as `file_name` through the OPDS API and return its document hash.
async fn upload_book(app: &Router, file_name: &str, data: &[u8]) -> String {
    let (body, boundary) = multipart(file_name, "application/epub+zip", data);
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

/// `PUT` progress for `document` as the test user.
async fn put_progress(app: &Router, document: &str, progress: &str, percentage: f64) {
    let mut headers = auth_headers();
    headers.push((JSON, "application/json"));
    let body = format!(
        r#"{{"document":"{document}","progress":"{progress}","percentage":{percentage},"device":"CrossPoint"}}"#
    );
    let (status, _) = send(app, "PUT", "/syncs/progress", &headers, &body).await;
    assert_eq!(status, StatusCode::OK);
}

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

#[tokio::test]
async fn uploaded_book_digest_matches_koreader_partial_md5() {
    let app = app(false).await;
    let epub = build_epub(None);
    let hash = upload_book(&app, "book.epub", &epub).await;
    assert_eq!(hash, reference_partial_md5(&epub));
}

#[tokio::test]
async fn filename_hash_progress_is_visible_under_binary_hash() {
    let app = app(false).await;
    register(&app).await;
    let epub = build_epub(None);
    let binary = upload_book(&app, "book.epub", &epub).await;
    let by_name = md5("book.epub");

    put_progress(&app, &by_name, "/body/DocFragment[3]/body", 0.4).await;

    let (status, body) = send(
        &app,
        "GET",
        &format!("/syncs/progress/{binary}"),
        &auth_headers(),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["document"], binary.as_str());
    assert_eq!(body["progress"], "/body/DocFragment[3]/body");
    assert_eq!(body["percentage"], 0.4);
}

#[tokio::test]
async fn binary_hash_progress_is_visible_under_filename_hash() {
    let app = app(false).await;
    register(&app).await;
    let epub = build_epub(None);
    let binary = upload_book(&app, "book.epub", &epub).await;
    let by_name = md5("book.epub");

    put_progress(&app, &binary, "/body/DocFragment[2]/body", 0.2).await;

    let (_, body) = send(
        &app,
        "GET",
        &format!("/syncs/progress/{by_name}"),
        &auth_headers(),
        "",
    )
    .await;
    assert_eq!(body["document"], by_name.as_str());
    assert_eq!(body["progress"], "/body/DocFragment[2]/body");
}

#[tokio::test]
async fn newest_alias_progress_wins() {
    let app = app(false).await;
    register(&app).await;
    let binary = upload_book(&app, "book.epub", &build_epub(None)).await;
    let by_name = md5("book.epub");

    put_progress(&app, &binary, "/body/DocFragment[1]/body", 0.1).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    put_progress(&app, &by_name, "/body/DocFragment[5]/body", 0.9).await;

    let (_, body) = send(
        &app,
        "GET",
        &format!("/syncs/progress/{binary}"),
        &auth_headers(),
        "",
    )
    .await;
    assert_eq!(body["progress"], "/body/DocFragment[5]/body");
}

#[tokio::test]
async fn update_progress_rejects_out_of_range_percentage() {
    let app = app(false).await;
    register(&app).await;
    let mut headers = auth_headers();
    headers.push((JSON, "application/json"));
    for percentage in ["1.5", "-0.1"] {
        let body = format!(
            r#"{{"document":"doc1","progress":"/body/p[1]","percentage":{percentage},"device":"kindle"}}"#
        );
        let (status, _) = send(&app, "PUT", "/syncs/progress", &headers, &body).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
