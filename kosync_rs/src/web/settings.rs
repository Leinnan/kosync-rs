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
    Form,
    extract::{Query, State},
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tower_cookies::Cookies;

use crate::{db, state::AppState, store, util::is_valid_field};

use super::{base_context, current_user, render};

/// Query parameters carrying a flash message code between requests.
#[derive(Debug, Default, Deserialize)]
pub(super) struct FlashQuery {
    /// Error message code, if any.
    #[serde(default)]
    error: Option<String>,
    /// Success message code, if any.
    #[serde(default)]
    notice: Option<String>,
}

/// Fields submitted by the change-your-own-password form.
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
async fn require_user(state: &AppState, cookies: &Cookies) -> Result<store::User, Response> {
    current_user(state, cookies)
        .await
        .ok_or_else(|| Redirect::to("/login").into_response())
}

/// Enforce an authenticated administrator session.
async fn require_admin(state: &AppState, cookies: &Cookies) -> Result<store::User, Response> {
    let Some(user) = current_user(state, cookies).await else {
        return Err(Redirect::to("/login").into_response());
    };
    if user.is_administrator {
        Ok(user)
    } else {
        Err(Redirect::to("/").into_response())
    }
}

/// Redirect back to the settings page with an error flash code.
fn redirect_error(code: &str) -> Response {
    Redirect::to(&format!("/settings?error={code}")).into_response()
}

/// Redirect back to the settings page with a success flash code.
fn redirect_notice(code: &str) -> Response {
    Redirect::to(&format!("/settings?notice={code}")).into_response()
}

/// Map flash codes from the query string to a rendered message.
fn flash_message(query: &FlashQuery) -> Option<(&'static str, &'static str)> {
    if let Some(code) = &query.error {
        let text = match code.as_str() {
            "wrong-current" => "Your current password is incorrect.",
            "invalid" => "Enter a non-empty password.",
            "protected" => "The built-in admin account cannot be modified.",
            "missing" => "That user no longer exists.",
            "db" => "Something went wrong. Please try again.",
            _ => return None,
        };
        return Some(("error", text));
    }

    if let Some(code) = &query.notice {
        let text = match code.as_str() {
            "password-changed" => "Password changed.",
            "password-reset" => "Password reset.",
            _ => return None,
        };
        return Some(("notice", text));
    }

    None
}

/// Render the settings page.
pub(super) async fn form(
    State(state): State<AppState>,
    Query(query): Query<FlashQuery>,
    cookies: Cookies,
) -> Response {
    let user = match require_user(&state, &cookies).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let mut ctx = base_context(&user, "settings");
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
                ctx.insert("error", "Something went wrong. Please try again.");
            }
        }
    }
    if let Some((kind, text)) = flash_message(&query) {
        ctx.insert(kind, text);
    }
    render(&state, "settings.html", &ctx)
}

/// Change the signed-in user's own password.
pub(super) async fn change_own(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<OwnPasswordForm>,
) -> Response {
    let user = match require_user(&state, &cookies).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    if !is_valid_field(&form.new_password) {
        return redirect_error("invalid");
    }
    if db::md5_hex(&form.current_password) != user.password_hash {
        return redirect_error("wrong-current");
    }

    let hash = db::md5_hex(&form.new_password);
    match store::set_password(&state.pool, &user.username, &hash).await {
        Ok(true) => redirect_notice("password-changed"),
        Ok(false) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}

/// Reset another user's password (administrator only).
pub(super) async fn change_user(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<UserPasswordForm>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }
    if form.username == "admin" {
        return redirect_error("protected");
    }
    if !is_valid_field(&form.password) {
        return redirect_error("invalid");
    }

    let hash = db::md5_hex(&form.password);
    match store::set_password(&state.pool, &form.username, &hash).await {
        Ok(true) => redirect_notice("password-reset"),
        Ok(false) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}
