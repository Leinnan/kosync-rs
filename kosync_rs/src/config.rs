//! Application configuration loaded from the environment.

use std::{fmt, net::IpAddr, path::PathBuf};

use web_mev_core::{Env, EnvError};

/// Runtime configuration for the sync server.
///
/// All values are read from the environment, prefixed with `KOSYNC_RS_`, and
/// can be overridden with a `.env` file. Process environment variables take
/// precedence over values from `.env`, and a missing `.env` file is ignored.
#[derive(Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub host: String,

    /// Port the HTTP server listens on.
    pub port: u16,

    /// `SQLite` connection string (e.g. `sqlite://data/kosync.db`).
    pub database_url: String,

    /// Password for the `admin` user created when the database has no
    /// administrator. Required in that case and ignored otherwise, so changing
    /// it does not change an existing account. MD5-hashed before storage.
    pub admin_password: Option<String>,

    /// When `true`, new user registration via `/users/create` is rejected.
    ///
    /// Enabled by default: accounts are created by an administrator. Set
    /// `KOSYNC_RS_REGISTRATION_DISABLED=false` to allow public sign-up.
    pub registration_disabled: bool,

    /// Trusted reverse proxy addresses, from a comma-separated list.
    pub trusted_proxies: Vec<IpAddr>,

    /// Directory where uploaded `EPUB` files and generated covers are stored.
    pub books_dir: String,

    /// Maximum accepted size, in bytes, for an uploaded `EPUB` file.
    pub max_upload_bytes: usize,

    /// When `true`, uploaded books are enriched in the background with
    /// metadata (genres, ISBN, covers) fetched from external book APIs.
    ///
    /// Disabled by default: enabling it means the server makes outbound
    /// requests revealing the ISBNs of uploaded books.
    pub metadata_enrichment: bool,

    /// Public base URL (e.g. `https://books.example.com`) used for absolute
    /// links in OPDS feeds. When unset, links are built from the request's
    /// `Host` header, and from `X-Forwarded-Proto`/`X-Forwarded-Host` when
    /// the request comes through a trusted proxy.
    pub public_url: Option<String>,

    /// Language for visitors whose browser asks for none of the supported
    /// ones, and for e-readers that send no `Accept-Language`: `en` or `pl`.
    pub default_language: String,

    /// Mark session, CSRF and flash cookies `Secure`, so browsers only send
    /// them over HTTPS. Defaults to `true` when the public URL is `https`.
    pub cookie_secure: bool,

    /// Read templates from this directory instead of the binary, for
    /// development. With the `reload` feature, edits show up without a restart.
    pub template_dir: Option<PathBuf>,

    /// Serve `/static/` from this directory instead of the binary, for
    /// development.
    pub static_dir: Option<PathBuf>,
}

impl fmt::Debug for Config {
    // The admin password is a secret, so only whether it is set is shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database_url", &self.database_url)
            .field(
                "admin_password",
                &self.admin_password.as_ref().map(|_| "<redacted>"),
            )
            .field("registration_disabled", &self.registration_disabled)
            .field("trusted_proxies", &self.trusted_proxies)
            .field("books_dir", &self.books_dir)
            .field("max_upload_bytes", &self.max_upload_bytes)
            .field("metadata_enrichment", &self.metadata_enrichment)
            .field("public_url", &self.public_url)
            .field("default_language", &self.default_language)
            .field("cookie_secure", &self.cookie_secure)
            .field("template_dir", &self.template_dir)
            .field("static_dir", &self.static_dir)
            .finish()
    }
}

/// A configuration error that never includes configured secret values.
#[derive(Debug)]
pub struct ConfigError(pub(crate) String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl From<EnvError> for ConfigError {
    fn from(error: EnvError) -> Self {
        Self(error.to_string())
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_owned(),
            port: 8090,
            database_url: "sqlite://data/kosync.db".to_owned(),
            admin_password: None,
            registration_disabled: true,
            trusted_proxies: Vec::new(),
            books_dir: "data/books".to_owned(),
            max_upload_bytes: 209_715_200,
            metadata_enrichment: false,
            public_url: None,
            default_language: "en".to_owned(),
            cookie_secure: false,
            template_dir: None,
            static_dir: None,
        }
    }
}

