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
        admin_password: Some("admin".to_owned()),
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
    router(build_state(&config, pool).unwrap())
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

/// The value of the `Set-Cookie` header that sets `name`, as `name=value`.
pub fn cookie_named(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or_default().to_owned())
        .find(|pair| pair.starts_with(&format!("{name}=")))
}

/// Fetch the login page and return its anonymous CSRF cookie and token.
pub async fn login_csrf(app: &Router) -> (String, String) {
    let (_, headers, _) = raw_send(app, "GET", "/login", &[], Body::empty()).await;
    let cookie = cookie_named(&headers, "kosync_anon_csrf").unwrap();
    let token = cookie.split_once('=').unwrap().1.to_owned();
    (cookie, token)
}

/// Post the login form with a valid anonymous CSRF token and return the
/// response.
pub async fn post_login(app: &Router, body: &str) -> (StatusCode, HeaderMap, String) {
    let (cookie, token) = login_csrf(app).await;
    send_html(
        app,
        "POST",
        "/login",
        &[
            (JSON, "application/x-www-form-urlencoded"),
            ("cookie", cookie.as_str()),
        ],
        &format!("{body}&csrf_token={token}"),
    )
    .await
}

/// Log in via the web form and return the session cookie, as `name=value`.
pub async fn login(app: &Router, username: &str, password: &str) -> String {
    let (_, headers, _) =
        post_login(app, &format!("username={username}&password={password}")).await;
    cookie_named(&headers, "kosync_session").unwrap()
}

/// The CSRF token of the session in `cookie`, read from a page's meta tag.
pub async fn session_csrf(app: &Router, cookie: &str) -> String {
    let (_, _, bytes) = raw_send(
        app,
        "GET",
        "/settings",
        &[("cookie", cookie)],
        Body::empty(),
    )
    .await;
    let page = String::from_utf8_lossy(&bytes);
    let marker = r#"<meta name="csrf-token" content=""#;
    let start = page.find(marker).unwrap() + marker.len();
    let end = start + page[start..].find('"').unwrap();
    page[start..end].to_owned()
}

/// Follow a redirect that set a flash message and return the page it shows,
/// sending the session in `cookie` along with the flash cookie.
pub async fn follow_flash(app: &Router, cookie: &str, headers: &HeaderMap) -> String {
    let location = headers
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let flash = cookie_named(headers, "kosync_flash").unwrap();
    let cookies = format!("{cookie}; {flash}");
    let (_, _, bytes) = raw_send(
        app,
        "GET",
        &location,
        &[("cookie", &cookies)],
        Body::empty(),
    )
    .await;
    String::from_utf8_lossy(&bytes).to_string()
}

/// Send a request like the browser does: a `POST` carrying a session cookie
/// also sends that session's CSRF token in `X-CSRF-Token`, as `app.js` does for
/// htmx, unless the caller sets that header itself.
async fn browser_send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let session = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
        .map(|(_, value)| *value)
        .filter(|value| value.contains("kosync_session="));
    let explicit = headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("x-csrf-token"));
    match session {
        Some(cookie) if method == "POST" && !explicit => {
            let token = session_csrf(app, cookie).await;
            let mut with_token = headers.to_vec();
            with_token.push(("x-csrf-token", token.as_str()));
            raw_send(app, method, uri, &with_token, body).await
        }
        _ => raw_send(app, method, uri, headers, body).await,
    }
}

/// Send a request exactly as given, with no CSRF token added.
pub async fn raw_send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

/// Send a request and return the response headers and text body.
pub async fn send_html(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, HeaderMap, String) {
    let (status, headers, bytes) =
        browser_send(app, method, uri, headers, Body::from(body.to_owned())).await;
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

/// Build a minimal uncompressed `MOBI` file, optionally encrypted or with a cover.
pub fn build_mobi(title: &str, author: &str, cover: Option<&[u8]>, encrypted: bool) -> Vec<u8> {
    fn exth_record(kind: u32, value: &[u8]) -> Vec<u8> {
        let length = u32::try_from(value.len() + 8).unwrap();
        [&kind.to_be_bytes(), &length.to_be_bytes(), value].concat()
    }

    let text = b"<html><body><p>Chapter one<mbp:pagebreak/><p>Chapter two</body></html>";

    let mut exth_records = exth_record(100, author.as_bytes());
    let mut exth_count = 1u32;
    if cover.is_some() {
        exth_records.extend(exth_record(201, &0u32.to_be_bytes()));
        exth_count += 1;
    }
    let exth_length = u32::try_from(exth_records.len() + 12).unwrap();
    let exth = [
        b"EXTH",
        &exth_length.to_be_bytes(),
        &exth_count.to_be_bytes(),
        exth_records.as_slice(),
    ]
    .concat();

    // Everything before the EXTH block: PalmDOC header (16) plus MOBI header (232).
    let mut header = vec![0u8; 248];
    let mut put = |offset: usize, value: u32| {
        header[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    };
    put(0, 1 << 16); // no compression
    put(4, u32::try_from(text.len()).unwrap());
    put(8, (1 << 16) | 4096); // one text record of up to 4096 bytes
    put(12, u32::from(encrypted) << 17); // encryption type 2 (Mobipocket)
    put(20, 232); // MOBI header length
    put(24, 2); // book
    put(28, 65001); // UTF-8
    put(36, 6); // format version
    put(0x50, 2); // first non-book record
    put(0x54, u32::try_from(248 + exth.len()).unwrap()); // title offset
    put(0x58, u32::try_from(title.len()).unwrap()); // title length
    put(0x5C, 9); // English
    put(0x6C, 2); // first image record
    put(0x80, 0x40); // EXTH present
    put(0xF4, u32::MAX); // no NCX
    header[16..20].copy_from_slice(b"MOBI");
    let first_record = [header.as_slice(), exth.as_slice(), title.as_bytes()].concat();

    let mut records: Vec<&[u8]> = vec![&first_record, text];
    records.extend(cover);
    records.push(b"\xe9\x8e\r\n"); // end-of-file record

    let mut file = vec![0u8; 78];
    file[..4].copy_from_slice(b"test");
    file[60..68].copy_from_slice(b"BOOKMOBI");
    file[76..78].copy_from_slice(&u16::try_from(records.len()).unwrap().to_be_bytes());
    let mut offset = 78 + records.len() * 8 + 2;
    for (index, record) in records.iter().enumerate() {
        file.extend_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
        file.extend_from_slice(&u32::try_from(index * 2).unwrap().to_be_bytes());
        offset += record.len();
    }
    file.extend_from_slice(&[0, 0]);
    for record in records {
        file.extend_from_slice(record);
    }
    file
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
    browser_send(app, method, uri, headers, Body::from(body)).await
}

/// Build an `EPUB` archive from raw `(path, contents)` entries. The
/// `mimetype` and `META-INF/container.xml` entries are added, pointing at
/// `opf_path`.
pub fn build_raw_epub(opf_path: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;

    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("mimetype", options).unwrap();
    zip.write_all(b"application/epub+zip").unwrap();
    let container = format!(
        r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="{opf_path}" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#
    );
    zip.start_file("META-INF/container.xml", options).unwrap();
    zip.write_all(container.as_bytes()).unwrap();

    for (path, contents) in files {
        zip.start_file(*path, options).unwrap();
        zip.write_all(contents).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// Encode a solid-colour `PNG` of the given size.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 255]));
    let mut bytes = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut bytes),
        image::ImageFormat::Png,
    )
    .unwrap();
    bytes
}
