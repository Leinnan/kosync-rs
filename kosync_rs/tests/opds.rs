//! Tests for the OPDS catalog, upload, download, and cover endpoints.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::Cursor;

use axum::{Router, http::StatusCode};

use common::{
    JSON, app, basic, build_epub, build_epub_with_metadata, md5, multipart, register_plaintext,
    send, send_bytes,
};

/// Parse a response body as JSON.
fn as_json(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap()
}

/// Upload an `EPUB` as the admin user and return the parsed response body.
async fn admin_upload(app: &Router, epub: &[u8]) -> serde_json::Value {
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
    assert_eq!(status, StatusCode::OK, "upload should succeed");
    as_json(&bytes)
}

#[tokio::test]
async fn opds_feed_requires_auth() {
    let app = app(false).await;
    let (status, headers, _) = send_bytes(&app, "GET", "/opds/v2/", &[], Vec::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.get("www-authenticate").is_some());
}

#[tokio::test]
async fn upload_requires_upload_permission() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    // New accounts are allowed to upload by default.
    let epub = build_epub(None);
    let (body, boundary) = multipart("book.epub", "application/epub+zip", &epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        &app,
        "POST",
        "/opds/upload",
        &[
            ("content-type", content_type.as_str()),
            ("authorization", basic("alice", "secret").as_str()),
        ],
        body,
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

    let (body, boundary) = multipart("book2.epub", "application/epub+zip", &build_epub(None));
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, _) = send_bytes(
        &app,
        "POST",
        "/opds/upload",
        &[
            ("content-type", content_type.as_str()),
            ("authorization", basic("alice", "secret").as_str()),
        ],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn upload_feed_download_roundtrip() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let result = admin_upload(&app, &epub).await;
    assert_eq!(result["results"][0]["status"], "imported");
    let document_hash = result["results"][0]["id"].as_str().unwrap().to_owned();

    let auth = basic("alice", "secret");
    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/publications",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let feed = as_json(&bytes);
    assert_eq!(feed["metadata"]["numberOfItems"], 1);
    assert_eq!(feed["publications"][0]["metadata"]["title"], "Test Book");
    assert_eq!(
        feed["publications"][0]["metadata"]["author"][0]["name"],
        "Alice Author"
    );

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/opds/v2/publications/{document_hash}"),
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let publication = as_json(&bytes);
    assert_eq!(publication["metadata"]["title"], "Test Book");

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/opds/v2/publications/{document_hash}/file"),
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, epub);

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/publications",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let atom = String::from_utf8_lossy(&bytes);
    assert!(atom.contains("Test Book"));
    assert!(atom.contains("<feed"));
}

#[tokio::test]
async fn duplicate_upload_is_rejected() {
    let app = app(false).await;
    let epub = build_epub(None);

    let result = admin_upload(&app, &epub).await;
    assert_eq!(result["results"][0]["status"], "imported");

    let result = admin_upload(&app, &epub).await;
    assert_eq!(result["results"][0]["status"], "duplicate");
}

#[tokio::test]
async fn invalid_epub_is_rejected() {
    let app = app(false).await;
    let result = admin_upload(&app, b"this is not an epub").await;
    assert_eq!(result["results"][0]["status"], "error");
}

#[tokio::test]
async fn pdf_upload_is_rejected() {
    let app = app(false).await;
    let (body, boundary) = multipart("paper.pdf", "application/pdf", b"%PDF-1.4 not really");
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, bytes) = send_bytes(
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
    let result = as_json(&bytes);
    assert_eq!(result["results"][0]["status"], "error");
    assert_eq!(result["results"][0]["message"], "Not a valid EPUB file");
}

#[tokio::test]
async fn uploaded_book_matches_synced_progress() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let result = admin_upload(&app, &epub).await;
    let document_hash = result["results"][0]["id"].as_str().unwrap().to_owned();

    let body = format!(
        r#"{{"document":"{document_hash}","progress":"/body/p[1]","percentage":0.42,"device":"kindle","device_id":"dev1"}}"#
    );
    let key = md5("secret");
    let (status, _) = send(
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
    assert_eq!(status, StatusCode::OK);

    let auth = basic("alice", "secret");
    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/publications",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let feed = as_json(&bytes);
    let kosync = &feed["publications"][0]["metadata"]["kosync"];
    assert_eq!(kosync["document_hash"], document_hash);
    assert_eq!(kosync["percentage"], 0.42);
    assert_eq!(kosync["progress"], "/body/p[1]");
}

#[tokio::test]
async fn cover_is_extracted_and_served() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let mut cover = Vec::new();
    let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
    img.write_to(&mut Cursor::new(&mut cover), image::ImageFormat::Png)
        .unwrap();

    let epub = build_epub(Some(&cover));
    let result = admin_upload(&app, &epub).await;
    assert_eq!(result["results"][0]["status"], "imported");
    let document_hash = result["results"][0]["id"].as_str().unwrap().to_owned();

    let auth = basic("alice", "secret");
    let (status, headers, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/opds/v2/publications/{document_hash}/cover"),
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "image/png");
    assert_eq!(bytes, cover);

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        &format!("/opds/v2/publications/{document_hash}/thumbnail"),
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!bytes.is_empty());
}

