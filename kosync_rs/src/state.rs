//! Shared application state.

use std::net::IpAddr;
use std::sync::Arc;

use sqlx::SqlitePool;
use tera::Tera;
use tower_cookies::Key;

use crate::config::Config;

/// State shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    /// The `SQLite` connection pool.
    pub(crate) pool: SqlitePool,
    /// Runtime configuration.
    pub(crate) config: Config,
    /// Template engine for the server-rendered frontend.
    pub(crate) tera: Arc<Tera>,
    /// Key used to sign/encrypt session cookies.
    pub(crate) cookie_key: Arc<Key>,
    /// Pre-parsed list of trusted reverse proxy addresses.
    pub(crate) trusted_proxies: Vec<IpAddr>,
    /// Shared `HTTP` client for metadata enrichment; only built when
    /// [`Config::metadata_enrichment`] is enabled.
    pub(crate) http_client: Option<reqwest::Client>,
}

impl AppState {
    /// Create a new application state.
    pub(crate) fn new(
        pool: SqlitePool,
        config: Config,
        tera: Arc<Tera>,
        cookie_key: Arc<Key>,
    ) -> Self {
        let trusted_proxies = config.parse_trusted_proxies();
        let http_client = config
            .metadata_enrichment
            .then(crate::metadata::build_client)
            .flatten();
        Self {
            pool,
            config,
            tera,
            cookie_key,
            trusted_proxies,
            http_client,
        }
    }
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("pool", &self.pool)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
