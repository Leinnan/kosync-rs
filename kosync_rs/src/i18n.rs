//! Translations of the web UI, OPDS feed titles, and import messages.
//!
//! Every string lives in `locales/i18n.csv`: the first column is the key and
//! each later column a language (`en` first, which is also the fallback). Values
//! use `{name}` slots, and counted strings have one row per plural form
//! (`{key}_one`, `_few`, `_many`, `_other`). Keys whose value holds trusted
//! markup end in `_html`; their slot values are HTML-escaped.
//!
//! A request's language is negotiated once by [`negotiate_locale`]: the
//! language cookie, then `Accept-Language`, then `KOSYNC_RS_DEFAULT_LANGUAGE`.
//! A signed-in user's saved choice overrides that ([`I18n::resolve`]).

use std::{fmt, sync::Arc};

use axum::{
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::Response,
};
use tera::{Kwargs, Tera, TeraResult};
use web_mev_i18n::{Lang, LocaleCode, Localize as _, TranslationError, TranslationsAsset};

use crate::state::AppState;

/// The translation table, compiled into the binary.
const TABLE: &str = include_str!("../locales/i18n.csv");

/// The cookie that remembers a chosen language.
pub(crate) const LOCALE_COOKIE: &str = "kosync_locale";

tokio::task_local! {
    /// The negotiated language of the request being handled.
    static REQUEST_LOCALE: LocaleCode;
}

/// A translation table that cannot be used.
#[derive(Debug)]
pub enum I18nError {
    /// The table does not parse.
    Table(TranslationError),
    /// `KOSYNC_RS_DEFAULT_LANGUAGE` names a language the table lacks.
    UnknownDefault(String),
}

impl fmt::Display for I18nError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Table(error) => write!(f, "the translation table is invalid: {error}"),
            Self::UnknownDefault(code) => {
                write!(
                    f,
                    "KOSYNC_RS_DEFAULT_LANGUAGE {code:?} is not a supported language"
                )
            }
        }
    }
}

impl std::error::Error for I18nError {}

/// The parsed translation table and the server's default language.
#[derive(Debug)]
pub(crate) struct I18n {
    table: TranslationsAsset,
    supported: Vec<LocaleCode>,
    default: LocaleCode,
}

impl I18n {
    /// Parse the embedded table, with `default` as the language for requests
    /// that ask for none of the supported ones.
    ///
    /// # Errors
    ///
    /// Returns an error when the table is malformed or lacks `default`.
    pub(crate) fn load(default: &str) -> Result<Self, I18nError> {
        let table = TranslationsAsset::from_csv_str(TABLE).map_err(I18nError::Table)?;
        let supported: Vec<LocaleCode> = table.locales().cloned().collect();
        let default = LocaleCode::try_from(default)
            .ok()
            .filter(|code| supported.contains(code))
            .ok_or_else(|| I18nError::UnknownDefault(default.to_owned()))?;
        Ok(Self {
            table,
            supported,
            default,
        })
    }

    /// The languages in the table, in column order.
    pub(crate) fn supported(&self) -> &[LocaleCode] {
        &self.supported
    }

    /// A supported language from its code, such as `pl`.
    pub(crate) fn parse(&self, code: &str) -> Option<LocaleCode> {
        LocaleCode::try_from(code)
            .ok()
            .filter(|locale| self.supported.contains(locale))
    }

    /// The language for a request from its language cookie and
    /// `Accept-Language` header, falling back to the server default.
    pub(crate) fn negotiate(
        &self,
        cookie: Option<&str>,
        accept_language: Option<&str>,
    ) -> LocaleCode {
        web_mev_i18n::negotiate(cookie, accept_language, &self.supported, &self.default)
    }

    /// The language for a page: the user's saved choice when there is one,
    /// otherwise the request's negotiated language.
    pub(crate) fn resolve(&self, user_choice: Option<&str>) -> LocaleCode {
        user_choice
            .and_then(|code| self.parse(code))
            .or_else(request_locale)
            .unwrap_or_else(|| self.default.clone())
    }

    /// Lookups in `locale`.
    pub(crate) fn tr(&self, locale: &LocaleCode) -> Tr<'_> {
        Tr {
            lang: Lang {
                catalog: &self.table,
                locale: locale.clone(),
            },
        }
    }

    /// Every key in the table.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.table.keys()
    }

    /// The raw value of `key` in `locale`, without any fallback.
    #[cfg(test)]
    pub(crate) fn raw(&self, key: &str, locale: &LocaleCode) -> Option<&str> {
        self.table.tr(key, locale)
    }
}

