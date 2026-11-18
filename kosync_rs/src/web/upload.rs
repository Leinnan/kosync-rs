//! Book upload page for the web frontend.
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

use web_mev_axum::RequestError;

use super::{HtmlError, base_context, csrf_header_ok, current_user, is_htmx, render};

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

    let ctx = base_context(&state, &user, "upload");
    render(&state, "upload.html", &ctx)
}

/// The response to an upload without the session's CSRF token.
fn csrf_rejected() -> Response {
    HtmlError::from(RequestError::forbidden("invalid CSRF token")).into_response()
}

/// Handle a multipart `EPUB` or `MOBI` upload submission (administrators and
/// permitted uploaders only).
///
/// The CSRF token comes in the `X-CSRF-Token` header (htmx) or as a
/// `csrf_token` part before the files (a plain form post).
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

    let mut csrf_ok = csrf_header_ok(&user, &headers);
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

        if field.name() == Some("csrf_token") {
            let supplied = field.text().await.unwrap_or_default();
            csrf_ok = csrf_ok || web_mev_core::secure_eq(&user.csrf_token, &supplied);
            continue;
        }
        if !csrf_ok {
            return csrf_rejected();
        }

        let file_name = field.file_name().map(str::to_owned);
        let data = match field.bytes().await {
            Ok(data) => data,
            Err(err) => {
                tracing::error!(error = %err, "failed to read upload bytes");
                results.push(UploadResultView {
                    file_name: file_name.unwrap_or_default(),
                    status: "error".to_owned(),
                    message: Some(user.tr(&state).t("import_read_failed")),
                    document_hash: None,
                });
                continue;
            }
        };

        let outcome = library::import_file(&state, file_name, &data, Some(user.id)).await;
        results.push(UploadResultView {
            file_name: outcome.file_name.unwrap_or_default(),
            status: outcome.status.as_str().to_owned(),
            message: outcome.message.map(|key| user.tr(&state).t(key)),
            document_hash: outcome.document_hash,
        });
    }

    if !csrf_ok {
        return csrf_rejected();
    }

    let mut ctx = base_context(&state, &user, "upload");
    ctx.insert("results", &results);

    if is_htmx(&headers) {
        render(&state, "upload_results.html", &ctx)
    } else {
        render(&state, "upload.html", &ctx)
    }
}
