//! Tests for cover detection, thumbnails over OPDS, image caching, and the
//! cover backfill.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;

use common::{basic, build_epub, build_raw_epub, multipart, png, send_bytes, test_config};
use kosync_rs::{AppState, backfill_covers, build_state, init_db, router};

/// A plain chapter page without pictures.
const CHAPTER: &[u8] =
    br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Chapter 1</p></body></html>"#;

/// Build an application, keeping its state and pool for direct access.
async fn app_with_state() -> (Router, AppState, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);

    let config = test_config(&url, &books_dir, false);
    let pool = init_db(&config).await.unwrap();
    let state = build_state(&config, pool.clone());
    (router(state.clone()), state, pool)
}

/// The admin's `HTTP Basic` header value.
fn admin() -> String {
    basic("admin", "admin")
}

/// Upload an `EPUB` as admin and return its document digest.
async fn upload(app: &Router, epub: &[u8]) -> String {
    let (body, boundary) = multipart("book.epub", "application/epub+zip", epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, bytes) = send_bytes(
        app,
        "POST",
        "/opds/upload",
        &[
            ("content-type", content_type.as_str()),
            ("authorization", admin().as_str()),
        ],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(result["results"][0]["status"], "imported", "{result}");
    result["results"][0]["id"].as_str().unwrap().to_owned()
}

/// Fetch a publication's thumbnail, returning the status and body.
async fn thumbnail(app: &Router, hash: &str) -> (StatusCode, Vec<u8>) {
    let (status, _, bytes) = send_bytes(
        app,
        "GET",
        &format!("/opds/v2/publications/{hash}/thumbnail"),
        &[("authorization", admin().as_str())],
        Vec::new(),
    )
    .await;
    (status, bytes)
}

/// Fetch a feed as text.
async fn feed(app: &Router, uri: &str) -> String {
    let (status, _, bytes) = send_bytes(
        app,
        "GET",
        uri,
        &[("authorization", admin().as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    String::from_utf8(bytes).unwrap()
}

/// Upload `epub` and assert it gets a decodable `JPEG` thumbnail that the
/// Atom feed links to.
async fn assert_thumbnail_detected(epub: &[u8]) {
    let (app, _, _) = app_with_state().await;
    let hash = upload(&app, epub).await;

    let (status, bytes) = thumbnail(&app, &hash).await;
    assert_eq!(status, StatusCode::OK);
    let format = image::guess_format(&bytes).unwrap();
    assert_eq!(format, image::ImageFormat::Jpeg);

    let atom = feed(&app, "/opds/publications").await;
    assert!(atom.contains(&format!("/opds/v2/publications/{hash}/thumbnail")));
}

#[tokio::test]
async fn calibre_prefixed_package_with_meta_cover() {
    let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<ns0:package xmlns:ns0="http://www.idpf.org/2007/opf" unique-identifier="id" version="2.0">
<ns0:metadata>
<ns1:title xmlns:ns1="http://purl.org/dc/elements/1.1/">Calibre Book</ns1:title>
<ns1:identifier xmlns:ns1="http://purl.org/dc/elements/1.1/" id="id">urn:uuid:calibre-1</ns1:identifier>
<meta name="cover" content="cover"/>
</ns0:metadata>
<ns0:manifest>
<ns0:item href="cover.png" id="cover" media-type="image/png"/>
<ns0:item href="c1.xhtml" id="c1" media-type="application/xhtml+xml"/>
</ns0:manifest>
<ns0:spine><ns0:itemref idref="c1"/></ns0:spine>
</ns0:package>"#;
    let cover = png(60, 90);
    let epub = build_raw_epub(
        "content.opf",
        &[
            ("content.opf", opf),
            ("c1.xhtml", CHAPTER),
            ("cover.png", &cover),
        ],
    );
    assert_thumbnail_detected(&epub).await;
}

#[tokio::test]
async fn epub3_with_only_a_meta_cover() {
    let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:meta-only</dc:identifier>
    <dc:title>Meta Only</dc:title>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest>
    <item id="cover-img" href="images/cover.png" media-type="image/png"/>
    <item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
</package>"#;
    let cover = png(60, 90);
    let epub = build_raw_epub(
        "OEBPS/content.opf",
        &[
            ("OEBPS/content.opf", opf),
            ("OEBPS/c1.xhtml", CHAPTER),
            ("OEBPS/images/cover.png", &cover),
        ],
    );
    assert_thumbnail_detected(&epub).await;
}

#[tokio::test]
async fn guide_cover_page_with_relative_escaped_image() {
    let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:guide</dc:identifier>
    <dc:title>Guide Cover</dc:title>
  </metadata>
  <manifest>
    <item id="c1" href="Text/c1.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover-page" href="Text/cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="art" href="Images/cover%20art.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="c1"/></spine>
  <guide><reference type="cover" title="Cover" href="Text/cover.xhtml#top"/></guide>
</package>"#;
    let page = br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><div id="top"><img src="../Images/cover%20art.png" alt="Cover"/></div></body></html>"#;
    let cover = png(60, 90);
    let epub = build_raw_epub(
        "OEBPS/content.opf",
        &[
            ("OEBPS/content.opf", opf),
            ("OEBPS/Text/c1.xhtml", CHAPTER),
            ("OEBPS/Text/cover.xhtml", page),
            ("OEBPS/Images/cover art.png", &cover),
        ],
    );
    assert_thumbnail_detected(&epub).await;
}

#[tokio::test]
async fn undeclared_svg_cover_page_in_first_spine_item() {
    let opf = br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:svg-page</dc:identifier>
    <dc:title>SVG Cover Page</dc:title>
  </metadata>
  <manifest>
    <item id="title" href="titlepage.xhtml" media-type="application/xhtml+xml"/>
    <item id="c1" href="c1.xhtml" media-type="application/xhtml+xml"/>
    <item id="img" href="cover.png" media-type="image/png"/>
  </manifest>
  <spine><itemref idref="title"/><itemref idref="c1"/></spine>
</package>"#;
    let page = br#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:xlink="http://www.w3.org/1999/xlink"><head><title>Cover</title></head>
<body><p>&nbsp;</p><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 60 90"><image width="60" height="90" xlink:href="cover.png"/></svg></body></html>"#;
    let cover = png(60, 90);
    let epub = build_raw_epub(
        "content.opf",
        &[
            ("content.opf", opf),
            ("titlepage.xhtml", page),
            ("c1.xhtml", CHAPTER),
            ("cover.png", &cover),
        ],
    );
    assert_thumbnail_detected(&epub).await;
}

#[tokio::test]
async fn feeds_link_thumbnails_with_stanza_aliases() {
    let (app, _, _) = app_with_state().await;
    let hash = upload(&app, &build_epub(Some(&png(60, 90)))).await;

    let atom = feed(&app, "/opds/publications").await;
    let thumb = format!("/opds/v2/publications/{hash}/thumbnail");
    assert!(atom.contains("rel=\"http://opds-spec.org/image/thumbnail\""));
    assert!(atom.contains("rel=\"x-stanza-cover-image-thumbnail\""));
    assert!(atom.contains("rel=\"x-stanza-cover-image\""));
    assert_eq!(atom.matches(&thumb).count(), 2);

    let json: serde_json::Value =
        serde_json::from_str(&feed(&app, "/opds/v2/publications").await).unwrap();
    let links = json["publications"][0]["links"].as_array().unwrap();
    let thumbnail = links
        .iter()
        .find(|l| l["rel"] == "http://opds-spec.org/image/thumbnail")
        .unwrap();
    assert!(thumbnail["href"].as_str().unwrap().ends_with(&thumb));
    assert_eq!(thumbnail["type"], "image/jpeg");
}

#[tokio::test]
async fn cover_stands_in_when_no_thumbnail_could_be_made() {
    let (app, _, _) = app_with_state().await;
    let hash = upload(&app, &build_epub(Some(b"not really a png"))).await;

    let (status, _) = thumbnail(&app, &hash).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let cover = format!("/opds/v2/publications/{hash}/cover");
    let atom = feed(&app, "/opds/publications").await;
    assert!(atom.contains(&format!(
        "rel=\"http://opds-spec.org/image/thumbnail\" href=\"http://localhost{cover}\""
    )));

    let json: serde_json::Value =
        serde_json::from_str(&feed(&app, "/opds/v2/publications").await).unwrap();
    let links = json["publications"][0]["links"].as_array().unwrap();
    let thumbnail = links
        .iter()
        .find(|l| l["rel"] == "http://opds-spec.org/image/thumbnail")
        .unwrap();
    assert!(thumbnail["href"].as_str().unwrap().ends_with(&cover));
}

#[tokio::test]
async fn thumbnails_revalidate_with_etags() {
    let (app, _, _) = app_with_state().await;
    let hash = upload(&app, &build_epub(Some(&png(60, 90)))).await;
    let uri = format!("/opds/v2/publications/{hash}/thumbnail");
    let auth = admin();

    let (status, headers, bytes) = send_bytes(
        &app,
        "GET",
        &uri,
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let etag = headers.get("etag").unwrap().to_str().unwrap().to_owned();
    assert!(
        headers
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("private")
    );
    assert_eq!(
        headers.get("content-length").unwrap().to_str().unwrap(),
        bytes.len().to_string()
    );

    let (status, headers, bytes) = send_bytes(
        &app,
        "GET",
        &uri,
        &[
            ("authorization", auth.as_str()),
            ("if-none-match", etag.as_str()),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(headers.get("etag").unwrap().to_str().unwrap(), etag);
    assert_eq!(bytes, b"");

    let (status, _, _) = send_bytes(
        &app,
        "GET",
        &uri,
        &[
            ("authorization", auth.as_str()),
            ("if-none-match", "\"stale\""),
        ],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn backfill_restores_missing_covers_and_thumbnails() {
    let (app, state, pool) = app_with_state().await;
    let hash = upload(&app, &build_epub(Some(&png(60, 90)))).await;

    // An import from before cover detection improved: nothing recorded.
    sqlx::query(
        "UPDATE publications SET cover_path = NULL, cover_media_type = NULL,
                thumb_path = NULL, thumb_media_type = NULL",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(thumbnail(&app, &hash).await.0, StatusCode::NOT_FOUND);

    assert_eq!(backfill_covers(&state).await, 1);
    assert_eq!(thumbnail(&app, &hash).await.0, StatusCode::OK);

    // A cover whose thumbnail went missing gets a new thumbnail.
    sqlx::query("UPDATE publications SET thumb_path = NULL, thumb_media_type = NULL")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(backfill_covers(&state).await, 1);
    assert_eq!(thumbnail(&app, &hash).await.0, StatusCode::OK);

    // Nothing left to do.
    assert_eq!(backfill_covers(&state).await, 0);
}
