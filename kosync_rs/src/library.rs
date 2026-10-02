//! Shared `EPUB` import and file-management logic for the OPDS library.

use std::io::Cursor;
use std::path::PathBuf;

use crate::{
    cover,
    db::md5_hex,
    epub, metadata,
    state::AppState,
    store::{self, PublicationInput, StoreError},
    util::now_unix,
};

/// The outcome of importing a single file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportStatus {
    /// The file was stored and recorded successfully.
    Imported,
    /// A publication with the same digest already exists.
    Duplicate,
    /// The file could not be imported.
    Error,
}

impl ImportStatus {
    /// The machine-readable status string.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Duplicate => "duplicate",
            Self::Error => "error",
        }
    }
}

/// The result of importing a single uploaded file.
#[derive(Debug)]
pub(crate) struct ImportOutcome {
    /// The original file name, if known.
    pub(crate) file_name: Option<String>,
    /// The import status.
    pub(crate) status: ImportStatus,
    /// The document digest on success or duplicate.
    pub(crate) document_hash: Option<String>,
    /// A human-readable message for failures.
    pub(crate) message: Option<String>,
}

/// Resolve a stored file path relative to the books directory.
pub(crate) fn resolve_path(state: &AppState, relative: &str) -> PathBuf {
    std::path::Path::new(&state.config.books_dir).join(relative)
}

/// Strip characters that are unsafe in file names.
#[must_use]
pub(crate) fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '/' || c == '\\' || c == '"' || c == '\0' {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Parse and store a single uploaded file, returning its outcome.
///
/// Duplicate detection keys on the `KOReader` partial `MD5` document digest,
/// so sync progress always matches the file on the reader.
#[allow(clippy::too_many_lines)]
pub(crate) async fn import_file(
    state: &AppState,
    file_name: Option<String>,
    data: &[u8],
) -> ImportOutcome {
    let document_hash = epub::partial_md5_bytes(data);

    if store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        return ImportOutcome {
            file_name,
            status: ImportStatus::Duplicate,
            document_hash: Some(document_hash),
            message: None,
        };
    }

    let parsed = match epub::parse(Cursor::new(data)) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(error = %err, "uploaded file is not a valid EPUB");
            return ImportOutcome {
                file_name,
                status: ImportStatus::Error,
                document_hash: None,
                message: Some("Not a valid EPUB file".to_owned()),
            };
        }
    };

    if let Err(err) = std::fs::create_dir_all(&state.config.books_dir) {
        tracing::error!(error = %err, "failed to create books directory");
        return ImportOutcome {
            file_name,
            status: ImportStatus::Error,
            document_hash: None,
            message: Some("Storage error".to_owned()),
        };
    }

    let epub_path = format!("{document_hash}.epub");
    if let Err(err) = tokio::fs::write(resolve_path(state, &epub_path), data).await {
        tracing::error!(error = %err, "failed to write EPUB file");
        return ImportOutcome {
            file_name,
            status: ImportStatus::Error,
            document_hash: None,
            message: Some("Storage error".to_owned()),
        };
    }

    let (cover_path, cover_media_type, thumb_path) =
        if let Some((bytes, mime)) = parsed.cover.as_ref() {
            store_cover(state, &document_hash, bytes, mime).await
        } else {
            (None, None, None)
        };

    let title = parsed
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| file_name.clone())
        .unwrap_or_else(|| "Untitled".to_owned());

    let authors = serde_json::to_string(&parsed.authors).unwrap_or_else(|_| "[]".to_owned());
    let genres = serde_json::to_string(&parsed.subjects).unwrap_or_else(|_| "[]".to_owned());
    let file_name_hash = file_name.as_deref().map(md5_hex);
    let timestamp = now_unix();

    let input = PublicationInput {
        document_hash: &document_hash,
        file_name_hash: file_name_hash.as_deref(),
        file_name: file_name.as_deref(),
        title: &title,
        authors: &authors,
        language: parsed.language.as_deref(),
        identifier: parsed.identifier.as_deref(),
        publisher: parsed.publisher.as_deref(),
        published: parsed.published.as_deref(),
        description: parsed.description.as_deref(),
        genres: &genres,
        isbn: parsed.isbn.as_deref(),
        series: parsed.series.as_deref(),
        series_index: parsed.series_index,
        file_path: &epub_path,
        file_size: i64::try_from(data.len()).unwrap_or(i64::MAX),
        cover_path: cover_path.as_deref(),
        cover_media_type: cover_media_type.as_deref(),
        thumb_path: thumb_path.as_deref(),
        thumb_media_type: thumb_path.as_ref().map(|_| "image/jpeg"),
        timestamp,
    };

    match store::insert_publication(&state.pool, &input).await {
        Ok(_) => {
            if state.config.metadata_enrichment && state.http_client.is_some() {
                let state = state.clone();
                let enrichment_hash = document_hash.clone();
                // Detaching this task keeps upload latency independent of remote APIs.
                std::mem::drop(tokio::spawn(async move {
                    metadata::enrich_publication(&state, &enrichment_hash).await;
                }));
            }

            ImportOutcome {
                file_name,
                status: ImportStatus::Imported,
                document_hash: Some(document_hash),
                message: None,
            }
        }
        Err(StoreError::Exists) => {
            remove_file(state, &epub_path).await;
            if let Some(path) = cover_path.as_deref() {
                remove_file(state, path).await;
            }
            if let Some(path) = thumb_path.as_deref() {
                remove_file(state, path).await;
            }
            ImportOutcome {
                file_name,
                status: ImportStatus::Duplicate,
                document_hash: Some(document_hash),
                message: None,
            }
        }
        Err(err) => {
            err.log();
            remove_file(state, &epub_path).await;
            if let Some(path) = cover_path.as_deref() {
                remove_file(state, path).await;
            }
            if let Some(path) = thumb_path.as_deref() {
                remove_file(state, path).await;
            }
            ImportOutcome {
                file_name,
                status: ImportStatus::Error,
                document_hash: None,
                message: Some("Failed to save publication".to_owned()),
            }
        }
    }
}

