//! Shared application state.

use std::sync::Arc;

use sqlx::SqlitePool;
use web_mev_assets::Assets;
use web_mev_auth::SessionStore;
use web_mev_axum::{ErrorSurface, FlashStore, TrustedProxies};
use web_mev_templates::Templates;

use crate::{config::Config, i18n::I18n};

/// State shared by all request handlers.
#[derive(Clone)]
pub struct AppState {
    /// The `SQLite` connection pool.
    pub(crate) pool: SqlitePool,
    /// Runtime configuration.
    pub(crate) config: Config,
    /// Page templates for the server-rendered frontend.
    pub(crate) templates: Templates,
    /// Static stylesheets and scripts.
    pub(crate) assets: Assets,
    /// Browser sessions, CSRF tokens and login throttling for the web UI.
    pub(crate) sessions: SessionStore,
    /// One-shot messages shown on the page after a redirect.
    pub(crate) flash: FlashStore,
    /// Translations, and how a request picks its language.
    pub(crate) i18n: Arc<I18n>,
    /// Trusted reverse proxy addresses.
    pub(crate) trusted_proxies: TrustedProxies,
    /// Shared `HTTP` client for metadata enrichment; only built when
    /// [`Config::metadata_enrichment`] is enabled.
    pub(crate) http_client: Option<reqwest::Client>,
}

impl AppState {
    /// Create a new application state.
    pub(crate) fn new(
        pool: SqlitePool,
        config: Config,
        templates: Templates,
        assets: Assets,
        sessions: SessionStore,
        i18n: Arc<I18n>,
    ) -> Self {
        let trusted_proxies = TrustedProxies::new(config.trusted_proxies.iter().copied());
        let http_client = config
            .metadata_enrichment
            .then(crate::metadata::build_client)
            .flatten();
        Self {
            pool,
            config,
            templates,
            assets,
            flash: FlashStore::new("kosync_flash", sessions.config().secure),
            sessions,
            i18n,
            trusted_proxies,
            http_client,
        }
    }
}

impl axum::extract::FromRef<AppState> for TrustedProxies {
    fn from_ref(state: &AppState) -> Self {
        state.trusted_proxies.clone()
    }
}

impl AppState {
    /// Hash a new password for signing in to the web UI.
    ///
    /// # Errors
    ///
    /// Returns an error when the password is empty or too long, or hashing
    /// fails.
    pub(crate) async fn hash_web_password(
        &self,
        password: &str,
    ) -> Result<String, web_mev_core::PasswordError> {
        self.sessions
            .hasher()
            .hash(password.to_owned())
            .await
            .inspect_err(|error| tracing::error!(%error, "could not hash a password"))
    }
}

impl axum::extract::FromRef<AppState> for SessionStore {
    fn from_ref(state: &AppState) -> Self {
        state.sessions.clone()
    }
}

impl ErrorSurface for AppState {
    type Html = crate::web::HtmlError;
    type Json = crate::web::JsonError;
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("pool", &self.pool)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}
