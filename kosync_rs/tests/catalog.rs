//! Tests for catalog navigation: `OpenSearch`, `start`/`up` links, richer
//! Atom entries, and the "Recently added" and "Currently reading" shelves.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;

use common::{
    JSON, basic, build_epub_with_metadata, md5, multipart, register_plaintext, send, send_bytes,
    test_config,
};
use kosync_rs::{build_state, init_db, router};

/// Build an application, keeping its pool for direct database access.
async fn app_with_pool() -> (Router, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);

    let config = test_config(&url, &books_dir, false);
    let pool = init_db(&config).await.unwrap();
    (router(build_state(&config, pool.clone())), pool)
}

/// Upload a book titled `title` as admin and return its document digest.
async fn upload(app: &Router, title: &str, isbn: Option<&str>) -> String {
    let epub = build_epub_with_metadata(title, "Alice Author", &[], isbn, None, None);
    let (body, boundary) = multipart(&format!("{title}.epub"), "application/epub+zip", &epub);
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
    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    result["results"][0]["id"].as_str().unwrap().to_owned()
}

/// `GET` a feed as `user`/`password`, returning the status and body text.
async fn get(app: &Router, uri: &str, user: &str, password: &str) -> (StatusCode, String) {
    let (status, _, bytes) = send_bytes(
        app,
        "GET",
        uri,
        &[("authorization", basic(user, password).as_str())],
        Vec::new(),
    )
    .await;
    (status, String::from_utf8(bytes).unwrap())
}

/// The `href` of the Atom feed-level link with relation `rel`, if any.
fn feed_link(atom: &str, rel: &str) -> Option<String> {
    let head = &atom[..atom.find("<entry>").unwrap_or(atom.len())];
    let start = head.find(&format!("rel=\"{rel}\" href=\""))? + rel.len() + 13;
    Some(head[start..start + head[start..].find('"')?].to_owned())
}

#[tokio::test]
async fn opensearch_description_is_served_and_linked() {
    let (app, _) = app_with_pool().await;

    let (status, headers, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/opensearch.xml",
        &[("authorization", basic("admin", "admin").as_str())],
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
            .starts_with("application/opensearchdescription+xml")
    );
    let xml = String::from_utf8(bytes).unwrap();
    assert!(xml.contains("template=\"http://localhost/opds/publications?query={searchTerms}\""));

    let (_, root) = get(&app, "/opds/", "admin", "admin").await;
    assert!(root.contains(
        "rel=\"search\" href=\"http://localhost/opds/opensearch.xml\" type=\"application/opensearchdescription+xml\""
    ));
    assert!(root.contains("{searchTerms}"));
}

#[tokio::test]
async fn opensearch_description_requires_auth() {
    let (app, _) = app_with_pool().await;
    let (status, _, _) = send_bytes(&app, "GET", "/opds/opensearch.xml", &[], Vec::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn atom_feeds_link_start_and_up() {
    let (app, _) = app_with_pool().await;
    upload(&app, "Linked", None).await;
    let root = "http://localhost/opds/".to_owned();

    let cases = [
        ("/opds/", None),
        ("/opds/publications", Some(root.clone())),
        ("/opds/authors", Some(root.clone())),
        (
            "/opds/authors/Alice%20Author",
            Some("http://localhost/opds/authors".to_owned()),
        ),
        ("/opds/recent", Some(root.clone())),
    ];
    for (uri, up) in cases {
        let (status, atom) = get(&app, uri, "admin", "admin").await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(feed_link(&atom, "start"), Some(root.clone()), "{uri}");
        assert_eq!(feed_link(&atom, "up"), up, "{uri}");
    }
}

#[tokio::test]
async fn atom_entries_carry_isbn_identifiers() {
    let (app, _) = app_with_pool().await;
    upload(&app, "Has ISBN", Some("9780134685991")).await;
    let (_, atom) = get(&app, "/opds/publications", "admin", "admin").await;
    assert!(atom.contains("<dcterms:identifier>urn:isbn:9780134685991</dcterms:identifier>"));
}

/// Titles of the publications in an OPDS 2.0 feed, in order.
fn titles(feed: &str) -> Vec<String> {
    let json: serde_json::Value = serde_json::from_str(feed).unwrap();
    json["publications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["metadata"]["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn recently_added_lists_newest_first() {
    let (app, pool) = app_with_pool().await;
    for (title, created_at) in [("Oldest", 100), ("Newest", 300), ("Middle", 200)] {
        let hash = upload(&app, title, None).await;
        sqlx::query("UPDATE publications SET created_at = ? WHERE document_hash = ?")
            .bind(created_at)
            .bind(&hash)
            .execute(&pool)
            .await
            .unwrap();
    }

    let (status, feed) = get(&app, "/opds/v2/recent", "admin", "admin").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&feed), ["Newest", "Middle", "Oldest"]);

    let (status, atom) = get(&app, "/opds/recent", "admin", "admin").await;
    assert_eq!(status, StatusCode::OK);
    let newest = atom.find("<title>Newest</title>").unwrap();
    let oldest = atom.find("<title>Oldest</title>").unwrap();
    assert!(newest < oldest);

    let (_, root) = get(&app, "/opds/", "admin", "admin").await;
    assert_eq!(
        feed_link(&root, "http://opds-spec.org/sort/new").as_deref(),
        Some("http://localhost/opds/recent")
    );
}

#[tokio::test]
async fn currently_reading_lists_only_unfinished_books_of_the_user() {
    let (app, _) = app_with_pool().await;
    register_plaintext(&app, "alice", "secret").await;
    let reading = upload(&app, "Reading", None).await;
    let finished = upload(&app, "Finished", None).await;
    upload(&app, "Untouched", None).await;

    let key = md5("secret");
    for (document, percentage) in [(&reading, 0.4), (&finished, 1.0)] {
        let payload = format!(
            r#"{{"document":"{document}","progress":"/body/p[1]","percentage":{percentage},"device":"kindle","device_id":"dev1"}}"#
        );
        let (status, _) = send(
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
        assert_eq!(status, StatusCode::OK);
    }

    let (status, feed) = get(&app, "/opds/v2/reading", "alice", "secret").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&feed), ["Reading"]);

    let (_, atom) = get(&app, "/opds/reading", "alice", "secret").await;
    assert!(atom.contains("<title>Reading</title>"));
    assert!(!atom.contains("<title>Finished</title>"));

    // Another user's shelf is empty.
    let (status, feed) = get(&app, "/opds/v2/reading", "admin", "admin").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&feed), Vec::<String>::new());

    let (_, root) = get(&app, "/opds/v2/", "alice", "secret").await;
    assert!(root.contains("/opds/v2/reading"));
    assert!(root.contains("/opds/v2/recent"));
    assert!(root.contains("/opds/v2/publications{?query}"));
}
