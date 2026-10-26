//! Tests for overlaying each user's sync progress onto OPDS feeds, which is
//! fetched for a whole page of publications at once.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};

use common::{
    JSON, app, basic, build_epub_with_metadata, md5, multipart, register_plaintext, send,
    send_bytes,
};

/// Upload `epub` under `file_name` as admin and return its document digest.
async fn upload(app: &Router, file_name: &str, epub: &[u8]) -> String {
    let (body, boundary) = multipart(file_name, "application/epub+zip", epub);
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

/// Sync progress for alice.
async fn sync(app: &Router, document: &str, percentage: f64, file_name: Option<&str>) {
    let metadata = file_name
        .map(|name| format!(r#","metadata":{{"filename":"{name}"}}"#))
        .unwrap_or_default();
    let payload = format!(
        r#"{{"document":"{document}","progress":"/body/p[1]","percentage":{percentage},"device":"kindle","device_id":"dev1"{metadata}}}"#
    );
    let key = md5("secret");
    let (status, _) = send(
        app,
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

fn book(title: &str) -> Vec<u8> {
    build_epub_with_metadata(title, "Alice Author", &[], None, None, None)
}

#[tokio::test]
async fn feed_overlays_progress_matched_every_way() {
    let app = app(false).await;
    register_plaintext(&app, "alice", "secret").await;

    let by_hash = upload(&app, "a.epub", &book("A by hash")).await;
    upload(&app, "b.epub", &book("B by file name digest")).await;
    upload(&app, "c.epub", &book("C by literal file name")).await;
    let preferred = upload(&app, "d.epub", &book("D hash beats name")).await;
    upload(&app, "e.epub", &book("E unread")).await;

    sync(&app, &by_hash, 0.1, None).await;
    sync(&app, &md5("b.epub"), 0.2, None).await;
    sync(&app, "some-other-digest", 0.3, Some("c.epub")).await;
    sync(&app, &md5("d.epub"), 0.9, None).await;
    sync(&app, &preferred, 0.4, None).await;

    let (status, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/publications",
        &[("authorization", basic("alice", "secret").as_str())],
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let feed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let progress: Vec<(String, Option<f64>)> = feed["publications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["metadata"]["title"].as_str().unwrap().to_owned(),
                p["metadata"]["kosync"]["percentage"].as_f64(),
            )
        })
        .collect();
    assert_eq!(
        progress,
        [
            ("A by hash".to_owned(), Some(0.1)),
            ("B by file name digest".to_owned(), Some(0.2)),
            ("C by literal file name".to_owned(), Some(0.3)),
            ("D hash beats name".to_owned(), Some(0.4)),
            ("E unread".to_owned(), None),
        ]
    );

    // Progress is per user: the admin has none.
    let (_, _, bytes) = send_bytes(
        &app,
        "GET",
        "/opds/v2/publications",
        &[("authorization", basic("admin", "admin").as_str())],
        Vec::new(),
    )
    .await;
    let feed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        feed["publications"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["metadata"]["kosync"].is_null())
    );
}
