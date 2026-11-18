//! Polish translation: language negotiation, the switcher, the saved choice,
//! OPDS feed titles, plural forms, and every page rendering in Polish.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{
    JSON, basic, build_epub, cookie_named, login, multipart, register_plaintext, seed_documents,
    send, send_bytes, send_html,
};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";

/// Fetch `path` with `headers` and return the status and page.
async fn page(app: &Router, path: &str, headers: &[(&str, &str)]) -> (StatusCode, String) {
    let (status, _, body) = send_html(app, "GET", path, headers, "").await;
    (status, body)
}

#[tokio::test]
async fn accept_language_picks_polish() {
    let app = common::app(false).await;
    let (_, body) = page(
        &app,
        "/login",
        &[("accept-language", "pl-PL,pl;q=0.9,en;q=0.8")],
    )
    .await;
    assert!(body.contains(r#"<html lang="pl">"#), "{body}");
    assert!(body.contains("Zaloguj się"));
    assert!(body.contains("Nazwa użytkownika"));
    assert!(!body.contains("Sign in"));

    let (_, body) = page(&app, "/login", &[]).await;
    assert!(body.contains(r#"<html lang="en">"#));
    assert!(body.contains("Sign in"));
}

#[tokio::test]
async fn the_language_cookie_beats_accept_language() {
    let app = common::app(false).await;
    let (_, body) = page(
        &app,
        "/login",
        &[("accept-language", "pl"), ("cookie", "kosync_locale=en")],
    )
    .await;
    assert!(body.contains("Sign in"));
    let (_, body) = page(&app, "/login", &[("cookie", "kosync_locale=pl")]).await;
    assert!(body.contains("Zaloguj się"));
}

#[tokio::test]
async fn the_switcher_sets_the_cookie_and_returns_safely() {
    let app = common::app(false).await;
    let (status, headers, _) = send_html(&app, "GET", "/locale/pl?next=/help", &[], "").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers.get("location").unwrap(), "/help");
    assert_eq!(
        cookie_named(&headers, "kosync_locale").unwrap(),
        "kosync_locale=pl"
    );

    // Off-site targets fall back to the home page.
    let (_, headers, _) = send_html(&app, "GET", "/locale/pl?next=//evil.example", &[], "").await;
    assert_eq!(headers.get("location").unwrap(), "/");
    // Without `next`, the referring page on this server is used.
    let (_, headers, _) = send_html(
        &app,
        "GET",
        "/locale/en",
        &[("referer", "http://books.example/books?page=2")],
        "",
    )
    .await;
    assert_eq!(headers.get("location").unwrap(), "/books?page=2");

    let (status, _, _) = send_html(&app, "GET", "/locale/xx", &[], "").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_header_offers_the_other_language() {
    let app = common::app(false).await;
    let (_, body) = page(&app, "/login", &[]).await;
    assert!(body.contains(r#"href="/locale/pl""#));
    assert!(body.contains(">Polski<"));
    let (_, body) = page(&app, "/login", &[("accept-language", "pl")]).await;
    assert!(body.contains(r#"href="/locale/en""#));
    assert!(body.contains(">English<"));
}

#[tokio::test]
async fn a_saved_choice_follows_the_user_to_other_browsers() {
    let app = common::app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let cookie = login(&app, "alice", "secret").await;

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/settings/language",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "language=pl",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // Another browser, asking for English, still gets Polish once signed in.
    let elsewhere = login(&app, "alice", "secret").await;
    let (_, body) = page(
        &app,
        "/",
        &[("cookie", elsewhere.as_str()), ("accept-language", "en")],
    )
    .await;
    assert!(body.contains("Postępy czytania"), "{body}");
    let (_, body) = page(&app, "/settings", &[("cookie", elsewhere.as_str())]).await;
    assert!(
        body.contains(r#"<option value="pl" lang="pl" selected>"#),
        "{body}"
    );

    // "Automatic" clears it again.
    send_html(
        &app,
        "POST",
        "/settings/language",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "language=",
    )
    .await;
    let (_, body) = page(
        &app,
        "/",
        &[("cookie", elsewhere.as_str()), ("accept-language", "en")],
    )
    .await;
    assert!(body.contains("Reading progress"));
}

#[tokio::test]
async fn opds_feeds_use_the_saved_language() {
    let app = common::app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let auth = basic("alice", "secret");

    let (_, json) = send(
        &app,
        "GET",
        "/opds/v2/",
        &[("authorization", auth.as_str())],
        "",
    )
    .await;
    assert_eq!(json["metadata"]["title"], "kosync-rs library");

    let cookie = login(&app, "alice", "secret").await;
    send_html(
        &app,
        "GET",
        "/locale/pl",
        &[("cookie", cookie.as_str())],
        "",
    )
    .await;

    // E-readers send neither the cookie nor Accept-Language.
    let (_, json) = send(
        &app,
        "GET",
        "/opds/v2/",
        &[("authorization", auth.as_str())],
        "",
    )
    .await;
    assert_eq!(json["metadata"]["title"], "Biblioteka kosync-rs");
    assert_eq!(json["navigation"][0]["title"], "Wszystkie książki");

    let (_, _, atom) = send_bytes(
        &app,
        "GET",
        "/opds/",
        &[("authorization", auth.as_str())],
        Vec::new(),
    )
    .await;
    let atom = String::from_utf8(atom).unwrap();
    assert!(atom.contains("Wszystkie książki"), "{atom}");
    assert!(atom.contains("Czytane obecnie"));
}

#[tokio::test]
async fn polish_counts_use_the_right_plural_form() {
    let app = common::app(false).await;
    seed_documents(&app, "alice", "secret", 2).await;
    seed_documents(&app, "bob", "secret", 5).await;
    seed_documents(&app, "carol", "secret", 22).await;
    let admin = login(&app, "admin", "admin").await;
    let (_, body) = page(
        &app,
        "/admin/users",
        &[("cookie", admin.as_str()), ("accept-language", "pl")],
    )
    .await;
    assert!(body.contains("2 dokumenty"), "{body}");
    assert!(body.contains("5 dokumentów"));
    assert!(body.contains("22 dokumenty"));
    assert!(body.contains("0 dokumentów"));
}

#[tokio::test]
async fn import_errors_are_translated() {
    let app = common::app(false).await;
    let admin = login(&app, "admin", "admin").await;
    let (body, boundary) = multipart("broken.epub", "application/epub+zip", b"not a zip");
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (_, _, page) = send_bytes(
        &app,
        "POST",
        "/books/upload",
        &[
            ("cookie", admin.as_str()),
            ("content-type", content_type.as_str()),
            ("accept-language", "pl"),
        ],
        body,
    )
    .await;
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("To nie jest prawidłowy plik EPUB"), "{page}");
    assert!(page.contains("Nie udało się zaimportować"));
}

#[tokio::test]
async fn every_page_renders_in_polish() {
    let app = common::app(false).await;
    let admin = login(&app, "admin", "admin").await;

    let (body, boundary) = multipart("book.epub", "application/epub+zip", &build_epub(None));
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (_, _, results) = send_bytes(
        &app,
        "POST",
        "/books/upload",
        &[
            ("cookie", admin.as_str()),
            ("content-type", content_type.as_str()),
        ],
        body,
    )
    .await;
    let results = String::from_utf8(results).unwrap();
    let start = results.find(r#"href="/books/"#).unwrap() + r#"href="/books/"#.len();
    let book = results[start..].split('"').next().unwrap().to_owned();
    // Sync the book so its history page exists.
    send(
        &app,
        "PUT",
        "/syncs/progress",
        &[
            ("x-auth-user", "admin"),
            ("x-auth-key", common::md5("admin").as_str()),
            (JSON, "application/json"),
        ],
        &format!(r#"{{"document":"{book}","progress":"p","percentage":0.42,"device":"kobo","device_id":"k"}}"#),
    )
    .await;

    let pages = [
        "/".to_owned(),
        "/books".to_owned(),
        format!("/books/{book}"),
        format!("/documents/{book}"),
        "/books/upload".to_owned(),
        "/admin/users".to_owned(),
        "/settings".to_owned(),
        "/help".to_owned(),
        "/search?q=test".to_owned(),
    ];
    for path in &pages {
        let (status, body) = page(
            &app,
            path,
            &[("cookie", admin.as_str()), ("accept-language", "pl")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(r#"<html lang="pl">"#), "{path}");
        assert!(!body.contains("{{"), "{path} left a template tag");
        for english in [
            "Library</span>",
            "Settings<",
            "Sign in",
            "Previous",
            "Unread<",
        ] {
            assert!(!body.contains(english), "{path} still says {english:?}");
        }
    }
}
