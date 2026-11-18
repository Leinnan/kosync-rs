//! Database connection setup, migrations, and admin seeding.

use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePool;
use web_mev_sqlite::DbTuning;

use crate::config::{Config, ConfigError};

/// The embedded schema migrations.
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

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

/// Connect to the `SQLite` database (WAL, full sync, foreign keys), run
/// migrations, and seed the admin user
/// if the database has no administrator.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, migrations fail, or an
/// administrator must be created and no password is configured.
pub(crate) async fn init(
    config: &Config,
) -> Result<SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    let pool =
        web_mev_sqlite::connect_with(&config.database_url, &MIGRATOR, &DbTuning::default()).await?;
    seed_admin(&pool, config.admin_password.as_deref()).await?;

    Ok(pool)
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
             ON CONFLICT (username) DO UPDATE SET password_hash = excluded.password_hash, web_password_hash = NULL, is_active = 1, is_administrator = 1",
        )
        .bind(md5_hex(password))
        .execute(pool)
        .await?;
        // A taken-over account keeps none of its former owner's sessions.
        sqlx::query(
            "DELETE FROM sessions WHERE user_id = (SELECT id FROM users WHERE username = 'admin')",
        )
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
