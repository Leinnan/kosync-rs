//! Application configuration loaded from the environment.

use std::{env, fmt, net::IpAddr, str::FromStr};

/// Runtime configuration for the sync server.
///
/// All values are read from the environment, prefixed with `KOSYNC_RS_`, and
/// can be overridden with a `.env` file. Process environment variables take
/// precedence over values from `.env`, and a missing `.env` file is ignored.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub host: String,

    /// Port the HTTP server listens on.
    pub port: u16,

    /// `SQLite` connection string (e.g. `sqlite://data/kosync.db`).
    pub database_url: String,

    /// Password for the seeded `admin` user. MD5-hashed before storage.
    pub admin_password: String,

    /// When `true`, new user registration via `/users/create` is rejected.
    ///
    /// Enabled by default: accounts are created by an administrator. Set
    /// `KOSYNC_RS_REGISTRATION_DISABLED=false` to allow public sign-up.
    pub registration_disabled: bool,

    /// Comma-separated list of trusted reverse proxy addresses.
    pub trusted_proxies: String,

    /// When `true`, logging output is emitted on a single line.
    pub single_line_logging: bool,

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
}

/// A configuration error that never includes configured secret values.
#[derive(Debug)]
pub struct ConfigError(String);

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_owned(),
            port: 8090,
            database_url: "sqlite://data/kosync.db".to_owned(),
            admin_password: "admin".to_owned(),
            registration_disabled: true,
            trusted_proxies: String::new(),
            single_line_logging: false,
            books_dir: "data/books".to_owned(),
            max_upload_bytes: 209_715_200,
            metadata_enrichment: false,
        }
    }
}

impl Config {
    /// Load `.env` as a fallback, then read and parse the process environment.
    ///
    /// Process environment variables take precedence over values in `.env`. A
    /// missing `.env` file is ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if `.env` cannot be parsed or a setting has a malformed
    /// value. The error never includes configured secret values.
    pub fn from_env() -> Result<Self, ConfigError> {
        let dot_env = match dotenv::EnvLoader::new().load() {
            Ok(values) => values,
            Err(error) if error.not_found() => dotenv::EnvMap::new(),
            Err(_) => return Err(ConfigError("failed to load .env configuration".into())),
        };
        let value = |name: &str, default: &str| -> String {
            env::var(name)
                .ok()
                .or_else(|| dot_env.get(name).cloned())
                .unwrap_or_else(|| default.to_owned())
        };

        Ok(Self {
            host: value("KOSYNC_RS_HOST", "0.0.0.0"),
            port: parse("KOSYNC_RS_PORT", &value("KOSYNC_RS_PORT", "8090"))?,
            database_url: value("KOSYNC_RS_DATABASE_URL", "sqlite://data/kosync.db"),
            admin_password: value("KOSYNC_RS_ADMIN_PASSWORD", "admin"),
            registration_disabled: parse(
                "KOSYNC_RS_REGISTRATION_DISABLED",
                &value("KOSYNC_RS_REGISTRATION_DISABLED", "true"),
            )?,
            trusted_proxies: value("KOSYNC_RS_TRUSTED_PROXIES", ""),
            single_line_logging: parse(
                "KOSYNC_RS_SINGLE_LINE_LOGGING",
                &value("KOSYNC_RS_SINGLE_LINE_LOGGING", "false"),
            )?,
            books_dir: value("KOSYNC_RS_BOOKS_DIR", "data/books"),
            max_upload_bytes: parse(
                "KOSYNC_RS_MAX_UPLOAD_BYTES",
                &value("KOSYNC_RS_MAX_UPLOAD_BYTES", "209715200"),
            )?,
            metadata_enrichment: parse(
                "KOSYNC_RS_METADATA_ENRICHMENT",
                &value("KOSYNC_RS_METADATA_ENRICHMENT", "false"),
            )?,
        })
    }

    /// The parsed list of trusted proxy addresses, ignoring invalid entries.
    pub(crate) fn parse_trusted_proxies(&self) -> Vec<IpAddr> {
        self.trusted_proxies
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| IpAddr::from_str(s).ok())
            .collect()
    }
}

/// Parse a setting, naming only the variable in any error message.
fn parse<T: FromStr>(name: &str, value: &str) -> Result<T, ConfigError> {
    value
        .parse()
        .map_err(|_| ConfigError(format!("{name} has an invalid value")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::parse;

    #[test]
    fn parse_reports_invalid_values_by_name() {
        let error = parse::<u16>("KOSYNC_RS_PORT", "not-a-port").unwrap_err();
        assert_eq!(error.to_string(), "KOSYNC_RS_PORT has an invalid value");
    }

    #[test]
    fn parse_accepts_valid_values() {
        assert_eq!(parse::<u16>("KOSYNC_RS_PORT", "8090").unwrap(), 8090);
        assert!(parse::<bool>("KOSYNC_RS_METADATA_ENRICHMENT", "true").unwrap());
    }
}
