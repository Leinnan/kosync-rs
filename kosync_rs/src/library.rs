//! Shared book import and file-management logic for the OPDS library.

use std::io::Cursor;
use std::path::PathBuf;

use axum::{
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{
    cover,
    db::md5_hex,
    epub, metadata, mobi,
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
    /// The translation key of the message for failures.
    pub(crate) message: Option<&'static str>,
}

/// Regenerate the file names readers may know each book by, then file any
/// progress recorded under them under the book's digest.
///
/// Failures are logged: identifiers that are not recognised yet only keep a
/// book's progress split until the next run.
pub(crate) async fn refresh_identities(state: &AppState) {
    match store::refresh_name_aliases(&state.pool).await {
        Ok(stored) => tracing::debug!(stored, "refreshed predicted book file names"),
        Err(err) => err.log(),
    }
    match store::reconcile_documents(&state.pool).await {
        Ok(0) => {}
        Ok(merged) => tracing::info!(merged, "merged split reading progress into library books"),
        Err(err) => err.log(),
    }
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
        match store::update_publication_hash(&state.pool, id, &stored, &actual, now_unix()).await {
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
            && row.thumb_width.is_some() == images.thumb_size.is_some()
            && row.cover_tint.is_some() == images.cover_tint.is_some();
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
        Some(path) => match tokio::fs::read(resolve_path(state, path)).await {
            Ok(thumb) => Some(StoredThumbnail {
                path: path.clone(),
                size: cover::dimensions(&thumb),
                tint: match &row.cover_tint {
                    Some(tint) => Some(tint.clone()),
                    None => tint_of(thumb).await,
                },
            }),
            Err(_) => None,
        },
        None => None,
    };
    let thumb = match stored_thumb {
        Some(thumb) => Some(thumb),
        None => store_thumbnail(state, &row.document_hash, bytes).await,
    };
    Some(CoverImages {
        cover_path: Some(cover_path.clone()),
        cover_media_type: Some(mime.clone()),
        cover_size,
        ..thumb.map(StoredThumbnail::into_images).unwrap_or_default()
    })
}

/// Extract the cover picture from a stored `EPUB`, off the async runtime.
async fn embedded_cover(state: &AppState, file_path: &str) -> Option<(Vec<u8>, String)> {
    let data = tokio::fs::read(resolve_path(state, file_path)).await.ok()?;
    tokio::task::spawn_blocking(move || epub::parse(Cursor::new(data)).ok()?.cover)
        .await
        .ok()
        .flatten()
        .map(|cover| (cover.bytes, cover.mime))
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
/// `MOBI` files are converted first and stored only as `EPUB`. `uploaded_by`
/// is the uploading account, which may later replace the book's cover.
pub(crate) async fn import_file(
    state: &AppState,
    file_name: Option<String>,
    data: &[u8],
    uploaded_by: Option<i64>,
) -> ImportOutcome {
    if mobi::is_mobi(data) {
        import_mobi(state, file_name, data, uploaded_by).await
    } else {
        let stored_name = file_name.clone();
        let upload = Upload {
            file_name,
            stored_name,
            source_hash: None,
            uploaded_by,
        };
        import_epub(state, upload, data).await
    }
}

/// Where an `EPUB` being imported came from.
struct Upload<'a> {
    /// The name reported back to the uploader.
    file_name: Option<String>,
    /// The name recorded on the publication.
    stored_name: Option<String>,
    /// Digest of the file it was converted from, if any.
    source_hash: Option<&'a str>,
    /// The uploading account, if known.
    uploaded_by: Option<i64>,
}

/// Convert an uploaded `MOBI` file and store the resulting `EPUB`.
///
/// Conversion output is not guaranteed to be stable, so duplicates key on the
/// digest of the uploaded file. The outcome keeps the uploaded name, while the
/// publication records the name of the `EPUB` it actually holds.
async fn import_mobi(
    state: &AppState,
    file_name: Option<String>,
    data: &[u8],
    uploaded_by: Option<i64>,
) -> ImportOutcome {
    let source_hash = epub::partial_md5_bytes(data);

    if let Ok(Some(document_hash)) =
        store::document_hash_for_source(&state.pool, &source_hash).await
    {
        return ImportOutcome {
            file_name,
            status: ImportStatus::Duplicate,
            document_hash: Some(document_hash),
            message: None,
        };
    }

    let source = data.to_vec();
    let converted = tokio::task::spawn_blocking(move || mobi::to_epub(&source)).await;
    let epub = match converted {
        Ok(Ok(epub)) => epub,
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "uploaded MOBI could not be converted");
            let message = if matches!(err, mobi::ConvertError::DrmProtected) {
                "import_drm"
            } else {
                "import_invalid_mobi"
            };
            return ImportOutcome {
                file_name,
                status: ImportStatus::Error,
                document_hash: None,
                message: Some(message),
            };
        }
        Err(err) => {
            tracing::error!(error = %err, "MOBI conversion task failed");
            return ImportOutcome {
                file_name,
                status: ImportStatus::Error,
                document_hash: None,
                message: Some("import_mobi_failed"),
            };
        }
    };

    let stored_name = file_name.as_deref().map(epub_file_name);
    let upload = Upload {
        file_name,
        stored_name,
        source_hash: Some(&source_hash),
        uploaded_by,
    };
    import_epub(state, upload, &epub).await
}

