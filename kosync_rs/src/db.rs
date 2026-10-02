//! Database connection setup, migrations, and admin seeding.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

use crate::config::Config;

/// Compute the MD5 hex digest of `input`.
///
/// The `KOReader` client `MD5`-hashes the password before sending it, so the
/// server must store and compare hashes in this exact format.
pub(crate) fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let digest = Md5::digest(input.as_bytes());
    format!("{digest:x}")
}

/// Connect to the `SQLite` database, run migrations, and seed the admin user.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrations fail.
pub(crate) async fn init(
    config: &Config,
) -> Result<SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    ensure_parent_dir(&config.database_url)?;

    let options = SqliteConnectOptions::from_str(&config.database_url)?
        .create_if_missing(true)
        .foreign_keys(true);

    let pool = SqlitePoolOptions::new().connect_with(options).await?;

    run_migrations(&pool).await?;
    seed_admin(&pool, &config.admin_password).await?;

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

/// Ensure the `admin` user exists and has the configured password hash.
async fn seed_admin(pool: &SqlitePool, admin_password: &str) -> Result<(), sqlx::Error> {
    let hash = md5_hex(admin_password);

    let existing =
        sqlx::query_scalar::<_, i64>("SELECT id FROM users WHERE username = 'admin' LIMIT 1")
            .fetch_optional(pool)
            .await?;

    match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE users SET password_hash = ?, is_administrator = 1, is_active = 1 WHERE id = ?",
            )
            .bind(&hash)
            .bind(id)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO users (username, password_hash, is_active, is_administrator) VALUES ('admin', ?, 1, 1)",
            )
            .bind(&hash)
            .execute(pool)
            .await?;
        }
    }

    Ok(())
}
