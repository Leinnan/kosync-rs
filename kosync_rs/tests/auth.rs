//! Web sessions: CSRF checks, the MD5-to-Argon2 upgrade on sign-in, session
//! revocation, the login throttle, and `KOReader` sync after a web password
//! change.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;

use common::{
    JSON, build_epub, cookie_named, login, md5, multipart, post_login, register_plaintext, send,
    send_bytes, send_html, test_config,
};
use kosync_rs::{build_state, init_db, router};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";

/// Build an application, keeping its pool for direct database access.
async fn app_with_pool() -> (Router, SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    std::mem::forget(dir);

    let config = test_config(&url, &books_dir, false);
    let pool = init_db(&config).await.unwrap();
    (router(build_state(&config, pool.clone()).unwrap()), pool)
}

/// The stored web password hash of `username`.
async fn web_hash(pool: &SqlitePool, username: &str) -> Option<String> {
    sqlx::query_scalar("SELECT web_password_hash FROM users WHERE username = ?")
        .bind(username)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Whether `cookie` still opens the dashboard.
async fn signed_in(app: &Router, cookie: &str) -> bool {
    let (status, _, _) = send_html(app, "GET", "/", &[("cookie", cookie)], "").await;
    status == StatusCode::OK
}

#[tokio::test]
async fn forms_without_the_session_csrf_token_are_refused() {
    let (app, pool) = app_with_pool().await;
    let admin = login(&app, "admin", "admin").await;

    for token in ["wrong", ""] {
        let (status, _, _) = send_html(
            &app,
            "POST",
            "/admin/users/create",
            &[
                ("cookie", admin.as_str()),
                ("content-type", FORM),
                ("x-csrf-token", token),
            ],
            "username=mallory&password=pw",
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "token {token:?}");
    }
    let created: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE username = 'mallory'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(created, 0);

    // The token as a form field works, as a plain HTML form sends it.
    let token = common::session_csrf(&app, &admin).await;
    let (status, _, _) = common::raw_send(
        &app,
        "POST",
        "/admin/users/create",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        axum::body::Body::from(format!("username=mallory&password=pw&csrf_token={token}")),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn uploads_need_the_csrf_token() {
    let app = common::app(false).await;
    let admin = login(&app, "admin", "admin").await;
    let (body, boundary) = multipart("book.epub", "application/epub+zip", &build_epub(None));
    let content_type = format!("multipart/form-data; boundary={boundary}");

    let (status, _, _) = send_bytes(
        &app,
        "POST",
        "/books/upload",
        &[
            ("cookie", admin.as_str()),
            ("content-type", content_type.as_str()),
            ("x-csrf-token", "wrong"),
        ],
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, page) = send_bytes(
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
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&page).contains("Added to the library"));
}

#[tokio::test]
async fn login_needs_the_anonymous_csrf_token() {
    let app = common::app(false).await;
    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, FORM)],
        "username=admin&password=admin",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(cookie_named(&headers, "kosync_session").is_none());
}

#[tokio::test]
async fn koreader_accounts_get_a_web_password_on_first_sign_in() {
    let (app, pool) = app_with_pool().await;
    register_plaintext(&app, "alice", "secret").await;
    assert_eq!(web_hash(&pool, "alice").await, None);

    // A wrong password neither signs in nor stores anything.
    let (_, headers, _) = post_login(&app, "username=alice&password=wrong").await;
    assert!(cookie_named(&headers, "kosync_session").is_none());
    assert_eq!(web_hash(&pool, "alice").await, None);

    let cookie = login(&app, "alice", "secret").await;
    assert!(signed_in(&app, &cookie).await);
    let hash = web_hash(&pool, "alice").await.unwrap();
    assert!(hash.starts_with("$argon2id$"), "{hash}");
}