/// Replace a file name's extension with `.epub`.
fn epub_file_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    format!("{stem}.epub")
}

/// Parse and store an `EPUB`, returning its outcome.
///
/// Duplicate detection keys on the `KOReader` partial `MD5` document digest,
/// so sync progress always matches the file on the reader.
#[allow(clippy::too_many_lines)]
async fn import_epub(state: &AppState, upload: Upload<'_>, data: &[u8]) -> ImportOutcome {
    let Upload {
        file_name,
        stored_name,
        source_hash,
        uploaded_by,
    } = upload;
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
        let message = err.key();
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
                message: Some("import_invalid_epub"),
            };
        }
    };

    if let Err(err) = std::fs::create_dir_all(&state.config.books_dir) {
        tracing::error!(error = %err, "failed to create books directory");
        return ImportOutcome {
            file_name,
            status: ImportStatus::Error,
            document_hash: None,
            message: Some("import_storage"),
        };
    }

    let epub_path = format!("{document_hash}.epub");
    if let Err(err) = tokio::fs::write(resolve_path(state, &epub_path), data).await {
        tracing::error!(error = %err, "failed to write EPUB file");
        return ImportOutcome {
            file_name,
            status: ImportStatus::Error,
            document_hash: None,
            message: Some("import_storage"),
        };
    }

    let images = if let Some(cover) = parsed.cover.as_ref() {
        store_cover(state, &document_hash, &cover.bytes, &cover.mime).await
    } else {
        CoverImages::default()
    };

    let title = parsed
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| stored_name.clone())
        .unwrap_or_else(|| "Untitled".to_owned());

    let authors = serde_json::to_string(&parsed.authors).unwrap_or_else(|_| "[]".to_owned());
    let genres = serde_json::to_string(&parsed.subjects).unwrap_or_else(|_| "[]".to_owned());
    let file_name_hash = stored_name.as_deref().map(md5_hex);
    let timestamp = now_unix();

    let input = PublicationInput {
        document_hash: &document_hash,
        source_hash,
        file_name_hash: file_name_hash.as_deref(),
        file_name: stored_name.as_deref(),
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
        uploaded_by,
        timestamp,
    };

    match store::insert_publication(&state.pool, &input).await {
        Ok(_) => {
            // Readers may already have synced this book under a name it
            // predicts, before it was in the library.
            refresh_identities(state).await;

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
                message: Some("import_save_failed"),
            }
        }
    }
}

/// A book rewritten around a new cover picture.
struct RewrittenCover {
    /// The rewritten `EPUB`.
    epub: Vec<u8>,
    /// The picture now inside it.
    cover: Vec<u8>,
    /// The picture's MIME type, unchanged from the old cover.
    mime: String,
}

/// Rewrite `data` with `image` in place of its cover picture.
///
/// Only the cover image entry changes: the package, spine and pages are
/// copied byte for byte, so reading positions recorded against the old file
/// stay valid. The result is parsed again and must show the new picture as
/// its cover. Errors are translation keys.
fn rewrite_cover(data: &[u8], image: &[u8]) -> Result<RewrittenCover, &'static str> {
    let parsed = epub::parse(Cursor::new(data)).map_err(|err| {
        tracing::warn!(error = %err, "stored EPUB could not be parsed for a cover change");
        "cover_rewrite_failed"
    })?;
    let old = parsed.cover.ok_or("cover_none")?;
    let cover = cover::prepare_replacement(image, &old.mime).map_err(|err| {
        tracing::info!(error = %err, "replacement cover rejected");
        "cover_invalid_image"
    })?;
    let rewritten = epub::replace_entry(data, &old.path, &cover).map_err(|err| {
        tracing::warn!(error = %err, "could not rewrite EPUB with a new cover");
        "cover_rewrite_failed"
    })?;

    let shows_new_cover = epub::preflight(&rewritten).is_ok()
        && epub::parse(Cursor::new(&rewritten))
            .ok()
            .and_then(|parsed| parsed.cover)
            .is_some_and(|found| found.path == old.path && found.bytes == cover);
    if !shows_new_cover {
        tracing::warn!(path = %old.path, "rewritten EPUB does not show the new cover");
        return Err("cover_rewrite_failed");
    }

    Ok(RewrittenCover {
        epub: rewritten,
        cover,
        mime: old.mime,
    })
}

