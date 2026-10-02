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
    Form,
    extract::{Path, Query, State},
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;

use crate::{
    db,
    models::UserSummary,
    state::AppState,
    store::{self, StoreError},
    util::{is_valid_field, is_valid_key_field},
};

use super::{base_context, current_user, format_timestamp, render};

/// A user row prepared for rendering, with the last sync as RFC 3339.
#[derive(Debug, Serialize)]
struct UserRow {
    #[serde(flatten)]
    user: UserSummary,
    last_synced_at: String,
}

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

/// Redirect back to the users page with an error flash code.
fn redirect_error(code: &str) -> Response {
    Redirect::to(&format!("/admin/users?error={code}")).into_response()
}

/// Redirect back to the users page with a success flash code.
fn redirect_notice(code: &str) -> Response {
    Redirect::to(&format!("/admin/users?notice={code}")).into_response()
}

/// Map flash codes from the query string to a rendered message.
fn flash_message(query: &FlashQuery) -> Option<(&'static str, &'static str)> {
    if let Some(code) = &query.error {
        let text = match code.as_str() {
            "invalid" => "Enter a username and a non-empty password.",
            "exists" => "A user with that name already exists.",
            "missing" => "That user no longer exists.",
            "protected" => "The built-in admin account cannot be modified.",
            "db" => "Something went wrong. Please try again.",
            _ => return None,
        };
        return Some(("error", text));
    }

    if let Some(code) = &query.notice {
        let text = match code.as_str() {
            "created" => "User created.",
            "activated" => "User activated.",
            "deactivated" => "User deactivated.",
            "upload-granted" => "Upload permission granted.",
            "upload-revoked" => "Upload permission revoked.",
            "password-reset" => "Password reset.",
            "deleted" => "User deleted.",
            _ => return None,
        };
        return Some(("notice", text));
    }

    None
}

/// Render the user-management page (administrator only).
pub(super) async fn list(
    State(state): State<AppState>,
    Query(query): Query<FlashQuery>,
    cookies: Cookies,
) -> Response {
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

    let mut ctx = base_context(&user, "users");
    ctx.insert("users", &rows);
    if let Some((kind, text)) = flash_message(&query) {
        ctx.insert(kind, text);
    }
    render(&state, "users.html", &ctx)
}

/// Render the users page with a generic database-error banner.
fn render_error(state: &AppState, user: &store::User) -> Response {
    let mut ctx = base_context(user, "users");
    ctx.insert("error", "Something went wrong. Please try again.");
    render(state, "users.html", &ctx)
}

/// Create a new account (administrator only).
pub(super) async fn create(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<CreateForm>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }

    let username = form.username.trim();
    if !is_valid_key_field(username) || !is_valid_field(&form.password) {
        return redirect_error("invalid");
    }

    let hash = db::md5_hex(&form.password);
    let can_upload = form.can_upload.is_some();

    match store::create_user(&state.pool, username, &hash, can_upload).await {
        Ok(()) => redirect_notice("created"),
        Err(StoreError::Exists) => redirect_error("exists"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}

/// Toggle a user's active flag (administrator only).
pub(super) async fn toggle_active(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(username): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }
    if username == "admin" {
        return redirect_error("protected");
    }

    match store::toggle_active(&state.pool, &username).await {
        Ok(Some(true)) => redirect_notice("activated"),
        Ok(Some(false)) => redirect_notice("deactivated"),
        Ok(None) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}

/// Grant or revoke a user's upload permission (administrator only).
pub(super) async fn toggle_can_upload(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(username): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }
    if username == "admin" {
        return redirect_error("protected");
    }

    match store::toggle_can_upload(&state.pool, &username).await {
        Ok(Some(true)) => redirect_notice("upload-granted"),
        Ok(Some(false)) => redirect_notice("upload-revoked"),
        Ok(None) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}

/// Reset a user's password (administrator only).
pub(super) async fn reset_password(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(username): Path<String>,
    Form(form): Form<PasswordForm>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }
    if username == "admin" {
        return redirect_error("protected");
    }
    if !is_valid_field(&form.password) {
        return redirect_error("invalid");
    }

    let hash = db::md5_hex(&form.password);
    match store::set_password(&state.pool, &username, &hash).await {
        Ok(true) => redirect_notice("password-reset"),
        Ok(false) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}

/// Delete a user (administrator only).
pub(super) async fn delete(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(username): Path<String>,
) -> Response {
    if let Err(response) = require_admin(&state, &cookies).await {
        return response;
    }
    if username == "admin" {
        return redirect_error("protected");
    }

    match store::delete_user(&state.pool, &username).await {
        Ok(true) => redirect_notice("deleted"),
        Ok(false) => redirect_error("missing"),
        Err(err) => {
            err.log();
            redirect_error("db")
        }
    }
}
