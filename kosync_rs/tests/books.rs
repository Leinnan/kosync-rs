//! Tests for the web book listing, upload, detail, download, and delete pages.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{
    JSON, app, basic, build_epub, build_epub_with_metadata, login, md5, multipart,
    register_plaintext, send, send_bytes, send_html,
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

/// Upload an `EPUB` via the web form as the given logged-in user.
async fn web_upload(app: &Router, cookie: &str, epub: &[u8]) -> StatusCode {
    let (body, boundary) = multipart("book.epub", "application/epub+zip", epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        app,
        "POST",
        "/books/upload",
        &[("content-type", content_type.as_str()), ("cookie", cookie)],
        body,
    )
    .await;
    status
}

#[tokio::test]
async fn books_page_requires_login() {
    let app = app(false).await;
    let (status, _, _) = send_html(&app, "GET", "/books", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn navbar_shows_books_link() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, body) =
        send_html(&app, "GET", "/books", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/books"));
}

#[tokio::test]
async fn admin_uploads_and_lists_book() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let epub = build_epub(None);
    let status = web_upload(&app, &admin, &epub).await;
    assert_eq!(status, StatusCode::OK);

    let (_, _, body) = send_html(&app, "GET", "/books", &[("cookie", admin.as_str())], "").await;
    assert!(body.contains("Test Book"));

    let alice = login(&app, "alice", "secret").await;
    let (_, _, body) = send_html(&app, "GET", "/books", &[("cookie", alice.as_str())], "").await;
    assert!(body.contains("Test Book"));
}