#[tokio::test]
async fn changing_a_password_signs_out_other_sessions_only() {
    let app = common::app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let here = login(&app, "alice", "secret").await;
    let elsewhere = login(&app, "alice", "secret").await;

    let (status, _, _) = send_html(
        &app,
        "POST",
        "/settings/password",
        &[("cookie", here.as_str()), ("content-type", FORM)],
        "current_password=secret&new_password=newpass",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(signed_in(&app, &here).await);
    assert!(!signed_in(&app, &elsewhere).await);

    // KOReader syncs with the MD5 of the new password.
    let new_key = md5("newpass");
    let (status, _) = send(
        &app,
        "GET",
        "/users/auth",
        &[("x-auth-user", "alice"), ("x-auth-key", new_key.as_str())],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, headers, _) = post_login(&app, "username=alice&password=secret").await;
    assert!(cookie_named(&headers, "kosync_session").is_none());
}

#[tokio::test]
async fn admin_reset_and_deactivation_sign_the_user_out() {
    let app = common::app(false).await;
    register_plaintext(&app, "alice", "secret").await;
    let admin = login(&app, "admin", "admin").await;

    let alice = login(&app, "alice", "secret").await;
    send_html(
        &app,
        "POST",
        "/admin/users/alice/password",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "password=reset",
    )
    .await;
    assert!(!signed_in(&app, &alice).await);

    let alice = login(&app, "alice", "reset").await;
    send_html(
        &app,
        "POST",
        "/admin/users/alice/active",
        &[("cookie", admin.as_str()), ("content-type", FORM)],
        "",
    )
    .await;
    assert!(!signed_in(&app, &alice).await);
    let (_, headers, _) = post_login(&app, "username=alice&password=reset").await;
    assert!(cookie_named(&headers, "kosync_session").is_none());
}

#[tokio::test]
async fn repeated_failures_lock_the_username() {
    let app = common::app(false).await;
    for _ in 0..5 {
        let (_, _, page) = post_login(&app, "username=admin&password=wrong").await;
        assert!(page.contains("Invalid username or password"));
    }
    let (_, headers, page) = post_login(&app, "username=admin&password=admin").await;
    assert!(cookie_named(&headers, "kosync_session").is_none());
    assert!(page.contains("Too many failed sign-ins"), "{page}");
}

/// The `csrf_token` field of a rendered login form.
fn form_token(page: &str) -> String {
    let marker = r#"name="csrf_token" value=""#;
    let start = page.find(marker).unwrap() + marker.len();
    page[start..].split('"').next().unwrap().to_owned()
}

#[tokio::test]
async fn a_second_login_page_keeps_the_first_forms_token() {
    let app = common::app(false).await;
    let (_, headers, first) = send_html(&app, "GET", "/login", &[], "").await;
    let cookie = cookie_named(&headers, "kosync_anon_csrf").unwrap();
    let token = form_token(&first);
    assert_eq!(cookie, format!("kosync_anon_csrf={token}"));

    // The browser loads /login again (another tab, a reload, a prerender):
    // the page reuses the token instead of replacing the cookie.
    let (_, headers, second) =
        send_html(&app, "GET", "/login", &[("cookie", cookie.as_str())], "").await;
    assert_eq!(form_token(&second), token);
    assert!(cookie_named(&headers, "kosync_anon_csrf").is_none());

    // So the form from the first page still signs in.
    let (_, headers, _) = send_html(
        &app,
        "POST",
        "/login",
        &[(JSON, FORM), ("cookie", cookie.as_str())],
        &format!("username=admin&password=admin&csrf_token={token}"),
    )
    .await;
    assert!(cookie_named(&headers, "kosync_session").is_some());
}

#[tokio::test]
async fn a_malformed_anonymous_csrf_cookie_is_replaced() {
    let app = common::app(false).await;
    let (_, headers, page) = send_html(
        &app,
        "GET",
        "/login",
        &[("cookie", "kosync_anon_csrf=<script>")],
        "",
    )
    .await;
    let token = form_token(&page);
    assert_eq!(token.len(), 32);
    assert_eq!(
        cookie_named(&headers, "kosync_anon_csrf").unwrap(),
        format!("kosync_anon_csrf={token}")
    );
}
