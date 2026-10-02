//! Authentication helpers based on the `x-auth-user` / `x-auth-key` headers.

use axum::http::HeaderMap;
use sqlx::SqlitePool;

use crate::error::{ApiError, ErrorCode};
use crate::store::{self, User};
use crate::util::is_valid_key_field;

fn header<'a>(map: &'a HeaderMap, name: &str) -> Option<&'a str> {
    map.get(name).and_then(|v| v.to_str().ok())
}

/// Authenticate a request using the `KOReader` auth headers.
///
/// Looks up the user named in `x-auth-user` and compares `x-auth-key`
/// against the stored password hash, additionally rejecting inactive users.
///
/// # Errors
///
/// Returns [`ErrorCode::Unauthorized`] on missing/invalid credentials,
/// inactive accounts, or unknown users, and [`ErrorCode::Internal`] on
/// storage failures.
pub(crate) async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> Result<User, ApiError> {
    let username = header(headers, "x-auth-user").unwrap_or("");
    let key = header(headers, "x-auth-key").unwrap_or("");

    if !is_valid_key_field(username) || key.is_empty() {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    }

    let user = store::get_user(pool, username)
        .await
        .map_err(store::StoreError::into_api_error)?;

    let user = user
        .filter(|u| u.password_hash == key)
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;

    if !user.is_active {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    }

    Ok(user)
}

/// Authenticate a request and require administrator privileges.
///
/// # Errors
///
/// Returns [`ErrorCode::Unauthorized`] for the same reasons as
/// [`authenticate`], and additionally when the user is not an administrator.
pub(crate) async fn require_admin(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<User, ApiError> {
    let user = authenticate(pool, headers).await?;
    if user.is_administrator {
        Ok(user)
    } else {
        Err(ApiError::new(ErrorCode::Unauthorized))
    }
}
