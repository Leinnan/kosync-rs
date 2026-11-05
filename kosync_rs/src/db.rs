//! Database connection setup, migrations, and admin seeding.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

use crate::config::{Config, ConfigError};

/// Password earlier versions gave the `admin` user when none was configured.
const OLD_DEFAULT_ADMIN_PASSWORD: &str = "admin";

/// Compute the MD5 hex digest of `input`.
///
/// The `KOReader` client `MD5`-hashes the password before sending it, so the
/// server must store and compare hashes in this exact format.
pub(crate) fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let digest = Md5::digest(input.as_bytes());
    format!("{digest:x}")
}

/// Connect to the `SQLite` database, run migrations, and seed the admin user
/// if the database has no administrator.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, migrations fail, or an
/// administrator must be created and no password is configured.
pub(crate) async fn init(
    config: &Config,
) -> Result<SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    ensure_parent_dir(&config.database_url)?;

    let options = SqliteConnectOptions::from_str(&config.database_url)?
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new().connect_with(options).await?;

    run_migrations(&pool).await?;
    seed_admin(&pool, config.admin_password.as_deref()).await?;

    Ok(pool)
}

/// Create the parent directory for a file-backed `SQLite` database, if needed.
fn ensure_parent_dir(database_url: &str) -> std::io::Result<()> {
    let path = database_url
        .strip_prefix("sqlite://")
        .or_else(|| database_url.strip_prefix("sqlite:"))
        .unwrap_or(database_url);

    if path.is_empty() || path.starts_with(":memory:") {
        return Ok(());
    }

    if let Some(parent) = Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }

    Ok(())
}

/// Run pending SQL migrations.
async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

/// Create the `admin` user if the database has no administrator.
///
/// An existing administrator is never modified, so a password changed after the
/// first start survives restarts. With no administrator and no configured
/// password, startup fails rather than creating an account anyone could guess. A
/// non-administrator already named `admin` is taken over, and its password
/// replaced so whoever registered the name cannot sign in as administrator.
async fn seed_admin(
    pool: &SqlitePool,
    admin_password: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let has_administrator = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM users WHERE is_administrator = 1)",
    )
    .fetch_one(pool)
    .await?;

    if !has_administrator {
        let password = admin_password.ok_or_else(|| {
            ConfigError("KOSYNC_RS_ADMIN_PASSWORD must be set to create the admin user".into())
        })?;
        sqlx::query(
            "INSERT INTO users (username, password_hash, is_active, is_administrator) VALUES ('admin', ?, 1, 1)
             ON CONFLICT (username) DO UPDATE SET password_hash = excluded.password_hash, is_active = 1, is_administrator = 1",
        )
        .bind(md5_hex(password))
        .execute(pool)
        .await?;
    }

    let uses_old_default = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM users WHERE username = 'admin' AND is_administrator = 1 AND password_hash = ?)",
    )
    .bind(md5_hex(OLD_DEFAULT_ADMIN_PASSWORD))
    .fetch_one(pool)
    .await?;
    if uses_old_default {
        tracing::warn!("the admin user's password is `admin`; change it at /settings");
    }

    Ok(())
}
