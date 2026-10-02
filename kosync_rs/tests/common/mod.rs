//! Shared helpers for the integration tests.

#![allow(dead_code)]

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http_body_util::BodyExt;
use tower::ServiceExt;

use kosync_rs::{Config, build_state, init_db, router};

/// Content-type header name.
pub const JSON: &str = "content-type";

/// Build an `HTTP Basic` authorization header value.
pub fn basic(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("{username}:{password}"))
    )
}

/// Compute the MD5 hex digest of `input`.
pub fn md5(input: &str) -> String {
    use md5::{Digest, Md5};
    format!("{:x}", Md5::digest(input.as_bytes()))
}

/// Build a test configuration backed by a temporary database and books dir.
pub fn test_config(database_url: &str, books_dir: &str, registration_disabled: bool) -> Config {
    Config {
        database_url: database_url.to_owned(),
        books_dir: books_dir.to_owned(),
        registration_disabled,
        ..Config::default()
    }
}

/// Build a fully wired application against a temporary `SQLite` database.
pub async fn app(registration_disabled: bool) -> Router {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    // Leak the temp dir so the database file outlives this helper.
    std::mem::forget(dir);

    let config = test_config(&url, &books_dir, registration_disabled);
    let pool = init_db(&config).await.unwrap();
    router(build_state(&config, pool))
}

/// Send a JSON request and decode the JSON response body.
pub async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_owned())).unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();

    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };

    (status, json)
}

/// Send a request and return the response headers and text body.
pub async fn send_html(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body.to_owned())).unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8_lossy(&bytes).to_string())
}

/// Auth headers for the test user `alice`.
pub fn auth_headers() -> Vec<(&'static str, &'static str)> {
    vec![("x-auth-user", "alice"), ("x-auth-key", "hash123")]
}

/// Register the test user `alice` with pre-hashed password `hash123`.
pub async fn register(app: &Router) {
    let (status, _) = send(
        app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        r#"{"username":"alice","password":"hash123"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Register a user, MD5-hashing the given plaintext password as the client does.
pub async fn register_plaintext(app: &Router, username: &str, password: &str) {
    let hash = md5(password);
    let body = format!(r#"{{"username":"{username}","password":"{hash}"}}"#);
    let (status, _) = send(
        app,
        "POST",
        "/users/create",
        &[(JSON, "application/json")],
        &body,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

/// Register a user and seed `count` progress documents for them.
pub async fn seed_documents(app: &Router, username: &str, password: &str, count: usize) {
    register_plaintext(app, username, password).await;
    let hash = md5(password);

    for i in 0..count {
        let body = format!(
            r#"{{"document":"doc{i}","progress":"/body/p[{i}]","percentage":0.1,"device":"kindle","device_id":"dev"}}"#
        );
        let headers = vec![
            ("x-auth-user", username),
            ("x-auth-key", hash.as_str()),
            (JSON, "application/json"),
        ];
        send(app, "PUT", "/syncs/progress", &headers, &body).await;
    }
}

/// Log in via the web form and return the session cookie value.
pub async fn login(app: &Router, username: &str, password: &str) -> String {
    let (_, headers, _) = send_html(
        app,
        "POST",
        "/login",
        &[(JSON, "application/x-www-form-urlencoded")],
        &format!("username={username}&password={password}"),
    )
    .await;
    headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Build a minimal `EPUB` archive, optionally including a cover image.
pub fn build_epub(cover: Option<&[u8]>) -> Vec<u8> {
    build_epub_with_metadata("Test Book", "Alice Author", &[], None, None, cover)
}

/// Build a minimal `EPUB` archive with configurable metadata.
#[allow(clippy::format_push_string)]
pub fn build_epub_with_metadata(
    title: &str,
    author: &str,
    subjects: &[&str],
    isbn: Option<&str>,
    series: Option<(&str, f64)>,
    cover: Option<&[u8]>,
) -> Vec<u8> {
    use std::io::Write;

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();

    let container = br#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(container).unwrap();

    let mut opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">urn:uuid:test-1234</dc:identifier>
    <dc:title>{title}</dc:title>
    <dc:creator>{author}</dc:creator>
    <dc:language>en</dc:language>
"#,
    );
    if let Some(isbn) = isbn {
        opf.push_str(&format!(
            "    <dc:identifier>urn:isbn:{isbn}</dc:identifier>\n"
        ));
    }
    for subject in subjects {
        opf.push_str(&format!("    <dc:subject>{subject}</dc:subject>\n"));
    }
    if let Some((name, index)) = series {
        opf.push_str(&format!(
            "    <meta name=\"calibre:series\" content=\"{name}\"/>\n    <meta name=\"calibre:series_index\" content=\"{index}\"/>\n"
        ));
    }
    opf.push_str(
        r#"  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="chap1.xhtml" media-type="application/xhtml+xml"/>
"#,
    );
    if cover.is_some() {
        opf.push_str(
            "    <item id=\"cover\" href=\"cover.png\" media-type=\"image/png\" properties=\"cover-image\"/>\n",
        );
    }
    opf.push_str("  </manifest>\n  <spine>\n    <itemref idref=\"c1\"/>\n  </spine>\n</package>");
    zip.start_file("OEBPS/content.opf", options).unwrap();
    zip.write_all(opf.as_bytes()).unwrap();

    zip.start_file("OEBPS/nav.xhtml", options).unwrap();
    zip.write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body/></html>"#)
        .unwrap();
    zip.start_file("OEBPS/chap1.xhtml", options).unwrap();
    zip.write_all(
        br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><p>Chapter 1</p></body></html>"#,
    )
    .unwrap();

    if let Some(cover) = cover {
        zip.start_file("OEBPS/cover.png", options).unwrap();
        zip.write_all(cover).unwrap();
    }

    zip.finish().unwrap().into_inner()
}

/// Build a multipart form body containing a single file field.
pub fn multipart(filename: &str, content_type: &str, data: &[u8]) -> (Vec<u8>, String) {
    let boundary = "opds-test-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, boundary.to_owned())
}

/// Send a raw-byte request and return the status, headers, and body bytes.
pub async fn send_bytes(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::from(body)).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}