/// Lookups in one language. A missing key shows as itself and is logged once.
pub(crate) struct Tr<'a> {
    lang: Lang<'a>,
}

impl Tr<'_> {
    /// The text of `key`.
    pub(crate) fn t(&self, key: &str) -> String {
        self.lang.t(key)
    }

    /// The text of `key` with its `{name}` slots filled from `args`.
    pub(crate) fn args(&self, key: &str, args: &[(&str, &str)]) -> String {
        self.lang.t_args(key, args)
    }

    /// The plural form of `key` for `n`, with `{n}` and `args` filled in.
    pub(crate) fn n(&self, key: &str, n: i64, args: &[(&str, &str)]) -> String {
        self.lang.t_n(key, n.unsigned_abs(), args)
    }

    /// Format a decimal number with this language's decimal separator.
    pub(crate) fn decimal(&self, formatted: &str) -> String {
        if self.t("number_decimal") == "," {
            formatted.replace('.', ",")
        } else {
            formatted.to_owned()
        }
    }
}

/// The negotiated language of the request being handled, if any.
pub(crate) fn request_locale() -> Option<LocaleCode> {
    REQUEST_LOCALE.try_with(Clone::clone).ok()
}

/// Middleware that negotiates the request's language from the language cookie
/// and `Accept-Language`, for handlers and templates to read through
/// [`I18n::resolve`] and [`request_locale`].
pub(crate) async fn negotiate_locale(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == LOCALE_COOKIE)
        .map(|(_, value)| value.to_owned());
    let accept = headers
        .get(header::ACCEPT_LANGUAGE)
        .and_then(|value| value.to_str().ok());
    let locale = state.i18n.negotiate(cookie.as_deref(), accept);
    REQUEST_LOCALE.scope(locale, next.run(request)).await
}

/// The language a template renders in: its `locale` variable, else the
/// request's negotiated language, else the default.
pub(crate) fn template_locale(i18n: &I18n, state: &tera::State) -> LocaleCode {
    state
        .get::<String>("locale")
        .ok()
        .flatten()
        .and_then(|code| i18n.parse(&code))
        .unwrap_or_else(|| i18n.resolve(None))
}

