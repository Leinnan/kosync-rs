//! Administrator user-management pages for the web frontend.
//!
//! Provides an account-creation form plus per-user controls: activate and
//! deactivate, grant or revoke upload permission, reset the password, and
//! delete. Every handler requires an authenticated administrator session.

#![allow(
    clippy::result_large_err,
    reason = "Handlers return complete HTTP responses for authorization failures; boxing them adds allocation without a practical benefit."
)]

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;
use web_mev_auth::{CsrfForm, EmptyForm, UiSession};

use crate::{
    db,
    models::UserSummary,
    state::AppState,
    store::{self, StoreError},
    util::{is_valid_field, is_valid_key_field},
};

use super::{Viewer, base_context, current_user, flash_next, format_timestamp, render, take_flash};

/// A user row prepared for rendering, with the last sync as RFC 3339.
#[derive(Debug, Serialize)]
struct UserRow {
    #[serde(flatten)]
    user: UserSummary,
    last_synced_at: String,
}

/// Fields submitted by the account-creation form.
#[derive(Debug, Deserialize)]
pub(super) struct CreateForm {
    /// Username for the new account.
    username: String,
    /// Plaintext password; hashed before storage.
    password: String,
    /// Present (any value) when the upload-permission checkbox is ticked.
    #[serde(default)]
    can_upload: Option<String>,
}

/// Field submitted by the password-reset form.
#[derive(Debug, Deserialize)]
pub(super) struct PasswordForm {
    /// New plaintext password; hashed before storage.
    password: String,
}

/// Enforce an authenticated administrator session for a page.
async fn require_admin(state: &AppState, cookies: &Cookies) -> Result<Viewer, Response> {
    let Some(user) = current_user(state, cookies).await else {
        return Err(Redirect::to("/login").into_response());
    };
    if user.is_administrator {
        Ok(user)
    } else {
        Err(Redirect::to("/").into_response())
    }
}

/// The administrator behind a CSRF-checked form, or a redirect for anyone else.
fn admin_form(state: &AppState, ui: UiSession<store::User>) -> Result<(Viewer, Cookies), Response> {
    if ui.session.user.is_administrator {
        Ok((Viewer::new(state, ui.session), ui.cookies))
    } else {
        Err(Redirect::to("/").into_response())
    }
}

/// Return to the users page, showing the message for `outcome`: a notice code
/// on success, an error code otherwise.
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
    Redirect::to("/admin/users").into_response()
}

/// The translation key of the message for a flash code.
fn flash_key(code: &str) -> &'static str {
    match code {
        "invalid" => "flash_admin_invalid",
        "exists" => "flash_exists",
        "missing" => "flash_missing",
        "protected" => "flash_protected",
        "created" => "flash_created",
        "activated" => "flash_activated",
        "deactivated" => "flash_deactivated",
        "upload-granted" => "flash_upload-granted",
        "upload-revoked" => "flash_upload-revoked",
        "password-reset" => "flash_password-reset",
        "deleted" => "flash_deleted",
        _ => "error_generic",
    }
}

/// Render the user-management page (administrator only).
pub(super) async fn list(State(state): State<AppState>, cookies: Cookies) -> Response {
    let user = match require_admin(&state, &cookies).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let users = match store::list_users(&state.pool).await {
        Ok(users) => users,
        Err(err) => {
            err.log();
            return render_error(&state, &user);
        }
    };

    let rows: Vec<UserRow> = users
        .into_iter()
        .map(|summary| UserRow {
            last_synced_at: summary
                .last_synced
                .map_or_else(String::new, format_timestamp),
            user: summary,
        })
        .collect();

    let mut ctx = base_context(&state, &user, "users");
    ctx.insert("users", &rows);
    take_flash(&state, &cookies, &mut ctx);
    render(&state, "users.html", &ctx)
}

/// Render the users page with a generic database-error banner.
fn render_error(state: &AppState, user: &Viewer) -> Response {
    let mut ctx = base_context(state, user, "users");
    ctx.insert("error", &user.tr(state).t("error_generic"));
    render(state, "users.html", &ctx)
}

