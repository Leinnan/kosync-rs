//! Tests for replacing a book's cover from the web portal while its sync
//! progress carries over between the old and the new file.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::{Cursor, Read};

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;

use common::{
    JSON, basic, build_epub, build_raw_epub, follow_flash, login, md5, multipart, png,
    register_plaintext, send, send_bytes, send_html, test_config,
};
use kosync_rs::{build_state, init_db, router};

/// Build an application, keeping its pool for direct checks.
async fn app_with_pool() -> (Router, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);

    let config = test_config(&url, &books_dir, false);
    let pool = init_db(&config).await.unwrap();
    let state = build_state(&config, pool.clone()).unwrap();
    (router(state), pool)
}

/// The `KOReader` partial `MD5` digest, as the reader computes it.
fn partial_md5(data: &[u8]) -> String {
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

/// Upload an `EPUB` over OPDS as `username` and return the upload result.
async fn upload_as(app: &Router, username: &str, password: &str, epub: &[u8]) -> serde_json::Value {
    let (body, boundary) = multipart("book.epub", "application/epub+zip", epub);
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let auth = basic(username, password);
    let (status, _, bytes) = send_bytes(
        app,
        "POST",
        "/opds/upload",
        &[
            ("content-type", content_type.as_str()),
            ("authorization", auth.as_str()),
        ],
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    result["results"][0].clone()
}

/// Upload an `EPUB` as `username` and return its document digest.
async fn upload(app: &Router, username: &str, password: &str, epub: &[u8]) -> String {
    let result = upload_as(app, username, password, epub).await;
    assert_eq!(result["status"], "imported", "{result}");
    result["id"].as_str().unwrap().to_owned()
}

/// Post a replacement cover for `hash` from the session in `cookie`.
async fn replace_cover(
    app: &Router,
    cookie: &str,
    hash: &str,
    image: &[u8],
    extra: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap) {
    let boundary = "cover-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"cover\"; filename=\"cover.png\"\r\nContent-Type: image/png\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(image);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let mut headers = vec![("content-type", content_type.as_str()), ("cookie", cookie)];
    headers.extend_from_slice(extra);
    let (status, headers, _) =
        send_bytes(app, "POST", &format!("/books/{hash}/cover"), &headers, body).await;
    (status, headers)
}

/// The redirect target of a response.
fn location(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

/// Push sync progress for `document` as `username`.
async fn push(app: &Router, username: &str, password: &str, document: &str, progress: &str) {
    let key = md5(password);
    let body = format!(
        r#"{{"document":"{document}","progress":"{progress}","percentage":0.42,"device":"kobo","device_id":"dev1"}}"#
    );
    let (status, json) = send(
        app,
        "PUT",
        "/syncs/progress",
        &[
            ("x-auth-user", username),
            ("x-auth-key", key.as_str()),
            (JSON, "application/json"),
        ],
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(
        json["document"], document,
        "the reply echoes the digest sent"
    );
}

/// Pull sync progress for `document` as `username`.
async fn pull(app: &Router, username: &str, password: &str, document: &str) -> serde_json::Value {
    let key = md5(password);
    let (status, json) = send(
        app,
        "GET",
        &format!("/syncs/progress/{document}"),
        &[("x-auth-user", username), ("x-auth-key", key.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    json
}

/// Download a book from the portal.
async fn download(app: &Router, cookie: &str, hash: &str) -> Vec<u8> {
    let (status, _, bytes) = send_bytes(
        app,
        "GET",
        &format!("/books/{hash}/download"),
        &[("cookie", cookie)],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    bytes
}

/// Read one entry of a `ZIP` archive.
fn entry(archive: &[u8], name: &str) -> Vec<u8> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    let mut file = zip.by_name(name).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}

/// The names of a `ZIP` archive's entries, in order.
fn entry_names(archive: &[u8]) -> Vec<String> {
    let zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    zip.file_names().map(str::to_owned).collect::<Vec<_>>()
}

/// The path of the cover image inside archives from `build_epub`.
fn cover_entry(archive: &[u8]) -> String {
    entry_names(archive)
        .into_iter()
        .find(|name| name.contains("cover") && !name.ends_with("html"))
        .unwrap()
}

#[tokio::test]
async fn replaced_covers_keep_sync_progress_across_both_files() {
    let (app, pool) = app_with_pool().await;
    register_plaintext(&app, "reader", "secret").await;
    let original = build_epub(Some(&png(60, 90)));
    let old_hash = upload(&app, "reader", "secret", &original).await;
    assert_eq!(old_hash, partial_md5(&original));
    push(
        &app,
        "reader",
        "secret",
        &old_hash,
        "/body/DocFragment[3]/body/p[7]/text().0",
    )
    .await;

    let cookie = login(&app, "reader", "secret").await;
    let new_cover = png(80, 100);
    let (status, headers) = replace_cover(&app, &cookie, &old_hash, &new_cover, &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let target = location(&headers);
    let new_hash = target.strip_prefix("/books/").unwrap().to_owned();
    assert_ne!(new_hash, old_hash);
    let page = follow_flash(&app, &cookie, &headers).await;
    assert!(page.contains("Cover replaced"), "{page}");

    // The served book is the rewritten one, with the new cover and nothing
    // else changed.
    let replaced = download(&app, &cookie, &new_hash).await;
    assert_eq!(partial_md5(&replaced), new_hash);
    let cover = cover_entry(&original);
    assert_eq!(
        entry(&replaced, &cover),
        new_cover,
        "a PNG slot keeps the PNG as is"
    );
    assert_eq!(entry_names(&replaced), entry_names(&original));
    for name in entry_names(&original).iter().filter(|name| **name != cover) {
        assert_eq!(
            entry(&replaced, name),
            entry(&original, name),
            "{name} is unchanged"
        );
    }

    // Both digests see the progress recorded before the change.
    for hash in [&new_hash, &old_hash] {
        let json = pull(&app, "reader", "secret", hash).await;
        assert_eq!(
            json["progress"], "/body/DocFragment[3]/body/p[7]/text().0",
            "{hash}"
        );
        assert_eq!(json["document"], hash.as_str());
    }

    // An old copy (KOReader caches the digest) keeps syncing the same book.
    push(
        &app,
        "reader",
        "secret",
        &old_hash,
        "/body/DocFragment[4]/body/p[1]/text().0",
    )
    .await;
    let json = pull(&app, "reader", "secret", &new_hash).await;
    assert_eq!(json["progress"], "/body/DocFragment[4]/body/p[1]/text().0");
    let rows: Vec<String> = sqlx::query_scalar("SELECT document_hash FROM documents")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        rows,
        std::slice::from_ref(&new_hash),
        "one progress record, under the new digest"
    );
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM progress_events WHERE document_hash = ?")
            .bind(&new_hash)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 2, "the history moved along");

    // Old links still work, and the old file is still recognised.
    let (status, _, body) = send_html(
        &app,
        "GET",
        &format!("/books/{old_hash}"),
        &[("cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(&new_hash),
        "the page shows the current digest"
    );
    let again = upload_as(&app, "reader", "secret", &original).await;
    assert_eq!(again["status"], "duplicate", "{again}");

    // A second replacement chains the aliases.
    let (_, headers) = replace_cover(&app, &cookie, &new_hash, &png(70, 100), &[]).await;
    let third = location(&headers)
        .strip_prefix("/books/")
        .unwrap()
        .to_owned();
    assert_ne!(third, new_hash);
    let json = pull(&app, "reader", "secret", &old_hash).await;
    assert_eq!(json["progress"], "/body/DocFragment[4]/body/p[1]/text().0");
}

#[tokio::test]
async fn pictures_in_another_format_are_converted_to_the_covers_format() {
    let (app, _) = app_with_pool().await;
    let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Jpeg Book</dc:title><dc:identifier id="id">jpeg-book</dc:identifier>
    <meta name="cover" content="cover"/>
  </metadata>
  <manifest>
    <item id="cover" href="images/cover.jpg" media-type="image/jpeg"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;
    let jpeg = {
        let img = image::RgbImage::from_pixel(30, 40, image::Rgb([10, 200, 10]));
        let mut bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    };
    let book = build_raw_epub(
        "OEBPS/content.opf",
        &[
            ("OEBPS/content.opf", opf),
            ("OEBPS/images/cover.jpg", &jpeg),
            (
                "OEBPS/ch1.xhtml",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>One</p></body></html>"#,
            ),
        ],
    );
    let hash = upload(&app, "admin", "admin", &book).await;
    let cookie = login(&app, "admin", "admin").await;

    let (status, headers) = replace_cover(&app, &cookie, &hash, &png(50, 80), &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let new_hash = location(&headers)
        .strip_prefix("/books/")
        .unwrap()
        .to_owned();
    let replaced = download(&app, &cookie, &new_hash).await;
    let cover = entry(&replaced, "OEBPS/images/cover.jpg");
    assert_eq!(
        image::guess_format(&cover).unwrap(),
        image::ImageFormat::Jpeg
    );
    assert_eq!(image::load_from_memory(&cover).unwrap().width(), 50);
    assert_eq!(
        entry(&replaced, "OEBPS/content.opf"),
        opf,
        "the package is untouched"
    );
}

#[tokio::test]
async fn only_admins_and_the_uploader_may_replace_a_cover() {
    let (app, pool) = app_with_pool().await;
    register_plaintext(&app, "owner", "secret").await;
    register_plaintext(&app, "other", "secret").await;
    let hash = upload(&app, "owner", "secret", &build_epub(Some(&png(60, 90)))).await;

    let other = login(&app, "other", "secret").await;
    let (_, _, page) = send_html(
        &app,
        "GET",
        &format!("/books/{hash}"),
        &[("cookie", &other)],
        "",
    )
    .await;
    assert!(
        !page.contains("cover_modal"),
        "no cover action for other users"
    );
    let (status, headers) = replace_cover(&app, &other, &hash, &png(10, 10), &[]).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), format!("/books/{hash}"), "refused");
    let current: String = sqlx::query_scalar("SELECT document_hash FROM publications")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(current, hash);

    let owner = login(&app, "owner", "secret").await;
    let (_, _, page) = send_html(
        &app,
        "GET",
        &format!("/books/{hash}"),
        &[("cookie", &owner)],
        "",
    )
    .await;
    assert!(page.contains("cover_modal"), "the uploader sees the action");

    // Without the session's CSRF token nothing changes.
    let (status, _) = replace_cover(
        &app,
        &owner,
        &hash,
        &png(10, 10),
        &[("x-csrf-token", "wrong")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Books from before uploaders were recorded are left to administrators.
    sqlx::query("UPDATE publications SET uploaded_by = NULL")
        .execute(&pool)
        .await
        .unwrap();
    let (_, headers) = replace_cover(&app, &owner, &hash, &png(10, 10), &[]).await;
    assert_eq!(location(&headers), format!("/books/{hash}"), "refused");

    let admin = login(&app, "admin", "admin").await;
    let (_, headers) = replace_cover(&app, &admin, &hash, &png(10, 10), &[]).await;
    assert_ne!(location(&headers), format!("/books/{hash}"), "admins may");
}

#[tokio::test]
async fn books_without_a_cover_or_bad_pictures_are_left_unchanged() {
    let (app, pool) = app_with_pool().await;
    let admin = login(&app, "admin", "admin").await;

    // `build_epub` without a cover still has a first spine page; give the book
    // no pictures at all so there is nothing to replace.
    let opf = br#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Plain</dc:title><dc:identifier id="id">plain</dc:identifier>
  </metadata>
  <manifest><item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest>
  <spine><itemref idref="ch1"/></spine>
</package>"#;
    let plain = build_raw_epub(
        "content.opf",
        &[
            ("content.opf", opf),
            (
                "ch1.xhtml",
                br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>One</p></body></html>"#,
            ),
        ],
    );
    let hash = upload(&app, "admin", "admin", &plain).await;
    let (_, headers) = replace_cover(&app, &admin, &hash, &png(10, 10), &[]).await;
    assert_eq!(location(&headers), format!("/books/{hash}"));
    let page = follow_flash(&app, &admin, &headers).await;
    assert!(page.contains("no cover picture"), "{page}");

    let covered = upload(&app, "admin", "admin", &build_epub(Some(&png(60, 90)))).await;
    let (_, headers) = replace_cover(&app, &admin, &covered, b"not a picture", &[]).await;
    assert_eq!(location(&headers), format!("/books/{covered}"));
    let page = follow_flash(&app, &admin, &headers).await;
    assert!(page.contains("not a picture that can be used"), "{page}");

    let hashes: Vec<String> =
        sqlx::query_scalar("SELECT document_hash FROM publications ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(hashes, [hash, covered]);
    // No former digests; predicted file names are kept alongside them.
    let aliases: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM publication_aliases WHERE kind = 'digest'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(aliases, 0);
}