#[tokio::test]
async fn upload_page_requires_upload_permission() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let alice = login(&app, "alice", "secret").await;

    // New accounts are allowed to upload by default.
    let (status, _, _) = send_html(
        &app,
        "GET",
        "/books/upload",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Revoke the permission via the management API.
    let admin_key = md5("admin");
    let (status, _) = send(
        &app,
        "PUT",
        "/manage/users/can-upload?username=alice",
        &[("x-auth-user", "admin"), ("x-auth-key", admin_key.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

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
async fn book_detail_shows_metadata_and_progress() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let document_hash = upload_id(&app, &epub).await;

    let key = md5("secret");
    let body = format!(
        r#"{{"document":"{document_hash}","progress":"/body/p[1]","percentage":0.42,"device":"kindle","device_id":"dev1"}}"#
    );
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &[
            ("x-auth-user", "alice"),
            ("x-auth-key", key.as_str()),
            (JSON, "application/json"),
        ],
        &body,
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{document_hash}"),
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Test Book"));
    assert!(body.contains("Alice Author"));
    assert!(body.contains("Your reading progress"));
    assert!(body.contains("<time datetime="));
    assert!(body.contains("Technical information"));
    assert!(!body.contains("delete_modal"));
}

#[tokio::test]
async fn download_and_thumbnail_via_web() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let mut cover = Vec::new();
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
    img.write_to(
        &mut std::io::Cursor::new(&mut cover),
        image::ImageFormat::Png,
    )
    .unwrap();

    let epub = build_epub(Some(&cover));
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let document_hash = upload_id(&app, &epub).await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/books/{document_hash}/download"),
        &[("cookie", alice.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, epub);

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/books/{document_hash}/thumbnail"),
        &[("cookie", alice.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!bytes.is_empty());
}

#[tokio::test]
async fn book_grid_uses_semantic_cards_without_actions() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;
    let epub = build_epub(None);
    web_upload(&app, &admin, &epub).await;

    let (status, _, body) =
        send_html(&app, "GET", "/books", &[("cookie", admin.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("class=\"book-card\""));
    assert!(body.contains("cover-container"));
    assert!(!body.contains("/download"));
    assert!(!body.contains("Delete"));
}

#[tokio::test]
async fn admin_book_detail_has_dialog_and_time_tags() {
    let app = app(false).await;
    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let document_hash = upload_id(&app, &epub).await;

    let (status, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{document_hash}"),
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<dialog id=\"delete_modal\""));
    assert!(body.contains("Download EPUB"));
    assert!(body.contains("<time datetime="));
    assert!(body.contains("Technical information"));
}

#[tokio::test]
async fn admin_delete_removes_book() {
    let app = app(false).await;
    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let document_hash = upload_id(&app, &epub).await;

    let (status, _, _) = send_bytes(
        &app,
        "POST",
        &format!("/books/{document_hash}/delete"),
        &[("cookie", admin.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (_, _, body) = send_html(&app, "GET", "/books", &[("cookie", admin.as_str())], "").await;
    assert!(!body.contains("Test Book"));
}

#[tokio::test]
async fn document_maps_to_book_by_filename_hash() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let book_hash = upload_id(&app, &epub).await;

    let filename_digest = md5("book.epub");
    let key = md5("secret");
    let payload = format!(
        r#"{{"document":"{filename_digest}","progress":"/body/p[1]","percentage":0.42,"device":"kindle","device_id":"dev1"}}"#
    );
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &[
            ("x-auth-user", "alice"),
            ("x-auth-key", key.as_str()),
            (JSON, "application/json"),
        ],
        &payload,
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, html) = send_html(&app, "GET", "/", &[("cookie", alice.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Test Book"));
    assert!(html.contains(&format!("href=\"/books/{book_hash}\"")));
    assert!(html.contains(&format!("href=\"/documents/{filename_digest}\"")));

    let (status, _, html) = send_html(
        &app,
        "GET",
        &format!("/documents/{filename_digest}"),
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Test Book"));
}

#[tokio::test]
async fn unmatched_document_still_shows_raw_hash() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

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
        r#"{"document":"no-match","progress":"/body/p[1]","percentage":0.1,"device":"kindle","device_id":"dev1"}"#,
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, html) = send_html(&app, "GET", "/", &[("cookie", alice.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(r#"href="/documents/no-match""#));
    assert!(html.contains("<code>no-match</code>"));
}

#[tokio::test]
async fn sync_metadata_is_displayed() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

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
        r#"{"document":"meta-doc","progress":"/body/p[1]","percentage":0.5,"device":"kindle","device_id":"dev1","metadata":{"filename":"mybook.epub","title":"My Book","authors":"Jane Doe"}}"#,
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, html) = send_html(&app, "GET", "/", &[("cookie", alice.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("My Book"));
    assert!(html.contains("Jane Doe"));
}

#[tokio::test]
async fn literal_filename_matches_uploaded_book() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let (body, boundary) = multipart("mybook.epub", "application/epub+zip", &epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        &app,
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
        r#"{"document":"not-the-real-hash","progress":"/body/p[1]","percentage":0.5,"device":"kindle","device_id":"dev1","metadata":{"filename":"mybook.epub"}}"#,
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (status, _, html) = send_html(&app, "GET", "/", &[("cookie", alice.as_str())], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Test Book"));
}

/// Upload an `EPUB` under a specific file name as the given logged-in user.
async fn upload_named(app: &Router, cookie: &str, filename: &str, epub: &[u8]) {
    let (body, boundary) = multipart(filename, "application/epub+zip", epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        app,
        "POST",
        "/books/upload",
        &[("content-type", content_type.as_str()), ("cookie", cookie)],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn books_reading_status_filter_tracks_progress() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let epub = build_epub(None);
    let admin = login(&app, "admin", "admin").await;
    web_upload(&app, &admin, &epub).await;
    let document_hash = upload_id(&app, &epub).await;

    let alice = login(&app, "alice", "secret").await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?status=unread",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(body.contains("Test Book"));

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
        &format!(
            r#"{{"document":"{document_hash}","progress":"/p[1]","percentage":0.4,"device":"kindle","device_id":"d"}}"#
        ),
    )
    .await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?status=unread",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(!body.contains("Test Book"));

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?status=in-progress",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(body.contains("Test Book"));

    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?status=finished",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(!body.contains("Test Book"));
}

#[tokio::test]
async fn books_genre_filter_limits_results() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let fantasy =
        build_epub_with_metadata("Fantasy Book", "Author One", &["Fantasy"], None, None, None);
    let science =
        build_epub_with_metadata("Science Book", "Author Two", &["Science"], None, None, None);
    upload_named(&app, &admin, "fantasy.epub", &fantasy).await;
    upload_named(&app, &admin, "science.epub", &science).await;

    let alice = login(&app, "alice", "secret").await;
    let (_, _, body) = send_html(
        &app,
        "GET",
        "/books?genre=Fantasy",
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(body.contains("Fantasy Book"));
    assert!(!body.contains("Science Book"));
}

#[tokio::test]
async fn help_page_renders_opds_instructions() {
    let app = app(false).await;
    let (status, _, body) = send_html(&app, "GET", "/help", &[], "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("/opds/"));
}

#[tokio::test]
async fn book_detail_links_to_sync_history_when_progress_exists() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let epub = build_epub(None);
    let document_hash = upload_id(&app, &epub).await;

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
        &format!(
            r#"{{"document":"{document_hash}","progress":"/p[1]","percentage":0.5,"device":"kindle","device_id":"d"}}"#
        ),
    )
    .await;

    let alice = login(&app, "alice", "secret").await;
    let (_, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{document_hash}"),
        &[("cookie", alice.as_str())],
        "",
    )
    .await;
    assert!(body.contains("View sync history"));
    assert!(body.contains(&format!("href=\"/documents/{document_hash}\"")));
}

#[tokio::test]
async fn book_detail_without_progress_has_no_history_link() {
    let app = app(false).await;
    let epub = build_epub(None);
    let document_hash = upload_id(&app, &epub).await;
    let admin = login(&app, "admin", "admin").await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{document_hash}"),
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert!(!body.contains("View sync history"));
    assert!(body.contains("Not started"));
}

#[tokio::test]
async fn book_detail_shows_other_books_in_the_series() {
    let app = app(false).await;
    let first = build_epub_with_metadata(
        "The First Voyage",
        "Sea Author",
        &[],
        None,
        Some(("Ocean Saga", 1.0)),
        None,
    );
    let second = build_epub_with_metadata(
        "The Second Voyage",
        "Sea Author",
        &[],
        None,
        Some(("Ocean Saga", 2.0)),
        None,
    );
    let first_hash = upload_id(&app, &first).await;
    upload_id(&app, &second).await;
    let admin = login(&app, "admin", "admin").await;

    let (_, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{first_hash}"),
        &[("cookie", admin.as_str())],
        "",
    )
    .await;
    assert!(body.contains("More in Ocean Saga"));
    assert!(body.contains("The Second Voyage"));
    assert!(body.contains("Book 1 of"));
    assert!(body.contains("book-card is-current"));
}