/// Replace a publication's cover picture, inside its `EPUB` and on the site.
///
/// The rewritten book gets a new `KOReader` digest; the old one stays an alias
/// of the publication and recorded progress moves along, so copies of either
/// version keep syncing the same reading position. Returns the new digest, or
/// a translation key describing the failure.
///
/// # Errors
///
/// Returns `cover_none` if the book has no cover picture to replace,
/// `cover_invalid_image` if `image` is not a usable picture,
/// `cover_rewrite_failed` if the book cannot be rewritten, and
/// `import_storage` or `cover_save_failed` if the result cannot be stored.
pub(crate) async fn replace_cover(
    state: &AppState,
    publication: &store::Publication,
    image: Vec<u8>,
) -> Result<String, &'static str> {
    let data = tokio::fs::read(resolve_path(state, &publication.file_path))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "failed to read stored EPUB");
            "import_storage"
        })?;
    let rewritten = tokio::task::spawn_blocking(move || rewrite_cover(&data, &image))
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "cover rewrite task failed");
            "cover_rewrite_failed"
        })??;

    let document_hash = epub::partial_md5_bytes(&rewritten.epub);
    if document_hash == publication.document_hash {
        // The same picture again: the book is unchanged.
        return Ok(document_hash);
    }

    let epub_path = format!("{document_hash}.epub");
    if let Err(err) = tokio::fs::write(resolve_path(state, &epub_path), &rewritten.epub).await {
        tracing::error!(error = %err, "failed to write EPUB file");
        return Err("import_storage");
    }
    let images = store_cover(state, &document_hash, &rewritten.cover, &rewritten.mime).await;
    if images.cover_path.is_none() {
        remove_file(state, &epub_path).await;
        remove_images(state, &images).await;
        return Err("import_storage");
    }

    let replacement = store::Replacement {
        document_hash: &document_hash,
        file_path: &epub_path,
        file_size: i64::try_from(rewritten.epub.len()).unwrap_or(i64::MAX),
        images: &images,
        timestamp: now_unix(),
    };
    if let Err(err) = store::replace_publication_file(
        &state.pool,
        publication.id,
        &publication.document_hash,
        &replacement,
    )
    .await
    {
        err.log();
        remove_file(state, &epub_path).await;
        remove_images(state, &images).await;
        return Err("cover_save_failed");
    }

    remove_file(state, &publication.file_path).await;
    for path in [&publication.cover_path, &publication.thumb_path]
        .into_iter()
        .flatten()
    {
        remove_file(state, path).await;
    }
    Ok(document_hash)
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

    let thumb = store_thumbnail(state, document_hash, bytes.to_vec()).await;
    CoverImages {
        cover_path: Some(cover_path),
        cover_media_type: Some(mime.to_owned()),
        cover_size: cover::dimensions(bytes),
        ..thumb.map(StoredThumbnail::into_images).unwrap_or_default()
    }
}

/// A thumbnail written to the books directory.
pub(crate) struct StoredThumbnail {
    /// Path relative to the books directory.
    path: String,
    /// Pixel size, if it could be measured.
    size: Option<(u32, u32)>,
    /// Tint colour taken from the thumbnail.
    tint: Option<String>,
}

impl StoredThumbnail {
    /// The thumbnail fields of [`CoverImages`], with no cover.
    fn into_images(self) -> CoverImages {
        CoverImages {
            thumb_path: Some(self.path),
            thumb_size: self.size,
            cover_tint: self.tint,
            ..CoverImages::default()
        }
    }
}

/// The tint of a thumbnail, worked out off the async runtime.
async fn tint_of(thumbnail: Vec<u8>) -> Option<String> {
    tokio::task::spawn_blocking(move || cover::tint(&thumbnail))
        .await
        .ok()
        .flatten()
}

/// Generate and persist a cover thumbnail, returning its relative path, pixel
/// size and tint.
///
/// Decoding and resizing run on the blocking thread pool so a large cover does
/// not stall the async runtime.
pub(crate) async fn store_thumbnail(
    state: &AppState,
    document_hash: &str,
    cover: Vec<u8>,
) -> Option<StoredThumbnail> {
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
    let tint = tint_of(thumbnail.clone()).await;
    match tokio::fs::write(resolve_path(state, &path), thumbnail).await {
        Ok(()) => Some(StoredThumbnail { path, size, tint }),
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
