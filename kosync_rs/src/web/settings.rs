//! Self-service password management pages for the web frontend.
//!
//! Every signed-in user can change their own password by confirming the
//! current one. Administrators additionally get a form to reset the password
//! of any other account. The built-in `admin` account is protected from
//! modification, matching the rest of the administration UI.

#![allow(
    clippy::result_large_err,
    reason = "Handlers return complete HTTP responses for authorization failures; boxing them adds allocation without a practical benefit."
)]

use axum::{
    extract::State,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tower_cookies::Cookies;
use web_mev_auth::{CsrfForm, UiSession};

use crate::{db, state::AppState, store, util::is_valid_field};

use super::{Viewer, base_context, current_user, flash_next, render, take_flash};

#[derive(Debug, Deserialize)]
pub(super) struct OwnPasswordForm {
    /// The user's existing password, confirmed before any change.
    current_password: String,
    /// New plaintext password; hashed before storage.
    new_password: String,
}

/// Fields submitted by the administrator password-reset form.
#[derive(Debug, Deserialize)]
pub(super) struct UserPasswordForm {
    /// Username of the account whose password is being reset.
    username: String,
    /// New plaintext password; hashed before storage.
    password: String,
}

/// Enforce an authenticated session.
async fn require_user(state: &AppState, cookies: &Cookies) -> Result<Viewer, Response> {
    current_user(state, cookies)
        .await
        .ok_or_else(|| Redirect::to("/login").into_response())
}

/// Return to the settings page, showing the message for `outcome`: a notice
/// code on success, an error code otherwise.
fn back(
    state: &AppState,
    viewer: &Viewer,
    cookies: &Cookies,
    outcome: Result<&str, &str>,
) -> Response {
    let (error, code) = match outcome {
        Ok(code) => (false, code),
        Err(code) => (true, code),
    };
    flash_next(state, cookies, error, &viewer.tr(state).t(flash_key(code)));
    Redirect::to("/settings").into_response()
}

/// The translation key of the message for a flash code.
fn flash_key(code: &str) -> &'static str {
    match code {
        "wrong-current" => "flash_settings_wrong-current",
        "invalid" => "flash_settings_invalid",
        "protected" => "flash_protected",
        "missing" => "flash_missing",
        "password-changed" => "flash_password-changed",
        "password-reset" => "flash_password-reset",
        "language-saved" => "flash_language-saved",
        _ => "error_generic",
    }
}

/// Render the settings page.
pub(super) async fn form(State(state): State<AppState>, cookies: Cookies) -> Response {
    let user = match require_user(&state, &cookies).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let mut ctx = base_context(&state, &user, "settings");
    if user.is_administrator {
        match store::list_users(&state.pool).await {
            Ok(users) => {
                let targets: Vec<String> = users
                    .into_iter()
                    .filter(|u| u.username != "admin")
                    .map(|u| u.username)
                    .collect();
                ctx.insert("targets", &targets);
            }
            Err(err) => {
                err.log();
                ctx.insert("error", &user.tr(&state).t("error_generic"));
            }
        }
    }
    ctx.insert(
        "language_choice",
        &user.user.locale.clone().unwrap_or_default(),
    );
    let all_languages: Vec<super::LanguageChoice> = state
        .i18n
        .supported()
        .iter()
        .map(|locale| super::LanguageChoice {
            code: locale.code(),
            name: state.i18n.tr(locale).t("language_name"),
        })
        .collect();
    ctx.insert("all_languages", &all_languages);
    take_flash(&state, &cookies, &mut ctx);
    render(&state, "settings.html", &ctx)
}

/// Change the signed-in user's own password. Other browser sessions of the
/// user are signed out; this one stays.
pub(super) async fn change_own(
    State(state): State<AppState>,
    CsrfForm { ui, form }: CsrfForm<store::User, OwnPasswordForm>,
) -> Response {
    let UiSession { session, cookies } = ui;
    let user = Viewer::new(&state, session);

    if !is_valid_field(&form.new_password) {
        return back(&state, &user, &cookies, Err("invalid"));
    }
    if !web_mev_core::secure_eq(&db::md5_hex(&form.current_password), &user.password_hash) {
        return back(&state, &user, &cookies, Err("wrong-current"));
    }

    let hash = db::md5_hex(&form.new_password);
    let Ok(web_hash) = state.hash_web_password(&form.new_password).await else {
        return back(&state, &user, &cookies, Err("db"));
    };
    let outcome = match store::set_password(
        &state.pool,
        &user.username,
        &hash,
        &web_hash,
        Some(user.session_id),
    )
    .await
    {
        Ok(true) => Ok("password-changed"),
        Ok(false) => Err("missing"),
        Err(err) => {
            err.log();
            Err("db")
        }
    };
    back(&state, &user, &cookies, outcome)
}

/// Reset another user's password (administrator only). That user is signed
/// out of the web UI everywhere.
pub(super) async fn change_user(
    State(state): State<AppState>,
    CsrfForm { ui, form }: CsrfForm<store::User, UserPasswordForm>,
) -> Response {
    let UiSession { session, cookies } = ui;
    if !session.user.is_administrator {
        return Redirect::to("/").into_response();
    }
    let user = Viewer::new(&state, session);
    if form.username == "admin" {
        return back(&state, &user, &cookies, Err("protected"));
    }
    if !is_valid_field(&form.password) {
        return back(&state, &user, &cookies, Err("invalid"));
    }

    let hash = db::md5_hex(&form.password);
    let Ok(web_hash) = state.hash_web_password(&form.password).await else {
        return back(&state, &user, &cookies, Err("db"));
    };
    let outcome =
        match store::set_password(&state.pool, &form.username, &hash, &web_hash, None).await {
            Ok(true) => Ok("password-reset"),
            Ok(false) => Err("missing"),
            Err(err) => {
                err.log();
                Err("db")
            }
        };
    back(&state, &user, &cookies, outcome)
}

/// Field submitted by the language form.
#[derive(Debug, Deserialize)]
pub(super) struct LanguageForm {
    /// A supported language code, or empty for automatic.
    #[serde(default)]
    language: String,
}

/// Save the signed-in user's language, or clear it for automatic. A chosen
/// language is also remembered by this browser, for pages before sign-in.
pub(super) async fn change_language(
    State(state): State<AppState>,
    CsrfForm { ui, form }: CsrfForm<store::User, LanguageForm>,
) -> Response {
    let UiSession { session, cookies } = ui;
    let user_id = session.user.id;
    let choice = state.i18n.parse(form.language.trim());
    let code = choice.as_ref().map(web_mev_i18n::LocaleCode::code);
    let saved = store::set_locale(&state.pool, user_id, code.as_deref()).await;
    if let Some(locale) = &choice {
        super::remember_locale(&state, &cookies, locale);
    }
    // The message is shown in the language just picked.
    let mut user = Viewer::new(&state, session);
    user.locale = state.i18n.resolve(code.as_deref());
    match saved {
        Ok(()) => back(&state, &user, &cookies, Ok("language-saved")),
        Err(err) => {
            err.log();
            back(&state, &user, &cookies, Err("db"))
        }
    }
}
