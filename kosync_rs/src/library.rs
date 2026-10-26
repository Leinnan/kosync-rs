//! Shared `EPUB` import and file-management logic for the OPDS library.

use std::io::Cursor;
use std::path::PathBuf;

use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{
    cover,
    db::md5_hex,
    epub, metadata,
    state::AppState,
    store::{self, CoverImages, PublicationInput, StoreError},
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

/// Recompute each stored publication's partial `MD5` digest from its file.
///
/// Earlier releases sampled from the wrong start offset, so digests never
/// matched the ones `KOReader`-compatible readers send. Stored file and cover
/// paths are untouched; only the `document_hash` column changes. Returns the
/// number of publications updated.
pub(crate) async fn rehash_publications(state: &AppState) -> usize {
    let Ok(files) = store::list_publication_files(&state.pool).await else {
        tracing::warn!("could not list publications for digest check");
        return 0;
    };

    let mut updated = 0;
    for (id, stored, file_path) in files {
        let Ok(data) = tokio::fs::read(resolve_path(state, &file_path)).await else {
            continue;
        };
        let actual = epub::partial_md5_bytes(&data);
        if actual == stored {
            continue;
        }
        match store::update_publication_hash(&state.pool, id, &actual).await {
            Ok(()) => updated += 1,
            Err(err) => {
                tracing::warn!(id, error = ?err, "could not update publication digest");
            }
        }
    }
    updated
}

/// Fill in covers and thumbnails that older imports left empty.
///
/// Publications without a cover are re-parsed with the current cover
/// detection; publications with a cover but no thumbnail get one generated.
/// Returns the number of publications updated.
pub(crate) async fn backfill_covers(state: &AppState) -> usize {
    let rows = match store::list_publications_missing_images(&state.pool).await {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!(error = ?err, "could not list publications for cover backfill");
            return 0;
        }
    };

    let mut updated = 0;
    for row in rows {
        let Some(images) = backfill_images(state, &row).await else {
            continue;
        };
        let unchanged = row.thumb_path == images.thumb_path
            && row.cover_width.is_some() == images.cover_size.is_some()
            && row.thumb_width.is_some() == images.thumb_size.is_some();
        if row.cover_path.is_some() && unchanged {
            // Nothing new could be produced; failures are already logged.
            continue;
        }

        match store::update_publication_images(&state.pool, row.id, &images, now_unix()).await {
            Ok(()) => updated += 1,
            Err(err) => tracing::warn!(id = row.id, error = ?err, "could not record cover images"),
        }
    }
    updated
}

/// Work out the cover images a backfill should record for one publication.
///
/// A stored cover is measured and keeps its thumbnail when one exists (which
/// is measured too); a missing thumbnail is regenerated. Without a stored
/// cover, the `EPUB` is searched for one. Returns `None` when nothing can be
/// recorded.
async fn backfill_images(state: &AppState, row: &store::MissingImages) -> Option<CoverImages> {
    let (Some(cover_path), Some(mime)) = (&row.cover_path, &row.cover_media_type) else {
        let (bytes, mime) = embedded_cover(state, &row.file_path).await?;
        let images = store_cover(state, &row.document_hash, &bytes, &mime).await;
        return images.cover_path.is_some().then_some(images);
    };

    let bytes = tokio::fs::read(resolve_path(state, cover_path))
        .await
        .ok()?;
    let cover_size = cover::dimensions(&bytes);
    let stored_thumb = match &row.thumb_path {
        Some(path) => tokio::fs::read(resolve_path(state, path))
            .await
            .ok()
            .map(|thumb| (path.clone(), cover::dimensions(&thumb))),
        None => None,
    };
    let thumb = match stored_thumb {
        Some(thumb) => Some(thumb),
        None => store_thumbnail(state, &row.document_hash, bytes).await,
    };
    let (thumb_path, thumb_size) = thumb.unzip();
    Some(CoverImages {
        cover_path: Some(cover_path.clone()),
        cover_media_type: Some(mime.clone()),
        cover_size,
        thumb_path,
        thumb_size: thumb_size.flatten(),
    })
}

