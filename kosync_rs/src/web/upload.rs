//! EPUB upload page for the web frontend.
//!
//! Available to administrators and users granted the `can_upload` permission.

use axum::{
    extract::{Multipart, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::Serialize;
use tower_cookies::Cookies;

use crate::{library, state::AppState};

use super::{base_context, current_user, is_htmx, render};

/// A per-file upload result prepared for rendering.
#[derive(Debug, Serialize)]
struct UploadResultView {
    file_name: String,
    status: String,
    message: Option<String>,
    /// Library hash of the imported (or already present) book.
    document_hash: Option<String>,
}

/// Render the upload form (administrators and permitted uploaders only).
pub(super) async fn form(State(state): State<AppState>, cookies: Cookies) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    if !user.may_upload() {
        return Redirect::to("/books").into_response();
    }

    let ctx = base_context(&user, "upload");
    render(&state, "upload.html", &ctx)
}

/// Handle a multipart `EPUB` upload submission (administrators and
/// permitted uploaders only).
pub(super) async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    cookies: Cookies,
    mut multipart: Multipart,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    if !user.may_upload() {
        return Redirect::to("/books").into_response();
    }

    let mut results = Vec::new();

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                tracing::error!(error = %err, "failed to read upload part");
                break;
            }
        };

        let file_name = field.file_name().map(str::to_owned);
        let data = match field.bytes().await {
            Ok(data) => data,
            Err(err) => {
                tracing::error!(error = %err, "failed to read upload bytes");
                results.push(UploadResultView {
                    file_name: file_name.unwrap_or_default(),
                    status: "error".to_owned(),
                    message: Some("Failed to read file".to_owned()),
                    document_hash: None,
                });
                continue;
            }
        };

        let outcome = library::import_file(&state, file_name, &data).await;
        results.push(UploadResultView {
            file_name: outcome.file_name.unwrap_or_default(),
            status: outcome.status.as_str().to_owned(),
            message: outcome.message,
            document_hash: outcome.document_hash,
        });
    }

    let mut ctx = base_context(&user, "upload");
    ctx.insert("results", &results);

    if is_htmx(&headers) {
        render(&state, "upload_results.html", &ctx)
    } else {
        render(&state, "upload.html", &ctx)
    }
}