/// Create a new account (administrator only).
pub(super) async fn create(
    State(state): State<AppState>,
    CsrfForm { ui, form }: CsrfForm<store::User, CreateForm>,
) -> Response {
    let (viewer, cookies) = match admin_form(&state, ui) {
        Ok(found) => found,
        Err(response) => return response,
    };

    let username = form.username.trim();
    if !is_valid_key_field(username) || !is_valid_field(&form.password) {
        return back(&state, &viewer, &cookies, Err("invalid"));
    }

    let hash = db::md5_hex(&form.password);
    let Ok(web_hash) = state.hash_web_password(&form.password).await else {
        return back(&state, &viewer, &cookies, Err("db"));
    };
    let can_upload = form.can_upload.is_some();

    let outcome =
        match store::create_user(&state.pool, username, &hash, Some(&web_hash), can_upload).await {
            Ok(()) => Ok("created"),
            Err(StoreError::Exists) => Err("exists"),
            Err(err) => {
                err.log();
                Err("db")
            }
        };
    back(&state, &viewer, &cookies, outcome)
}

/// Toggle a user's active flag (administrator only). Deactivating signs the
/// user out of the web UI.
pub(super) async fn toggle_active(
    State(state): State<AppState>,
    Path(username): Path<String>,
    CsrfForm { ui, .. }: CsrfForm<store::User, EmptyForm>,
) -> Response {
    let (viewer, cookies) = match admin_form(&state, ui) {
        Ok(found) => found,
        Err(response) => return response,
    };
    if username == "admin" {
        return back(&state, &viewer, &cookies, Err("protected"));
    }

    let outcome = match store::toggle_active(&state.pool, &username).await {
        Ok(Some(true)) => Ok("activated"),
        Ok(Some(false)) => Ok("deactivated"),
        Ok(None) => Err("missing"),
        Err(err) => {
            err.log();
            Err("db")
        }
    };
    back(&state, &viewer, &cookies, outcome)
}

/// Grant or revoke a user's upload permission (administrator only).
pub(super) async fn toggle_can_upload(
    State(state): State<AppState>,
    Path(username): Path<String>,
    CsrfForm { ui, .. }: CsrfForm<store::User, EmptyForm>,
) -> Response {
    let (viewer, cookies) = match admin_form(&state, ui) {
        Ok(found) => found,
        Err(response) => return response,
    };
    if username == "admin" {
        return back(&state, &viewer, &cookies, Err("protected"));
    }

    let outcome = match store::toggle_can_upload(&state.pool, &username).await {
        Ok(Some(true)) => Ok("upload-granted"),
        Ok(Some(false)) => Ok("upload-revoked"),
        Ok(None) => Err("missing"),
        Err(err) => {
            err.log();
            Err("db")
        }
    };
    back(&state, &viewer, &cookies, outcome)
}

/// Reset a user's password (administrator only). The user is signed out of the
/// web UI everywhere.
pub(super) async fn reset_password(
    State(state): State<AppState>,
    Path(username): Path<String>,
    CsrfForm { ui, form }: CsrfForm<store::User, PasswordForm>,
) -> Response {
    let (viewer, cookies) = match admin_form(&state, ui) {
        Ok(found) => found,
        Err(response) => return response,
    };
    if username == "admin" {
        return back(&state, &viewer, &cookies, Err("protected"));
    }
    if !is_valid_field(&form.password) {
        return back(&state, &viewer, &cookies, Err("invalid"));
    }

    let hash = db::md5_hex(&form.password);
    let Ok(web_hash) = state.hash_web_password(&form.password).await else {
        return back(&state, &viewer, &cookies, Err("db"));
    };
    let outcome = match store::set_password(&state.pool, &username, &hash, &web_hash, None).await {
        Ok(true) => Ok("password-reset"),
        Ok(false) => Err("missing"),
        Err(err) => {
            err.log();
            Err("db")
        }
    };
    back(&state, &viewer, &cookies, outcome)
}

/// Delete a user (administrator only).
pub(super) async fn delete(
    State(state): State<AppState>,
    Path(username): Path<String>,
    CsrfForm { ui, .. }: CsrfForm<store::User, EmptyForm>,
) -> Response {
    let (viewer, cookies) = match admin_form(&state, ui) {
        Ok(found) => found,
        Err(response) => return response,
    };
    if username == "admin" {
        return back(&state, &viewer, &cookies, Err("protected"));
    }

    let outcome = match store::delete_user(&state.pool, &username).await {
        Ok(true) => Ok("deleted"),
        Ok(false) => Err("missing"),
        Err(err) => {
            err.log();
            Err("db")
        }
    };
    back(&state, &viewer, &cookies, outcome)
}