/// Register `t(key="…")` for templates. `n=` picks a plural form, and any
/// other argument fills a `{name}` slot. For keys ending in `_html` the slot
/// values are HTML-escaped, so the result can be printed with `| safe`.
pub(crate) fn register(tera: &mut Tera, i18n: Arc<I18n>) {
    tera.register_function(
        "t",
        move |kwargs: Kwargs, state: &tera::State| -> TeraResult<String> {
            let key: &str = kwargs.must_get("key")?;
            let escape = key.ends_with("_html");
            let filled: Vec<(String, String)> = kwargs
                .iter()
                .filter_map(|(name, value)| {
                    let name = name.as_str()?;
                    if name == "key" || name == "n" {
                        return None;
                    }
                    let text = value.to_string();
                    let text = if escape {
                        web_mev_axum::escape_html(&text)
                    } else {
                        text
                    };
                    Some((name.to_owned(), text))
                })
                .collect();
            let args: Vec<(&str, &str)> = filled
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let tr = i18n.tr(&template_locale(&i18n, state));
            Ok(match kwargs.get::<i64>("n")? {
                Some(n) => tr.n(key, n, &args),
                None => tr.args(key, &args),
            })
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{I18n, LocaleCode};

    fn i18n() -> I18n {
        I18n::load("en").unwrap()
    }

    #[test]
    fn the_table_has_english_and_polish() {
        let codes: Vec<String> = i18n().supported().iter().map(LocaleCode::code).collect();
        assert_eq!(codes, ["en", "pl"]);
    }

    #[test]
    fn unknown_default_language_is_refused() {
        assert!(I18n::load("de").is_err());
        assert!(I18n::load("pl").is_ok());
    }

    #[test]
    fn every_key_has_both_languages() {
        let i18n = i18n();
        let en = LocaleCode::try_from("en").unwrap();
        let pl = LocaleCode::try_from("pl").unwrap();
        let mut missing = Vec::new();
        for key in i18n.keys() {
            let plural = ["_one", "_few", "_many", "_other"]
                .iter()
                .find(|suffix| key.ends_with(*suffix));
            let needs_en = !matches!(plural, Some(&("_few" | "_many")));
            let needs_pl = !matches!(plural, Some(&"_other"));
            if needs_en && i18n.raw(key, &en).is_none() {
                missing.push(format!("{key} (en)"));
            }
            if needs_pl && i18n.raw(key, &pl).is_none() {
                missing.push(format!("{key} (pl)"));
            }
        }
        assert!(missing.is_empty(), "missing values: {missing:?}");
    }

    #[test]
    fn plural_keys_have_every_form() {
        let i18n = i18n();
        let keys: Vec<&str> = i18n.keys().collect();
        for key in &keys {
            if let Some(base) = key.strip_suffix("_one") {
                for suffix in ["_few", "_many", "_other"] {
                    assert!(
                        keys.contains(&format!("{base}{suffix}").as_str()),
                        "{base}{suffix} is missing"
                    );
                }
            }
        }
    }

    /// Every `"prefix_…"` string literal in `source` whose prefix is one the
    /// table uses, as a candidate translation key.
    fn key_literals<'a>(source: &'a str, prefixes: &[&str]) -> Vec<&'a str> {
        source
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|literal| {
                literal
                    .split_once('_')
                    .is_some_and(|(prefix, rest)| prefixes.contains(&prefix) && !rest.is_empty())
                    && literal.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-'
                    })
            })
            .collect()
    }

    /// Read every file under `dir` with the extension `ext`.
    fn sources(dir: &std::path::Path, ext: &str, found: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                sources(&path, ext, found);
            } else if path.extension().is_some_and(|e| e == ext) {
                found.push((
                    path.display().to_string(),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }

    #[test]
    fn every_key_the_code_uses_is_in_the_table() {
        let i18n = i18n();
        let keys: std::collections::HashSet<&str> = i18n.keys().collect();
        let known = |key: &str| keys.contains(key) || keys.contains(format!("{key}_one").as_str());
        let mut prefixes: Vec<&str> = keys
            .iter()
            .filter_map(|key| key.split_once('_').map(|(prefix, _)| prefix))
            .collect();
        prefixes.sort_unstable();
        prefixes.dedup();

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        sources(&root, "html", &mut files);
        sources(&root, "rs", &mut files);
        let mut missing = Vec::new();
        let mut used = 0;
        for (path, source) in &files {
            let is_template = std::path::Path::new(path)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("html"));
            if path.ends_with("i18n.rs") {
                continue;
            }
            // Templates name keys as `t(key="…")`; Rust code as string literals.
            let candidates: Vec<&str> = if is_template {
                source
                    .split("key=\"")
                    .skip(1)
                    .filter_map(|rest| rest.split('"').next())
                    .collect()
            } else {
                key_literals(source, &prefixes)
            };
            for key in candidates {
                // Template variables and settings that share a key prefix.
                const NOT_KEYS: &[&str] = &[
                    "books_dir",
                    "language_options",
                    "language_choice",
                    "js_strings",
                ];
                if NOT_KEYS.contains(&key) {
                    continue;
                }
                used += 1;
                if !known(key) {
                    missing.push(format!("{key} ({path})"));
                }
            }
        }
        assert!(used > 300, "only {used} key uses found");
        assert!(
            missing.is_empty(),
            "keys missing from the table: {missing:#?}"
        );
    }

    #[test]
    fn polish_plurals_pick_the_right_form() {
        let i18n = i18n();
        let pl = i18n.tr(&LocaleCode::try_from("pl").unwrap());
        let en = i18n.tr(&LocaleCode::try_from("en").unwrap());
        let forms: Vec<String> = [1, 2, 5, 12, 22, 25]
            .iter()
            .map(|n| pl.n("count_books", *n, &[]))
            .collect();
        assert_eq!(
            forms,
            [
                "1 książka",
                "2 książki",
                "5 książek",
                "12 książek",
                "22 książki",
                "25 książek"
            ]
        );
        assert_eq!(en.n("count_books", 1, &[]), "1 book");
        assert_eq!(en.n("count_books", 2, &[]), "2 books");
    }

    #[test]
    fn negotiation_prefers_the_cookie_then_accept_language() {
        let i18n = i18n();
        assert_eq!(
            i18n.negotiate(None, Some("pl-PL,pl;q=0.9,en;q=0.5")).code(),
            "pl"
        );
        assert_eq!(i18n.negotiate(Some("en"), Some("pl")).code(), "en");
        assert_eq!(i18n.negotiate(Some("xx"), Some("de")).code(), "en");
        assert_eq!(i18n.negotiate(None, None).code(), "en");
    }
}
