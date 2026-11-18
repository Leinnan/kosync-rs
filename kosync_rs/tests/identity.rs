//! A library book keeps one progress record whichever identifier a reader
//! syncs it under: its digest, or `md5` of the file name the reader saved it
//! as (`CrossPoint` names OPDS downloads `Author - Title.epub`).

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;

use common::{
    JSON, auth_headers, basic, build_epub_with_metadata, login, md5, multipart, register,
    register_plaintext, send, send_bytes, send_html, test_config,
};
use kosync_rs::{AppState, build_state, init_db, refresh_identities, router};

/// An application plus its state and pool, for tests that touch the database.
async fn app_with_state() -> (Router, AppState, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);
    let config = test_config(&url, &books_dir, false);
    let pool = init_db(&config).await.unwrap();
    let state = build_state(&config, pool.clone()).unwrap();
    (router(state.clone()), state, pool)
}

/// Upload `data` as `file_name` and return the book's digest.
async fn upload(app: &Router, file_name: &str, data: &[u8]) -> String {
    let (body, boundary) = multipart(file_name, "application/epub+zip", data);
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

/// The Gra o tron book, uploaded under a name no reader will use.
async fn upload_gra_o_tron(app: &Router) -> String {
    let epub =
        build_epub_with_metadata("Gra o tron 1", "George R. R. Martin", &[], None, None, None);
    upload(app, "01 Gra o tron 1 - George R. R. Martin.epub", &epub).await
}

/// `md5` of the name `CrossPoint` gives the book when downloading it over OPDS.
fn crosspoint_hash() -> String {
    md5("George R. R. Martin - Gra o tron 1.epub")
}

/// `PUT` progress as `alice`, with optional `KOReader` metadata JSON.
async fn put(app: &Router, document: &str, percentage: f64, device: &str, metadata: Option<&str>) {
    let mut headers = auth_headers();
    headers.push((JSON, "application/json"));
    let metadata = metadata
        .map(|m| format!(r#","metadata":{m}"#))
        .unwrap_or_default();
    let body = format!(
        r#"{{"document":"{document}","progress":"/body/DocFragment[3]/body/p[1]","percentage":{percentage},"device":"{device}"{metadata}}}"#
    );
    let (status, _) = send(app, "PUT", "/syncs/progress", &headers, &body).await;
    assert_eq!(status, StatusCode::OK);
}

/// `GET` progress as `alice`.
async fn get(app: &Router, document: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        "GET",
        &format!("/syncs/progress/{document}"),
        &auth_headers(),
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body
}

#[tokio::test]
async fn crosspoint_download_name_resolves_before_any_upload_from_it() {
    let (app, ..) = app_with_state().await;
    register(&app).await;
    let book = upload_gra_o_tron(&app).await;

    put(&app, &book, 0.37, "Skryba", None).await;

    let found = get(&app, &crosspoint_hash()).await;
    assert_eq!(found["percentage"], 0.37);
    assert_eq!(found["device"], "Skryba");
    // The reader gets back the identifier it asked for.
    assert_eq!(found["document"], crosspoint_hash());
}

#[tokio::test]
async fn progress_under_either_identifier_is_one_record() {
    let (app, _, pool) = app_with_state().await;
    register(&app).await;
    let book = upload_gra_o_tron(&app).await;

    put(&app, &book, 0.37, "Skryba", None).await;
    put(&app, &crosspoint_hash(), 0.42, "CrossPoint", None).await;
    assert_eq!(get(&app, &book).await["percentage"], 0.42);
    assert_eq!(get(&app, &book).await["device"], "CrossPoint");

    put(&app, &book, 0.45, "Skryba", None).await;
    assert_eq!(get(&app, &crosspoint_hash()).await["percentage"], 0.45);

    let hashes: Vec<String> = sqlx::query_scalar("SELECT document_hash FROM documents")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(hashes, vec![book.clone()]);
    let events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM progress_events WHERE document_hash = ?")
            .bind(&book)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(events, 3);
}

#[tokio::test]
async fn a_renamed_file_is_learned_from_its_metadata() {
    let (app, _, pool) = app_with_state().await;
    register(&app).await;
    let book = upload_gra_o_tron(&app).await;
    let renamed = md5("gra-o-tron.epub");

    put(
        &app,
        &renamed,
        0.5,
        "KOReader",
        Some(r#"{"filename":"gra-o-tron.epub","title":"Gra o Tron  1","authors":"George R. R. Martin"}"#),
    )
    .await;

    // Later syncs without metadata resolve too.
    assert_eq!(get(&app, &book).await["percentage"], 0.5);
    assert_eq!(get(&app, &renamed).await["percentage"], 0.5);
    let kind: String = sqlx::query_scalar("SELECT kind FROM publication_aliases WHERE hash = ?")
        .bind(&renamed)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kind, "learned");
}

#[tokio::test]
async fn metadata_is_not_trusted_for_a_digest_it_does_not_explain() {
    let (app, ..) = app_with_state().await;
    register(&app).await;
    let book = upload_gra_o_tron(&app).await;
    // A binary digest of some other edition: md5(filename) is not it.
    let other = md5("some other file bytes");

    put(
        &app,
        &other,
        0.9,
        "KOReader",
        Some(r#"{"filename":"gra-o-tron.epub","title":"Gra o tron 1"}"#),
    )
    .await;

    assert!(get(&app, &book).await.get("percentage").is_none());
    assert_eq!(get(&app, &other).await["percentage"], 0.9);
}

#[tokio::test]
async fn a_title_two_books_share_is_not_guessed() {
    let (app, ..) = app_with_state().await;
    register(&app).await;
    let first = build_epub_with_metadata("Poems", "Ann Author", &[], None, None, None);
    let second = build_epub_with_metadata("Poems", "Bob Writer", &[], None, None, None);
    let first = upload(&app, "a.epub", &first).await;
    let second = upload(&app, "b.epub", &second).await;
    put(&app, &first, 0.2, "Skryba", None).await;
    put(&app, &second, 0.6, "Skryba", None).await;

    // `Poems.epub` could be either book.
    assert!(
        get(&app, &md5("Poems.epub"))
            .await
            .get("percentage")
            .is_none()
    );
    // The author tells them apart.
    assert_eq!(
        get(&app, &md5("Ann Author - Poems.epub")).await["percentage"],
        0.2
    );
    assert_eq!(
        get(&app, &md5("Poems - Bob Writer.epub")).await["percentage"],
        0.6
    );
}

#[tokio::test]
async fn split_records_from_older_releases_are_merged() {
    let (app, state, pool) = app_with_state().await;
    register(&app).await;
    let book = upload_gra_o_tron(&app).await;
    let crosspoint = crosspoint_hash();
    put(&app, &book, 0.37, "Skryba", None).await;
    put(&app, &book, 0.376, "Skryba", None).await;

    // What an older release stored: CrossPoint's record under its own
    // identifier, synced after Skryba.
    let user: i64 = sqlx::query_scalar("SELECT id FROM users WHERE username = 'alice'")
        .fetch_one(&pool)
        .await
        .unwrap();
    for (table, extra) in [("documents", ", title, filename"), ("progress_events", "")] {
        let values = if extra.is_empty() {
            ""
        } else {
            ", 'Gra o tron 1', 'George R. R. Martin - Gra o tron 1.epub'"
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (user_id, document_hash, progress, percentage, device, timestamp{extra})
             VALUES (?, ?, '/body/DocFragment[37]/body/p[12]', 0.419, 'CrossPoint', 4102444800{values})"
        )))
        .bind(user)
        .bind(&crosspoint)
        .execute(&pool)
        .await
        .unwrap();
    }

    refresh_identities(&state).await;

    let rows: Vec<(String, f64, String)> =
        sqlx::query_as("SELECT document_hash, percentage, device FROM documents")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(book.clone(), 0.419, "CrossPoint".to_owned())]);
    let events: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT document_hash FROM progress_events")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(events, vec![book.clone()]);
    let history: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM progress_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(history, 3);
}

