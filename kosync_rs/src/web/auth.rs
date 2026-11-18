//! Signing in and out of the web UI.
//!
//! Sessions live in the database through `web_mev_auth`: the cookie holds a
//! random token, every form carries the session's CSRF token, and repeated
//! failures lock a username for a while. Accounts created before web sessions,
//! or registered from `KOReader`, only have the MD5 `KOReader` sends; their first
//! web sign-in with the right password stores an Argon2id hash next to it.

use axum::{
    Form,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tower_cookies::Cookies;
use web_mev_auth::{AnonForm, AuthError, CSRF_HEADER, IssuedSession, OptionalSession, audit};

use crate::{db, state::AppState, store};

use super::{anon_context, current_user, render};

/// Login form fields.
#[derive(Debug, Deserialize)]
pub(super) struct LoginForm {
    username: String,
    password: String,
}

/// The CSRF token a form posts back.
#[derive(Debug, Deserialize)]
pub(super) struct CsrfOnly {
    #[serde(default)]
    csrf_token: String,
}

/// Why a sign-in did not start a session.
enum SignInError {
    /// Wrong username or password, or an inactive account.
    Invalid,
    /// Too many recent failures for this username.
    Locked,
    /// Storage or hashing failed; already logged.
    Internal,
}

impl From<AuthError> for SignInError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::InvalidCredentials | AuthError::CredentialsChanged => Self::Invalid,
            AuthError::Locked => Self::Locked,
            other => {
                tracing::error!(error = %other, "sign-in failed");
                Self::Internal
            }
        }
    }
}

impl From<store::StoreError> for SignInError {
    fn from(error: store::StoreError) -> Self {
        error.log();
        Self::Internal
    }
}

/// Render the login form, or redirect to the dashboard if already authenticated.
pub(super) async fn login_form(State(state): State<AppState>, cookies: Cookies) -> Response {
    if current_user(&state, &cookies).await.is_some() {
        return Redirect::to("/").into_response();
    }
    login_page(&state, &cookies, None, "")
}

/// The login page, with a fresh anonymous CSRF token and the translated
/// `error`, a translation key.
fn login_page(
    state: &AppState,
    cookies: &Cookies,
    error: Option<&str>,
    username: &str,
) -> Response {
    let locale = state.i18n.resolve(None);
    let mut ctx = anon_context(state, &locale);
    ctx.insert("csrf_token", &anon_csrf_token(state, cookies));
    if let Some(key) = error {
        ctx.insert("error", &state.i18n.tr(&locale).t(key));
        ctx.insert("form_username", username);
    }
    render(state, "login.html", &ctx)
}

/// The anonymous CSRF token for the login form: the browser's current one when
/// it holds a well-formed token, otherwise a fresh one.
///
/// `issue_anon_csrf` replaces the cookie on every call, so issuing a token per
/// page view would invalidate a login form already on screen whenever the
/// browser loads `/login` again: a second tab, a reload, or the address bar
/// prerendering the page.
fn anon_csrf_token(state: &AppState, cookies: &Cookies) -> String {
    cookies
        .get(&state.sessions.config().anon_csrf_cookie)
        .map(|cookie| cookie.value().to_owned())
        .filter(|token| is_csrf_token(token))
        .unwrap_or_else(|| state.sessions.issue_anon_csrf(cookies))
}

/// Whether `value` has the shape of an issued CSRF token: 24 random bytes as
/// unpadded URL-safe Base64.
fn is_csrf_token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Handle a login submission.
pub(super) async fn login_submit(
    State(state): State<AppState>,
    AnonForm { form, cookies }: AnonForm<LoginForm>,
) -> Response {
    let username = form.username.trim().to_owned();
    match sign_in(&state, &username, form.password).await {
        Ok(issued) => {
            issued.set_cookie(&cookies);
            Redirect::to("/").into_response()
        }
        Err(SignInError::Invalid) => {
            login_page(&state, &cookies, Some("login_error_invalid"), &username)
        }
        Err(SignInError::Locked) => {
            login_page(&state, &cookies, Some("login_error_locked"), &username)
        }
        Err(SignInError::Internal) => {
            login_page(&state, &cookies, Some("error_generic"), &username)
        }
    }
}

/// Check the password and open a session, upgrading an MD5-only account first.
async fn sign_in(
    state: &AppState,
    username: &str,
    password: String,
) -> Result<IssuedSession, SignInError> {
    add_missing_web_password(state, username, &password).await?;
    let sessions = &state.sessions;
    let found = sessions
        .authenticate::<store::User>(username, password)
        .await?;
    let mut tx = sessions
        .begin_verified::<store::User>(found.user_id, &found.password_hash)
        .await?;
    let issued = sessions.insert_session(&mut tx, found.user_id).await?;
    audit::record(
        &mut *tx,
        Some(found.user_id),
        "user.login",
        None,
        None,
        &serde_json::json!({}),
    )
    .await
    .map_err(AuthError::from)?;
    tx.commit().await.map_err(AuthError::from)?;
    Ok(issued)
}

/// Give an active account that only has the `KOReader` MD5 its first web
/// password hash, when `password` matches that MD5. Anything else is left for
/// [`web_mev_auth::SessionStore::authenticate`] to refuse and count.
async fn add_missing_web_password(
    state: &AppState,
    username: &str,
    password: &str,
) -> Result<(), SignInError> {
    if state.sessions.login_locked(username).await? {
        return Ok(());
    }
    let Some(user) = store::get_user(&state.pool, username).await? else {
        return Ok(());
    };
    if !user.is_active
        || store::has_web_password(&state.pool, user.id).await?
        || !web_mev_core::secure_eq(&user.password_hash, &db::md5_hex(password))
    {
        return Ok(());
    }
    let hash = state
        .sessions
        .hasher()
        .hash(password.to_owned())
        .await
        .map_err(AuthError::from)?;
    if store::set_missing_web_password(&state.pool, user.id, &hash).await? {
        tracing::info!(user = %user.username, "added a web password hash on sign-in");
    }
    Ok(())
}

/// End the browser session and return to the login page. A request without a
/// session, or without its CSRF token, just goes to the login page.
pub(super) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    OptionalSession { session, cookies }: OptionalSession<store::User>,
    Form(form): Form<CsrfOnly>,
) -> Response {
    let supplied = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or(&form.csrf_token);
    if let Some(session) = session
        && session.check_csrf(supplied)
        && let Err(error) = state.sessions.destroy(&cookies).await
    {
        tracing::error!(%error, "could not end the session");
    }
    Redirect::to("/login").into_response()
}
