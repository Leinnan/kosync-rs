//! kosync-rs: a self-hostable [KOReader](https://koreader.rocks/) sync server.
//!
//! This crate implements the `KOReader` sync protocol, a `/manage/*` management
//! API, and a server-side-rendered web frontend, all backed by a single `SQLite`
//! database file. The binary entrypoint is a thin wrapper over [`run`].

mod auth;
mod basic_auth;
mod config;
mod cover;
mod db;
mod epub;
mod error;
mod i18n;
mod identity;
mod library;
/// EPUB metadata extraction and optional external enrichment.
pub mod metadata;
mod mobi;
mod models;
mod opds;
mod routes;
mod state;
mod store;
mod util;
mod web;

use std::sync::Arc;
use std::time::Duration;

use web_mev_auth::{AuthConfig, SessionStore};
use web_mev_core::PasswordHasher;
use web_mev_runtime::{ShutdownSignal, Workers, sleep_or_stop};

pub use config::{Config, ConfigError};
pub use state::AppState;

/// Connect to the database, run migrations, and seed the `admin` user if the
/// database has no administrator.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, migrations fail, or the
/// admin user cannot be seeded, including when it must be created and no password
/// is configured.
pub async fn init_db(
    config: &Config,
) -> Result<sqlx::SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    db::init(config).await
}

/// Build the shared application state from configuration and a connection pool.
///
/// This loads the templates and static files (embedded, or from the configured
/// development directories) and sets up browser sessions.
///
/// # Errors
///
/// Returns an error when a template does not compile.
pub fn build_state(
    config: &Config,
    pool: sqlx::SqlitePool,
) -> Result<AppState, Box<dyn std::error::Error + Send + Sync>> {
    let i18n = Arc::new(i18n::I18n::load(&config.default_language)?);
    let assets = web::assets(config.static_dir.clone());
    let templates = web::templates(&assets, config.template_dir.clone(), i18n.clone())?;
    let sessions = SessionStore::new(
        pool.clone(),
        AuthConfig::new("kosync", config.cookie_secure),
    )
    // KOReader accepts any non-empty password, so the web sign-in does too.
    .with_hasher(PasswordHasher::new(4, 1..=1024));
    Ok(AppState::new(
        pool,
        config.clone(),
        templates,
        assets,
        sessions,
        i18n,
    ))
}

/// Fill in covers and thumbnails that older imports left empty, returning the
/// number of publications updated. [`run`] does this in the background on
/// startup.
pub async fn backfill_covers(state: &AppState) -> usize {
    library::backfill_covers(state).await
}

/// Recognise every identifier readers may use for each library book and merge
/// progress recorded under them into the book's own record. [`run`] does this
/// on startup, and each import does it again.
pub async fn refresh_identities(state: &AppState) {
    library::refresh_identities(state).await;
}

/// Assemble the full application router for the given state.
pub fn router(state: AppState) -> axum::Router {
    routes::router(state)
}

/// How often expired sessions and stale login throttles are deleted.
const SESSION_PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// How long background work gets to finish once the server has stopped.
const WORKER_GRACE: Duration = Duration::from_secs(5);

/// Load configuration, initialize the database, and serve requests until
/// `SIGTERM` or `SIGINT`, then finish in-flight requests and stop.
///
/// The caller loads `.env` and sets up logging first; see `main.rs`.
///
/// # Errors
///
/// Returns an error if the configuration is invalid, the database cannot be
/// initialized, the listener cannot be bound, or the server fails at runtime.
pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    let shutdown = ShutdownSignal::install()?;

    let pool = init_db(&config).await?;
    let state = build_state(&config, pool)?;

    let rehashed = library::rehash_publications(&state).await;
    if rehashed > 0 {
        tracing::info!(rehashed, "corrected publication document digests");
    }
    // Before serving, so sync requests see one record per book.
    library::refresh_identities(&state).await;

    // Re-reads KOSYNC_RS_TEMPLATE_DIR on edits while the guard lives.
    #[cfg(feature = "reload")]
    let _template_watch = state
        .templates
        .watch()
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let mut workers = Workers::new();
    let backfill_state = state.clone();
    workers.spawn("cover-backfill", move |_stop| async move {
        let filled = backfill_covers(&backfill_state).await;
        if filled > 0 {
            tracing::info!(
                filled,
                "filled in missing publication covers and thumbnails"
            );
        }
    });

    let sessions = state.sessions.clone();
    workers.spawn("session-prune", move |mut stop| async move {
        loop {
            match sessions.prune_expired().await {
                Ok(0) => {}
                Ok(pruned) => tracing::debug!(pruned, "pruned expired sessions"),
                Err(error) => tracing::warn!(%error, "could not prune expired sessions"),
            }
            if sleep_or_stop(&mut stop, SESSION_PRUNE_INTERVAL).await {
                break;
            }
        }
    });

    let app = routes::router(state);
    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("listening on {addr}");
    web_mev_axum::serve(listener, app, async move {
        let signal = shutdown.wait().await;
        tracing::info!(signal, "shutting down");
    })
    .await?;

    workers.shutdown(WORKER_GRACE).await;
    Ok(())
}