/// Remove a stored file, ignoring failures.
pub(crate) async fn remove_file(state: &AppState, relative: &str) {
    let full = resolve_path(state, relative);
    if let Err(err) = tokio::fs::remove_file(&full).await {
        tracing::warn!(error = %err, path = %full.display(), "failed to remove stored file");
    }
}

/// Open a stored file as a streaming response body.
///
/// # Errors
///
/// Returns an error if the file cannot be opened.
pub(crate) async fn file_body(
    state: &AppState,
    relative: &str,
) -> std::io::Result<axum::body::Body> {
    use tokio_util::io::ReaderStream;

    let file = tokio::fs::File::open(resolve_path(state, relative)).await?;
    Ok(axum::body::Body::from_stream(ReaderStream::new(file)))
}

/// Persist a cover image and generate its thumbnail.
pub(crate) async fn store_cover(
    state: &AppState,
    document_hash: &str,
    bytes: &[u8],
    mime: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let extension = extension_for_mime(mime);
    let cover_path = format!("{document_hash}.cover.{extension}");

    if let Err(err) = tokio::fs::write(resolve_path(state, &cover_path), bytes).await {
        tracing::error!(error = %err, "failed to write cover image");
        return (None, None, None);
    }

    let thumb_path = match cover::thumbnail(bytes) {
        Ok(thumbnail) => {
            let path = format!("{document_hash}.thumb.jpg");
            match tokio::fs::write(resolve_path(state, &path), thumbnail).await {
                Ok(()) => Some(path),
                Err(err) => {
                    tracing::error!(error = %err, "failed to write cover thumbnail");
                    None
                }
            }
        }
        Err(err) => {
            tracing::warn!(error = %err, "failed to generate cover thumbnail");
            None
        }
    };

    (Some(cover_path), Some(mime.to_owned()), thumb_path)
}

/// Map a cover MIME type to a file extension.
fn extension_for_mime(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        _ => "img",
    }
}
