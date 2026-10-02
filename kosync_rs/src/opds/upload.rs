//! EPUB upload and deletion handlers for the shared OPDS library.

#![allow(
    clippy::result_large_err,
    reason = "Axum handlers return complete HTTP responses for authentication and storage failures; boxing them adds allocation without a practical benefit."
)]

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::HeaderMap,
};
use serde::Serialize;

use crate::{library, state::AppState, store};

use super::{not_found, require_admin, require_uploader, store_error};

/// Outcome for a single uploaded file.
#[derive(Debug, Serialize)]
pub(super) struct UploadItemResult {
    #[serde(rename = "fileName")]
    file_name: Option<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

impl From<library::ImportOutcome> for UploadItemResult {
    fn from(outcome: library::ImportOutcome) -> Self {
        Self {
            file_name: outcome.file_name,
            status: outcome.status.as_str(),
            id: outcome.document_hash,
            message: outcome.message,
        }
    }
}

/// Aggregated upload response.
#[derive(Debug, Serialize)]
pub(super) struct UploadResponse {
    results: Vec<UploadItemResult>,
}

/// Handle a multipart `EPUB` upload (administrators and users granted the
/// `can_upload` permission).
///
/// # Errors
///
/// Returns an error response if the caller may not upload books or the
/// multipart body cannot be read.
pub(super) async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, axum::response::Response> {
    let _uploader = require_uploader(&state, &headers).await?;

    let mut results = Vec::new();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                tracing::error!(error = %err, "failed to read upload part");
                results.push(UploadItemResult {
                    file_name: None,
                    status: "error",
                    id: None,
                    message: Some("Failed to read upload".to_owned()),
                });
                break;
            }
        };

        let file_name = field.file_name().map(str::to_owned);
        let data = match field.bytes().await {
            Ok(data) => data,
            Err(err) => {
                tracing::error!(error = %err, "failed to read upload bytes");
                results.push(UploadItemResult {
                    file_name,
                    status: "error",
                    id: None,
                    message: Some("Failed to read file".to_owned()),
                });
                continue;
            }
        };

        let outcome = library::import_file(&state, file_name, &data).await;
        results.push(outcome.into());
    }

    Ok(Json(UploadResponse { results }))
}

/// Delete a publication and its stored files (admin only).
///
/// # Errors
///
/// Returns an error response if the caller is not an administrator, the
/// publication does not exist, or the database update fails.
pub(super) async fn delete_publication(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document_hash): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let _admin = require_admin(&state, &headers).await?;

    let publication = store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .map_err(|e| store_error(&e))?
        .ok_or_else(not_found)?;

    library::remove_file(&state, &publication.file_path).await;
    if let Some(path) = &publication.cover_path {
        library::remove_file(&state, path).await;
    }
    if let Some(path) = &publication.thumb_path {
        library::remove_file(&state, path).await;
    }

    store::delete_publication(&state.pool, publication.id)
        .await
        .map_err(|e| store_error(&e))?;

    Ok(Json(serde_json::json!({ "message": "Deleted" })))
}