#[tokio::test]
async fn an_unmatched_sync_can_be_linked_to_a_book_by_hand() {
    let (app, ..) = app_with_state().await;
    register_plaintext(&app, "bob", "secret").await;
    let book = upload_gra_o_tron(&app).await;
    let unknown = "4445d246f0922233500e38b223f7f28c";

    let key = md5("secret");
    let auth = [
        ("x-auth-user", "bob"),
        ("x-auth-key", key.as_str()),
        (JSON, "application/json"),
    ];
    let body = format!(
        r#"{{"document":"{unknown}","progress":"/body/p[1]","percentage":0.3,"device":"Kindle"}}"#
    );
    send(&app, "PUT", "/syncs/progress", &auth, &body).await;

    let bob = login(&app, "bob", "secret").await;
    let (_, _, page) = send_html(
        &app,
        "GET",
        &format!("/documents/{unknown}"),
        &[("cookie", bob.as_str())],
        "",
    )
    .await;
    assert!(page.contains(&format!(r#"action="/documents/{unknown}/link""#)));
    assert!(page.contains(&format!(r#"value="{book}""#)));

    let form = format!("publication={book}");
    let (status, headers, _) = send_bytes(
        &app,
        "POST",
        &format!("/documents/{unknown}/link"),
        &[
            ("cookie", bob.as_str()),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        form.into_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], format!("/documents/{book}"));

    let (_, _, page) = send_html(
        &app,
        "GET",
        &format!("/documents/{book}"),
        &[("cookie", bob.as_str())],
        "",
    )
    .await;
    assert!(page.contains("Gra o tron 1"));
    assert!(!page.contains("/link\""));

    // The Kindle's next sync lands on the book too.
    let (_, found) = send(
        &app,
        "GET",
        &format!("/syncs/progress/{book}"),
        &auth[..2],
        "",
    )
    .await;
    assert_eq!(found["percentage"], 0.3);
}
