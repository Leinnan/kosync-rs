//! HTTP Basic authentication for the OPDS endpoints.
//!
//! The credentials are sent as `Authorization: Basic base64(user:pass)` with a
//! plaintext password, which is `MD5`-hashed and compared against the stored
//! hash (the same representation used by the `KOReader` sync protocol).

use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sqlx::SqlitePool;

use crate::db::md5_hex;
use crate::error::{ApiError, ErrorCode};
use crate::store::{self, User};

/// Extract and verify `HTTP Basic` credentials from the request headers.
///
/// # Errors
///
/// Returns [`ErrorCode::Unauthorized`] on missing/malformed credentials, unknown
/// or inactive users, or a password mismatch, and [`ErrorCode::Internal`] on
/// storage failures.
pub(crate) async fn authenticate(pool: &SqlitePool, headers: &HeaderMap) -> Result<User, ApiError> {
    let (username, password) = parse_credentials(headers)?;
    let hash = md5_hex(&password);

    let user = store::get_user(pool, &username)
        .await
        .map_err(store::StoreError::into_api_error)?;

    let user = user
        .filter(|u| u.password_hash == hash)
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;

    if !user.is_active {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    }

    Ok(user)
}

/// Authenticate via `HTTP Basic` and require administrator privileges.
///
/// # Errors
///
/// Returns [`ErrorCode::Unauthorized`] for the same reasons as [`authenticate`],
/// and additionally when the user is not an administrator.
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

/// Authenticate via `HTTP Basic` and require permission to upload books.
///
/// Administrators and users granted the `can_upload` permission are allowed.
///
/// # Errors
///
/// Returns [`ErrorCode::Unauthorized`] for the same reasons as [`authenticate`],
/// and additionally when the user may not upload books.
pub(crate) async fn require_uploader(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<User, ApiError> {
    let user = authenticate(pool, headers).await?;
    if user.may_upload() {
        Ok(user)
    } else {
        Err(ApiError::new(ErrorCode::Unauthorized))
    }
}

fn parse_credentials(headers: &HeaderMap) -> Result<(String, String), ApiError> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let encoded = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;

    let decoded = STANDARD
        .decode(encoded)
        .map_err(|_| ApiError::new(ErrorCode::Unauthorized))?;
    let credentials =
        String::from_utf8(decoded).map_err(|_| ApiError::new(ErrorCode::Unauthorized))?;

    credentials
        .split_once(':')
        .map(|(user, pass)| (user.to_owned(), pass.to_owned()))
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))
}
