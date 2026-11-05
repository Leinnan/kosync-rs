//! Tests for `MOBI` uploads, which are converted to `EPUB` and stored as such.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Cursor;

use axum::{Router, http::StatusCode};

use common::{app, basic, build_mobi, login, multipart, send_bytes, send_html};

/// Upload a file as the admin user and return the single result it reports.
async fn upload(app: &Router, file_name: &str, data: &[u8]) -> serde_json::Value {
    let (body, boundary) = multipart(file_name, "application/x-mobipocket-ebook", data);
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
    let response: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    response["results"][0].clone()
}

/// Fetch a publication resource as the admin user.
async fn fetch(app: &Router, hash: &str, resource: &str) -> (StatusCode, Vec<u8>) {
    let (status, _, bytes) = send_bytes(
        app,
        "GET",
        &format!("/opds/v2/publications/{hash}/{resource}"),
        &[("authorization", basic("admin", "admin").as_str())],
        Vec::new(),
    )
    .await;
    (status, bytes)
}

/// A small valid picture.
fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::RgbImage::new(4, 6)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

#[tokio::test]
async fn upload_is_stored_and_served_as_epub() {
    let app = app(false).await;

    let result = upload(
        &app,
        "my-book.mobi",
        &build_mobi("Converted Book", "Mo Bi", None, false),
    )
    .await;
    assert_eq!(result["status"], "imported");
    assert_eq!(result["fileName"], "my-book.mobi");
    let hash = result["id"].as_str().unwrap();

    let (status, bytes) = fetch(&app, hash, "file").await;
    assert_eq!(status, StatusCode::OK);
    let doc = epub::doc::EpubDoc::from_reader(Cursor::new(bytes)).unwrap();
    assert_eq!(doc.get_title().as_deref(), Some("Converted Book"));

    let admin = login(&app, "admin", "admin").await;
    let (_, _, page) = send_html(&app, "GET", "/books", &[("cookie", admin.as_str())], "").await;
    assert!(page.contains("Converted Book"));
}

#[tokio::test]
async fn cover_survives_conversion() {
    let app = app(false).await;
    let cover = png();

    let result = upload(
        &app,
        "book.mobi",
        &build_mobi("Covered", "Mo Bi", Some(&cover), false),
    )
    .await;
    let hash = result["id"].as_str().unwrap();

    let (status, bytes) = fetch(&app, hash, "cover").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, cover);
    let (status, _) = fetch(&app, hash, "thumbnail").await;
    assert_eq!(status, StatusCode::OK);

    let (_, epub) = fetch(&app, hash, "file").await;
    let mut doc = epub::doc::EpubDoc::from_reader(Cursor::new(epub)).unwrap();
    assert_eq!(doc.get_cover().unwrap().0, cover);
}

#[tokio::test]
async fn repeated_upload_is_a_duplicate_until_deleted() {
    let app = app(false).await;
    let data = build_mobi("Twice", "Mo Bi", None, false);

    let first = upload(&app, "twice.mobi", &data).await;
    assert_eq!(first["status"], "imported");

    let second = upload(&app, "renamed.mobi", &data).await;
    assert_eq!(second["status"], "duplicate");
    assert_eq!(second["id"], first["id"]);

    let (status, _, _) = send_bytes(
        &app,
        "DELETE",
        &format!("/opds/v2/publications/{}", first["id"].as_str().unwrap()),
        &[("authorization", basic("admin", "admin").as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let third = upload(&app, "twice.mobi", &data).await;
    assert_eq!(third["status"], "imported");
}

#[tokio::test]
async fn encrypted_book_is_rejected() {
    let app = app(false).await;

    let result = upload(
        &app,
        "locked.mobi",
        &build_mobi("Locked", "Mo Bi", None, true),
    )
    .await;
    assert_eq!(result["status"], "error");
    assert_eq!(result["message"], "DRM-protected books cannot be converted");
}

#[tokio::test]
async fn damaged_book_is_rejected() {
    let app = app(false).await;
    let mut data = build_mobi("Damaged", "Mo Bi", None, false);
    data.truncate(80);

    let result = upload(&app, "damaged.mobi", &data).await;
    assert_eq!(result["status"], "error");
    assert_eq!(result["message"], "Not a valid MOBI file");
}

#[tokio::test]
async fn file_named_mobi_with_other_content_is_rejected() {
    let app = app(false).await;

    let result = upload(&app, "fake.mobi", b"just some text").await;
    assert_eq!(result["status"], "error");
}