#[tokio::test]
async fn delete_publication_removes_record() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    let result = admin_upload(&app, &epub).await;
    let document_hash = result["results"][0]["id"].as_str().unwrap().to_owned();

    let (status, _, bytes) = send_bytes(
        &app,
        "DELETE",
        &format!("/opds/v2/publications/{document_hash}"),
        &[("authorization", basic("admin", "admin").as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = as_json(&bytes);
    assert_eq!(body["message"], "Deleted");

    let (status, _, _) = send_bytes(
        &app,
        "GET",
        &format!("/opds/v2/publications/{document_hash}/file"),
        &[("authorization", basic("alice", "secret").as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn atom_feed_is_wellformed_with_expected_links() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    admin_upload(&app, &epub).await;

    let auth = basic("alice", "secret");
    let (status, headers, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/publications",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("application/atom+xml")
    );

    let atom = String::from_utf8(bytes).unwrap();

    let mut reader = quick_xml::Reader::from_str(&atom);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Eof) => break,
            Ok(_) => {}
            Err(err) => panic!("Atom feed is not well-formed XML: {err}"),
        }
        buf.clear();
    }

    assert!(atom.contains("rel=\"search\""));
    assert!(atom.contains("{searchTerms}"));
    assert!(atom.contains("application/epub+zip"));
    assert_eq!(atom.matches("rel=\"self\"").count(), 1);
    assert!(!atom.contains("rel=\"prev\""));
}

#[tokio::test]
async fn atom_alias_routes_serve_navigation_feed() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let auth = basic("alice", "secret");

    for path in ["/opds", "/opds/", "/opds.xml"] {
        let (status, _, bytes) = send_bytes(
            &app,
            "GET",
            path,
            &[("authorization", auth.as_str())],
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let body = String::from_utf8_lossy(&bytes);
        assert!(body.contains("All books"));
    }
}

#[tokio::test]
async fn opds2_feed_has_required_structure() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let epub = build_epub(None);
    admin_upload(&app, &epub).await;

    let auth = basic("alice", "secret");
    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/publications",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let feed = as_json(&bytes);
    assert_eq!(feed["@context"].as_array().unwrap().len(), 2);
    assert_eq!(feed["metadata"]["numberOfItems"], 1);
    assert!(feed["publications"].is_array());

    let publication = &feed["publications"][0];
    assert!(publication["@context"].is_string());
    assert_eq!(publication["metadata"]["title"], "Test Book");
    assert!(publication["links"].is_array());

    let links = publication["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["rel"] == "self"));
    assert!(
        links
            .iter()
            .any(|l| l["rel"] == "http://opds-spec.org/acquisition")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn opds_groups_publications_by_author_genre_and_series() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let first = build_epub_with_metadata(
        "First Book",
        "Alice Author",
        &["Fantasy", "Epic Fantasy"],
        Some("9780134685991"),
        Some(("Example Saga", 1.0)),
        None,
    );
    let second = build_epub_with_metadata(
        "Second Book",
        "Bob Writer",
        &["Mystery"],
        None,
        Some(("Example Saga", 2.0)),
        None,
    );
    admin_upload(&app, &first).await;
    admin_upload(&app, &second).await;

    let auth = basic("alice", "secret");
    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let root = as_json(&bytes);
    let navigation = root["navigation"].as_array().unwrap();
    assert!(navigation.iter().any(|entry| entry["title"] == "Authors"));
    assert!(navigation.iter().any(|entry| entry["title"] == "Genres"));
    assert!(navigation.iter().any(|entry| entry["title"] == "Series"));

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/authors",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let authors = as_json(&bytes);
    assert!(
        authors["navigation"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| {
                entry["title"] == "Alice Author" && entry["properties"]["numberOfItems"] == 1
            })
    );

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/authors/Alice%20Author",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let author_feed = as_json(&bytes);
    assert_eq!(author_feed["metadata"]["numberOfItems"], 1);
    assert_eq!(
        author_feed["publications"][0]["metadata"]["title"],
        "First Book"
    );
    assert_eq!(
        author_feed["publications"][0]["metadata"]["subject"],
        serde_json::json!([{"name": "Fantasy"}, {"name": "Epic Fantasy"}])
    );
    assert_eq!(
        author_feed["publications"][0]["metadata"]["belongs_to"]["series"]["name"],
        "Example Saga"
    );

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/genres/Fantasy",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let genre_feed = as_json(&bytes);
    assert_eq!(
        genre_feed["publications"][0]["metadata"]["title"],
        "First Book"
    );

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/series/Example%20Saga",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let series_feed = as_json(&bytes);
    assert_eq!(series_feed["metadata"]["numberOfItems"], 2);
    assert_eq!(
        series_feed["publications"][0]["metadata"]["title"],
        "First Book"
    );

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/genres/Fantasy",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let atom = String::from_utf8(bytes).unwrap();
    assert!(atom.contains("<category term=\"Fantasy\""));
    assert!(atom.contains("First Book"));
}
