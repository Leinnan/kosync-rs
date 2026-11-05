//! Tests for seeding the `admin` account across server restarts.

#![allow(clippy::unwrap_used)]

mod common;

use axum::{Router, http::StatusCode};
use sqlx::SqlitePool;
use tempfile::TempDir;

use common::{login, md5, send_html, test_config};
use kosync_rs::{Config, build_state, init_db, router};

/// Content type for HTML form submissions.
const FORM: &str = "application/x-www-form-urlencoded";

/// Build a configuration for a deployment whose data lives in `dir`.
fn config(dir: &TempDir, admin_password: Option<&str>) -> Config {
    let url = format!("sqlite://{}/test.db", dir.path().display());
    let books_dir = format!("{}/books", dir.path().display());
    Config {
        admin_password: admin_password.map(str::to_owned),
        ..test_config(&url, &books_dir, false)
    }
}

/// Start the deployment in `dir`, as the server does on boot.
async fn boot(dir: &TempDir, admin_password: Option<&str>) -> SqlitePool {
    init_db(&config(dir, admin_password)).await.unwrap()
}

/// Build the application for a booted deployment.
fn app_for(dir: &TempDir, pool: &SqlitePool) -> Router {
    router(build_state(&config(dir, None), pool.clone()))
}

/// Whether the web login form accepts the given credentials.
async fn can_log_in(app: &Router, username: &str, password: &str) -> bool {
    let (_, headers, _) = send_html(
        app,
        "POST",
        "/login",
        &[("content-type", FORM)],
        &format!("username={username}&password={password}"),
    )
    .await;
    headers.get("set-cookie").is_some()
}

/// The stored `(password_hash, is_active, is_administrator)` of a user.
async fn user_row(pool: &SqlitePool, username: &str) -> Option<(String, bool, bool)> {
    sqlx::query_as(
        "SELECT password_hash, is_active, is_administrator FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// Insert a user directly, bypassing the seeding under test.
async fn insert_user(pool: &SqlitePool, username: &str, password: &str, is_administrator: bool) {
    sqlx::query("INSERT INTO users (username, password_hash, is_administrator) VALUES (?, ?, ?)")
        .bind(username)
        .bind(md5(password))
        .bind(is_administrator)
        .execute(pool)
        .await
        .unwrap();
}

/// Remove the seeded `admin` account.
async fn delete_admin(pool: &SqlitePool) {
    sqlx::query("DELETE FROM users WHERE username = 'admin'")
        .execute(pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn first_boot_creates_admin_with_configured_password() {
    let dir = tempfile::tempdir().unwrap();
    let pool = boot(&dir, Some("first")).await;

    let (hash, is_active, is_administrator) = user_row(&pool, "admin").await.unwrap();
    assert_eq!(hash, md5("first"));
    assert!(is_active);
    assert!(is_administrator);
}

#[tokio::test]
async fn first_boot_without_password_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();

    let error = init_db(&config(&dir, None)).await.unwrap_err();
    assert!(
        error.to_string().contains("KOSYNC_RS_ADMIN_PASSWORD"),
        "the error should name the setting, got: {error}"
    );

    // Nothing was seeded, so setting the password recovers the deployment.
    let pool = boot(&dir, Some("first")).await;
    assert_eq!(user_row(&pool, "admin").await.unwrap().0, md5("first"));
}

#[tokio::test]
async fn password_changed_in_settings_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let pool = boot(&dir, Some("first")).await;
    let app = app_for(&dir, &pool);
    let cookie = login(&app, "admin", "first").await;

    let (status, headers, _) = send_html(
        &app,
        "POST",
        "/settings/password",
        &[("cookie", cookie.as_str()), ("content-type", FORM)],
        "current_password=first&new_password=changed",
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers.get("location").unwrap().to_str().unwrap();
    assert!(location.contains("notice=password-changed"), "{location}");
    pool.close().await;

    let pool = boot(&dir, Some("first")).await;
    let app = app_for(&dir, &pool);
    assert!(can_log_in(&app, "admin", "changed").await);
    assert!(!can_log_in(&app, "admin", "first").await);
}

#[tokio::test]
async fn restart_ignores_a_different_configured_password() {
    let dir = tempfile::tempdir().unwrap();
    boot(&dir, Some("first")).await.close().await;

    let pool = boot(&dir, Some("second")).await;
    assert_eq!(user_row(&pool, "admin").await.unwrap().0, md5("first"));
}

#[tokio::test]
async fn restart_without_password_keeps_existing_admin() {
    let dir = tempfile::tempdir().unwrap();
    boot(&dir, Some("first")).await.close().await;

    let pool = boot(&dir, None).await;
    assert_eq!(user_row(&pool, "admin").await.unwrap().0, md5("first"));
}

#[tokio::test]
async fn restart_leaves_admin_flags_alone() {
    let dir = tempfile::tempdir().unwrap();
    let pool = boot(&dir, Some("first")).await;
    sqlx::query("UPDATE users SET is_active = 0 WHERE username = 'admin'")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;

    let pool = boot(&dir, Some("first")).await;
    let (_, is_active, is_administrator) = user_row(&pool, "admin").await.unwrap();
    assert!(!is_active);
    assert!(is_administrator);
}

#[tokio::test]
async fn another_administrator_prevents_seeding() {
    let dir = tempfile::tempdir().unwrap();
    let pool = boot(&dir, Some("first")).await;
    delete_admin(&pool).await;
    insert_user(&pool, "root", "secret", true).await;
    pool.close().await;

    let pool = boot(&dir, Some("first")).await;
    assert!(user_row(&pool, "admin").await.is_none());
}

#[tokio::test]
async fn seeding_takes_over_a_leftover_admin_username() {
    let dir = tempfile::tempdir().unwrap();
    let pool = boot(&dir, Some("first")).await;
    delete_admin(&pool).await;
    insert_user(&pool, "admin", "registered", false).await;
    pool.close().await;

    // Without a password the account is neither promoted nor reset.
    assert!(init_db(&config(&dir, None)).await.is_err());

    let pool = boot(&dir, Some("second")).await;
    let (hash, is_active, is_administrator) = user_row(&pool, "admin").await.unwrap();
    assert_eq!(hash, md5("second"));
    assert!(is_active);
    assert!(is_administrator);
}
