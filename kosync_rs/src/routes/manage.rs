//! Admin and self-service management endpoints.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::Deserialize;

use crate::{
    auth, db,
    error::ApiError,
    models::{MessageResponse, PasswordChangeRequest, UserCreateRequest},
    state::AppState,
    store::{self, StoreError},
    util::{is_valid_field, is_valid_key_field},
};

/// Build the management router.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/manage/users",
            get(get_users).post(create_user).delete(delete_user),
        )
        .route(
            "/manage/users/documents",
            get(get_documents).delete(delete_document),
        )
        .route("/manage/users/active", put(update_active))
        .route("/manage/users/can-upload", put(update_can_upload))
        .route("/manage/users/password", put(update_password))
}

#[derive(Debug, Deserialize)]
struct UsernameQuery {
    username: String,
}

#[derive(Debug, Deserialize)]
struct DocumentQuery {
    username: String,
    #[serde(rename = "documentHash", default)]
    document_hash: Option<String>,
}

fn message(status: StatusCode, text: impl Into<String>) -> Response {
    (status, Json(MessageResponse::new(text))).into_response()
}

fn unauthorized() -> Response {
    message(StatusCode::UNAUTHORIZED, "Unauthorized")
}

fn bad_gateway() -> Response {
    message(StatusCode::BAD_GATEWAY, "Unknown server error.")
}

fn invalid_request() -> Response {
    message(StatusCode::FORBIDDEN, "Invalid request")
}

fn user_not_found() -> Response {
    message(StatusCode::BAD_REQUEST, "User does not exist")
}

/// Map a storage failure into a management error response, logging the cause.
fn db_error(err: &StoreError) -> Response {
    err.log();
    bad_gateway()
}

/// Map a protocol error into a management error response.
fn from_api_error(err: ApiError) -> Response {
    if err.status() == StatusCode::UNAUTHORIZED {
        unauthorized()
    } else {
        bad_gateway()
    }
}

/// Require the caller to be an admin; returns an error response otherwise.
async fn require_admin(state: &AppState, headers: &HeaderMap) -> Option<Response> {
    match auth::require_admin(&state.pool, headers).await {
        Ok(_) => None,
        Err(err) => Some(from_api_error(err)),
    }
}

/// Require the caller to be an admin, or the named user for self-service actions.
async fn authorize_admin_or_self(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
) -> Option<Response> {
    match auth::authenticate(&state.pool, headers).await {
        Ok(user) if user.is_administrator || user.username == username => None,
        Ok(_) => Some(unauthorized()),
        Err(err) => Some(from_api_error(err)),
    }
}

/// Look up a user by username, mapping storage failures to an error response.
async fn find_user(state: &AppState, username: &str) -> Result<Option<store::User>, Response> {
    store::get_user(&state.pool, username)
        .await
        .map_err(|e| db_error(&e))
}

async fn get_users(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(response) = require_admin(&state, &headers).await {
        return response;
    }

    match store::list_users(&state.pool).await {
        Ok(users) => (StatusCode::OK, Json(users)).into_response(),
        Err(e) => db_error(&e),
    }
}

async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<UserCreateRequest>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers).await {
        return response;
    }

    let Some(username) = payload
        .username
        .as_deref()
        .filter(|u| is_valid_key_field(u))
    else {
        return invalid_request();
    };
    let Some(password) = payload.password.as_deref().filter(|p| is_valid_field(p)) else {
        return invalid_request();
    };

    let hash = db::md5_hex(password);
    let can_upload = payload.can_upload.unwrap_or(true);
    match store::create_user(&state.pool, username, &hash, can_upload).await {
        Ok(()) => message(StatusCode::OK, "User created successfully"),
        Err(StoreError::Exists) => message(StatusCode::BAD_REQUEST, "User already exists"),
        Err(e) => db_error(&e),
    }
}

async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsernameQuery>,
) -> Response {
    if let Some(response) = authorize_admin_or_self(&state, &headers, &query.username).await {
        return response;
    }

    match store::delete_user(&state.pool, &query.username).await {
        Ok(true) => message(StatusCode::OK, "Success"),
        Ok(false) => message(StatusCode::NOT_FOUND, "User does not exist"),
        Err(e) => db_error(&e),
    }
}

async fn get_documents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsernameQuery>,
) -> Response {
    if let Some(response) = authorize_admin_or_self(&state, &headers, &query.username).await {
        return response;
    }

    let target = match find_user(&state, &query.username).await {
        Ok(Some(user)) => user,
        Ok(None) => return user_not_found(),
        Err(response) => return response,
    };

    match store::list_documents(&state.pool, target.id).await {
        Ok(docs) => (StatusCode::OK, Json(docs)).into_response(),
        Err(e) => db_error(&e),
    }
}

async fn delete_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DocumentQuery>,
) -> Response {
    if let Some(response) = authorize_admin_or_self(&state, &headers, &query.username).await {
        return response;
    }

    let Some(document_hash) = query.document_hash.as_deref() else {
        return invalid_request();
    };

    let target = match find_user(&state, &query.username).await {
        Ok(Some(user)) => user,
        Ok(None) => return user_not_found(),
        Err(response) => return response,
    };

    match store::delete_document(&state.pool, target.id, document_hash).await {
        Ok(true) => message(StatusCode::OK, "Success"),
        Ok(false) => message(
            StatusCode::NOT_FOUND,
            format!(
                "Document hash [{document_hash}] was not found for user [{}].",
                query.username
            ),
        ),
        Err(e) => db_error(&e),
    }
}

async fn update_active(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsernameQuery>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers).await {
        return response;
    }

    if query.username == "admin" {
        return message(StatusCode::BAD_REQUEST, "Cannot update admin user");
    }

    match store::toggle_active(&state.pool, &query.username).await {
        Ok(Some(new_active)) => message(
            StatusCode::OK,
            if new_active {
                "User marked as active"
            } else {
                "User marked as inactive"
            },
        ),
        Ok(None) => user_not_found(),
        Err(e) => db_error(&e),
    }
}

async fn update_can_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsernameQuery>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers).await {
        return response;
    }

    if query.username == "admin" {
        return message(StatusCode::BAD_REQUEST, "Cannot update admin user");
    }

    match store::toggle_can_upload(&state.pool, &query.username).await {
        Ok(Some(new_can_upload)) => message(
            StatusCode::OK,
            if new_can_upload {
                "User can upload books"
            } else {
                "User can no longer upload books"
            },
        ),
        Ok(None) => user_not_found(),
        Err(e) => db_error(&e),
    }
}

async fn update_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UsernameQuery>,
    Json(payload): Json<PasswordChangeRequest>,
) -> Response {
    if let Some(response) = require_admin(&state, &headers).await {
        return response;
    }

    let Some(password) = payload.password.as_deref().filter(|p| !p.trim().is_empty()) else {
        return message(
            StatusCode::BAD_REQUEST,
            "Password cannot be empty or whitespace",
        );
    };

    if query.username == "admin" {
        return message(StatusCode::BAD_REQUEST, "Cannot update admin user");
    }

    let hash = db::md5_hex(password);
    match store::set_password(&state.pool, &query.username, &hash).await {
        Ok(true) => message(StatusCode::OK, "Password changed successfully"),
        Ok(false) => user_not_found(),
        Err(e) => db_error(&e),
    }
}
