//! Login, logout, and session handling.

use axum::{
    Form,
    extract::State,
    response::{IntoResponse, Redirect, Response},
};
use serde::Deserialize;
use tower_cookies::{Cookie, Cookies};

use crate::{db, state::AppState, store};

use super::{AUTH_COOKIE, current_user, render};

/// Login form fields.
#[derive(Debug, Deserialize)]
pub(super) struct LoginForm {
    username: String,
    password: String,
}

/// Render the login form, or redirect to the dashboard if already authenticated.
pub(super) async fn login_form(State(state): State<AppState>, cookies: Cookies) -> Response {
    if current_user(&state, &cookies).await.is_some() {
        return Redirect::to("/").into_response();
    }

    render(&state, "login.html", &tera::Context::new())
}

/// Handle a login submission.
pub(super) async fn login_submit(
    State(state): State<AppState>,
    cookies: Cookies,
    Form(form): Form<LoginForm>,
) -> Response {
    let hash = db::md5_hex(&form.password);

    let authenticated = matches!(
        store::get_user(&state.pool, &form.username).await,
        Ok(Some(user)) if user.is_active && user.password_hash == hash
    );

    if authenticated {
        let jar = cookies.private(&state.cookie_key);
        jar.add(Cookie::new(AUTH_COOKIE, form.username));
        Redirect::to("/").into_response()
    } else {
        let mut ctx = tera::Context::new();
        ctx.insert("error", "Invalid username or password");
        ctx.insert("form_username", &form.username);
        render(&state, "login.html", &ctx)
    }
}

/// Clear the session cookie and redirect to the login page.
pub(super) async fn logout(State(state): State<AppState>, cookies: Cookies) -> Response {
    let jar = cookies.private(&state.cookie_key);
    jar.remove(Cookie::new(AUTH_COOKIE, ""));
    Redirect::to("/login").into_response()
}