impl Config {
    /// Read and parse the `KOSYNC_RS_*` process environment.
    ///
    /// Call [`web_mev_runtime::load_dotenv`] first to fill it from a `.env`
    /// file; process environment variables take precedence over the file.
    ///
    /// # Errors
    ///
    /// Returns an error if a setting has a malformed value. The error never
    /// includes configured secret values.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&Env::system("KOSYNC_RS_"))
    }

    /// Read the configuration from `env`, applying defaults for unset values.
    fn from_source(env: &Env) -> Result<Self, ConfigError> {
        let defaults = Self::default();
        let public_url = parse_public_url(&env.var("PUBLIC_URL")?.unwrap_or_default())?;
        let https = public_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://"));
        Ok(Self {
            host: env.var("HOST")?.unwrap_or(defaults.host),
            port: env.parse("PORT", defaults.port)?,
            database_url: env.var("DATABASE_URL")?.unwrap_or(defaults.database_url),
            admin_password: env.var("ADMIN_PASSWORD")?,
            registration_disabled: env
                .bool("REGISTRATION_DISABLED", defaults.registration_disabled)?,
            trusted_proxies: env.ip_list("TRUSTED_PROXIES")?,
            books_dir: env.var("BOOKS_DIR")?.unwrap_or(defaults.books_dir),
            max_upload_bytes: env.parse("MAX_UPLOAD_BYTES", defaults.max_upload_bytes)?,
            metadata_enrichment: env.bool("METADATA_ENRICHMENT", defaults.metadata_enrichment)?,
            default_language: env
                .var("DEFAULT_LANGUAGE")?
                .map_or(defaults.default_language, |code| {
                    code.trim().to_ascii_lowercase()
                }),
            cookie_secure: env.bool("COOKIE_SECURE", https)?,
            public_url,
            template_dir: env.var("TEMPLATE_DIR")?.map(PathBuf::from),
            static_dir: env.var("STATIC_DIR")?.map(PathBuf::from),
        })
    }
}

/// Normalize the optional public base URL: empty means unset, and a value must
/// be an absolute `http` or `https` URL. A trailing slash is removed.
fn parse_public_url(value: &str) -> Result<Option<String>, ConfigError> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return Ok(None);
    }
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"));
    match rest {
        Some(host) if !host.is_empty() && !host.contains(char::is_whitespace) => {
            Ok(Some(value.to_owned()))
        }
        _ => Err(ConfigError(
            "KOSYNC_RS_PUBLIC_URL must be an absolute http(s) URL".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use web_mev_core::Env;

    use super::{Config, parse_public_url};

    fn config(pairs: &[(&str, &str)]) -> Result<Config, super::ConfigError> {
        Config::from_source(&Env::from_pairs("KOSYNC_RS_", pairs.iter().copied()))
    }

    #[test]
    fn blank_admin_password_is_unset() {
        assert_eq!(config(&[]).unwrap().admin_password, None);
        assert_eq!(
            config(&[("KOSYNC_RS_ADMIN_PASSWORD", "  ")])
                .unwrap()
                .admin_password,
            None
        );
        assert_eq!(
            config(&[("KOSYNC_RS_ADMIN_PASSWORD", " pass ")])
                .unwrap()
                .admin_password
                .as_deref(),
            Some(" pass ")
        );
    }

    #[test]
    fn public_url_is_optional_and_normalized() {
        assert_eq!(parse_public_url("").unwrap(), None);
        assert_eq!(
            parse_public_url("https://books.example.com/")
                .unwrap()
                .as_deref(),
            Some("https://books.example.com")
        );
        assert!(parse_public_url("books.example.com").is_err());
        assert!(parse_public_url("https://").is_err());
    }

    #[test]
    fn invalid_values_are_reported_by_name() {
        let error = config(&[("KOSYNC_RS_PORT", "not-a-port")]).unwrap_err();
        assert!(error.to_string().contains("KOSYNC_RS_PORT"), "{error}");
        let error = config(&[("KOSYNC_RS_TRUSTED_PROXIES", "1.2.3.4,nope")]).unwrap_err();
        assert!(
            error.to_string().contains("KOSYNC_RS_TRUSTED_PROXIES"),
            "{error}"
        );
    }

    #[test]
    fn valid_values_are_parsed() {
        let config = config(&[
            ("KOSYNC_RS_PORT", "8091"),
            ("KOSYNC_RS_METADATA_ENRICHMENT", "true"),
            ("KOSYNC_RS_TRUSTED_PROXIES", "127.0.0.1, ::1"),
        ])
        .unwrap();
        assert_eq!(config.port, 8091);
        assert!(config.metadata_enrichment);
        assert_eq!(config.trusted_proxies.len(), 2);
        assert!(config.registration_disabled);
    }
}
