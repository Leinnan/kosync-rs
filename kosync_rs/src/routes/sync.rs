//! HTTP route handlers for the `KOReader` sync protocol.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::{get, post, put},
};

use crate::{
    auth,
    error::{ApiError, ErrorCode},
    models::{
        AuthResponse, GetProgressResponse, HealthResponse, ProgressRequest, ProgressResponse,
        ProgressUpdateResponse, UserCreateRequest, UserCreateResponse,
    },
    state::AppState,
    store::{self, StoreError},
    util::{is_valid_field, is_valid_key_field, now_unix},
};

impl From<store::Document> for ProgressResponse {
    fn from(doc: store::Document) -> Self {
        Self {
            document_hash: doc.document_hash,
            percentage: doc.percentage,
            progress: doc.progress,
            device: doc.device,
            device_id: doc.device_id,
            timestamp: doc.timestamp,
        }
    }
}

/// Build the sync-protocol router.
pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/healthcheck", get(healthcheck))
        .route("/users/create", post(create_user))
        .route("/users/auth", get(auth_user))
        .route("/syncs/progress", put(update_progress))
        .route("/syncs/progress/{document}", get(get_progress))
}

async fn healthcheck() -> Json<HealthResponse> {
    Json(HealthResponse { state: "OK" })
}

async fn create_user(
    State(state): State<AppState>,
    Json(payload): Json<UserCreateRequest>,
) -> Result<(axum::http::StatusCode, Json<UserCreateResponse>), ApiError> {
    if state.config.registration_disabled {
        return Err(ApiError::new(ErrorCode::RegistrationDisabled));
    }

    let username = payload
        .username
        .as_deref()
        .filter(|u| is_valid_key_field(u));
    let password = payload.password.as_deref().filter(|p| is_valid_field(p));

    let (Some(username), Some(password)) = (username, password) else {
        return Err(ApiError::new(ErrorCode::InvalidFields));
    };

    store::create_user(&state.pool, username, password, true)
        .await
        .map_err(StoreError::into_api_error)?;

    Ok((
        axum::http::StatusCode::CREATED,
        Json(UserCreateResponse {
            username: username.to_owned(),
        }),
    ))
}

async fn auth_user(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AuthResponse>, ApiError> {
    auth::authenticate(&state.pool, &headers).await?;
    Ok(Json(AuthResponse { authorized: "OK" }))
}

async fn update_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ProgressRequest>,
) -> Result<Json<ProgressUpdateResponse>, ApiError> {
    let user = auth::authenticate(&state.pool, &headers).await?;

    let Some(document) = payload
        .document
        .as_deref()
        .filter(|d| is_valid_key_field(d))
    else {
        return Err(ApiError::new(ErrorCode::DocumentMissing));
    };

    let progress = payload.progress.as_deref().filter(|p| is_valid_field(p));
    let percentage = payload.percentage;
    let device = payload.device.as_deref().filter(|d| is_valid_field(d));

    let (Some(progress), Some(percentage), Some(device)) = (progress, percentage, device) else {
        return Err(ApiError::new(ErrorCode::InvalidFields));
    };

    let timestamp = now_unix();
    let metadata = payload.metadata.as_ref();
    let input = store::ProgressInput {
        document_hash: document,
        progress,
        percentage,
        device,
        device_id: payload.device_id.as_deref(),
        timestamp,
        title: metadata.and_then(|m| m.title.as_deref()),
        authors: metadata.and_then(|m| m.authors.as_deref()),
        filename: metadata.and_then(|m| m.filename.as_deref()),
    };
    store::record_progress(&state.pool, user.id, &input)
        .await
        .map_err(StoreError::into_api_error)?;

    Ok(Json(ProgressUpdateResponse {
        document: document.to_owned(),
        timestamp,
    }))
}

async fn get_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document): Path<String>,
) -> Result<Json<GetProgressResponse>, ApiError> {
    let user = auth::authenticate(&state.pool, &headers).await?;

    if !is_valid_key_field(&document) {
        return Err(ApiError::new(ErrorCode::DocumentMissing));
    }

    let doc = store::get_progress(&state.pool, user.id, &document)
        .await
        .map_err(StoreError::into_api_error)?;

    let body = match doc {
        Some(d) => GetProgressResponse::Found(ProgressResponse::from(d)),
        None => GetProgressResponse::Missing(crate::models::EmptyObject {}),
    };

    Ok(Json(body))
}