/// Extract the cover picture from a stored `EPUB`, off the async runtime.
async fn embedded_cover(state: &AppState, file_path: &str) -> Option<(Vec<u8>, String)> {
    let data = tokio::fs::read(resolve_path(state, file_path)).await.ok()?;
    tokio::task::spawn_blocking(move || epub::parse(Cursor::new(data)).ok()?.cover)
        .await
        .ok()
        .flatten()
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

    if let Err(err) = epub::preflight(data) {
        tracing::warn!(error = %err, "uploaded file failed archive checks");
        let message = if err == epub::PreflightError::NotZip {
            "Not a valid EPUB file".to_owned()
        } else {
            format!("Rejected: {err}")
        };
        return ImportOutcome {
            file_name,
            status: ImportStatus::Error,
            document_hash: None,
            message: Some(message),
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

    let images = if let Some((bytes, mime)) = parsed.cover.as_ref() {
        store_cover(state, &document_hash, bytes, mime).await
    } else {
        CoverImages::default()
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
        images: &images,
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
            remove_images(state, &images).await;
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
            remove_images(state, &images).await;
            ImportOutcome {
                file_name,
                status: ImportStatus::Error,
                document_hash: None,
                message: Some("Failed to save publication".to_owned()),
            }
        }
    }
}

/// Remove stored cover and thumbnail files, ignoring failures.
async fn remove_images(state: &AppState, images: &CoverImages) {
    for path in [&images.cover_path, &images.thumb_path]
        .into_iter()
        .flatten()
    {
        remove_file(state, path).await;
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

/// The stored path and MIME type of a publication's cover or thumbnail.
pub(crate) fn publication_image(
    publication: &store::Publication,
    thumbnail: bool,
) -> Option<(&str, &str)> {
    let (path, media_type) = if thumbnail {
        (&publication.thumb_path, &publication.thumb_media_type)
    } else {
        (&publication.cover_path, &publication.cover_media_type)
    };
    Some((path.as_deref()?, media_type.as_deref()?))
}

/// Serve a publication's cover or thumbnail with cache validators.
///
/// The `ETag` changes whenever the publication is updated, so clients that
/// re-browse a catalog get a `304 Not Modified` instead of the image again.
/// Responses are `private` because they require authentication.
///
/// # Errors
///
/// Returns an error if the stored file cannot be opened.
pub(crate) async fn image_response(
    state: &AppState,
    request: &HeaderMap,
    publication: &store::Publication,
    path: &str,
    media_type: &str,
) -> std::io::Result<Response> {
    // Stored paths embed the document digest and never contain quotes.
    let etag = format!("\"{}-{path}\"", publication.updated_at);
    let cache_headers = [
        (header::ETAG, etag.clone()),
        (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
    ];

    let matches = request
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(|tag| tag.trim().trim_start_matches("W/"))
                .any(|tag| tag == etag || tag == "*")
        });
    if matches {
        return Ok((StatusCode::NOT_MODIFIED, cache_headers).into_response());
    }

    let full = resolve_path(state, path);
    let length = tokio::fs::metadata(&full).await?.len();
    let body = file_body(state, path).await?;
    Ok((
        cache_headers,
        [
            (header::CONTENT_TYPE, media_type.to_owned()),
            (header::CONTENT_LENGTH, length.to_string()),
        ],
        body,
    )
        .into_response())
}

/// Persist a cover image and generate its thumbnail, measuring both.
///
/// Returns empty [`CoverImages`] if the cover could not be written.
pub(crate) async fn store_cover(
    state: &AppState,
    document_hash: &str,
    bytes: &[u8],
    mime: &str,
) -> CoverImages {
    let extension = extension_for_mime(mime);
    let cover_path = format!("{document_hash}.cover.{extension}");

    if let Err(err) = tokio::fs::write(resolve_path(state, &cover_path), bytes).await {
        tracing::error!(error = %err, "failed to write cover image");
        return CoverImages::default();
    }

    let (thumb_path, thumb_size) = store_thumbnail(state, document_hash, bytes.to_vec())
        .await
        .unzip();
    CoverImages {
        cover_path: Some(cover_path),
        cover_media_type: Some(mime.to_owned()),
        cover_size: cover::dimensions(bytes),
        thumb_path,
        thumb_size: thumb_size.flatten(),
    }
}

/// Generate and persist a cover thumbnail, returning its relative path and
/// pixel size.
///
/// Decoding and resizing run on the blocking thread pool so a large cover does
/// not stall the async runtime.
pub(crate) async fn store_thumbnail(
    state: &AppState,
    document_hash: &str,
    cover: Vec<u8>,
) -> Option<(String, Option<(u32, u32)>)> {
    let thumbnail = match tokio::task::spawn_blocking(move || cover::thumbnail(&cover)).await {
        Ok(Ok(thumbnail)) => thumbnail,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "failed to generate cover thumbnail");
            return None;
        }
        Err(err) => {
            tracing::error!(error = %err, "cover thumbnail task failed");
            return None;
        }
    };

    let path = format!("{document_hash}.thumb.jpg");
    let size = cover::dimensions(&thumbnail);
    match tokio::fs::write(resolve_path(state, &path), thumbnail).await {
        Ok(()) => Some((path, size)),
        Err(err) => {
            tracing::error!(error = %err, "failed to write cover thumbnail");
            None
        }
    }
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
