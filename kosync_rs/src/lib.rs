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
mod library;
/// EPUB metadata extraction and optional external enrichment.
pub mod metadata;
mod middleware;
mod models;
mod opds;
mod routes;
mod state;
mod store;
mod util;
mod web;

use std::net::SocketAddr;
use std::sync::Arc;

use tower_cookies::Key;
use tracing_subscriber::EnvFilter;

pub use config::{Config, ConfigError};
pub use state::AppState;

/// Connect to the database, run migrations, and seed the `admin` user.
///
/// # Errors
///
/// Returns an error if the database cannot be opened, migrations fail, or the
/// admin user cannot be seeded.
pub async fn init_db(
    config: &Config,
) -> Result<sqlx::SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    db::init(config).await
}

/// Build the shared application state from configuration and a connection pool.
///
/// This compiles the embedded templates, generates an ephemeral cookie-signing
/// key, and pre-parses the trusted proxy list.
#[must_use]
pub fn build_state(config: &Config, pool: sqlx::SqlitePool) -> AppState {
    let tera = Arc::new(web::templates());
    let cookie_key = Arc::new(Key::generate());
    AppState::new(pool, config.clone(), tera, cookie_key)
}

/// Assemble the full application router for the given state.
pub fn router(state: AppState) -> axum::Router {
    routes::router(state)
}

/// Load configuration, initialize the database, and serve requests forever.
///
/// # Errors
///
/// Returns an error if the configuration is invalid, the database cannot be
/// initialized, the listener cannot be bound, or the server fails at runtime.
pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = Config::from_env()?;
    init_tracing(&config);

    let pool = init_db(&config).await?;
    let state = build_state(&config, pool);

    let app = routes::router(state.clone()).layer(axum::middleware::from_fn_with_state(
        state,
        middleware::access_log,
    ));

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    tracing::info!("listening on {addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);

    if config.single_line_logging {
        builder.compact().init();
    } else {
        builder.init();
    }
}
