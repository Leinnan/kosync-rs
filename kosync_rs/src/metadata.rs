//! Metadata extraction and enrichment for publications.
//!
//! [`process_epub_metadata`] is the standalone integration point: it parses
//! an `EPUB` file from disk and, when the embedded genres are sparse,
//! enriches the result with categories and a high-resolution cover fetched
//! from external book APIs (Google Books, with Open Library as a fallback).

use std::collections::HashSet;
use std::fmt;
use std::io::Cursor;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::state::AppState;
use crate::store::{self, EnrichmentUpdate};
use crate::util::now_unix;
use crate::{epub, library};

/// Base URL of the Google Books volumes API.
const GOOGLE_BOOKS_URL: &str = "https://www.googleapis.com/books/v1";
/// Base URL of the Open Library API.
const OPEN_LIBRARY_URL: &str = "https://openlibrary.org";
/// Base URL of the Open Library cover service.
const OPEN_LIBRARY_COVERS_URL: &str = "https://covers.openlibrary.org";

/// Timeout for a single external `HTTP` request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum accepted size, in bytes, for a downloaded cover image.
const MAX_COVER_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum cover size as a `usize` for comparing downloaded buffers.
const MAX_COVER_BYTES_USIZE: usize = 5 * 1024 * 1024;
/// Minimum plausible size, in bytes, of a real cover image. Smaller responses
/// (e.g. the Open Library 1x1 placeholder) are discarded.
const MIN_COVER_BYTES: usize = 512;

/// Enriched metadata extracted from an `EPUB` file, optionally augmented by
/// external book APIs.
#[derive(Debug, Default)]
pub struct EnrichedMetadata {
    /// A normalized ISBN, extracted from the `EPUB` identifiers.
    pub isbn: Option<String>,
    /// Genres (subjects), merging embedded `dc:subject` entries with external
    /// categories. Embedded values come first.
    pub genres: Vec<String>,
    /// A high-resolution cover image (bytes and MIME type) downloaded from an
    /// external API, if the external lookup provided one.
    pub cover: Option<(Vec<u8>, String)>,
    /// The document title.
    pub title: Option<String>,
    /// The primary author (first creator entry).
    pub author: Option<String>,
}

/// Errors returned by [`process_epub_metadata`].
#[derive(Debug)]
pub enum MetadataError {
    /// The `EPUB` file could not be read from disk.
    Io(std::io::Error),
    /// The file is not a valid `EPUB`.
    Epub(String),
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "failed to read EPUB file: {err}"),
            Self::Epub(err) => write!(f, "invalid EPUB: {err}"),
        }
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Epub(_) => None,
        }
    }
}

/// Build the shared `HTTP` client used for enrichment requests.
///
/// Returns `None` if the client cannot be constructed (e.g. `TLS`
/// initialization failure); enrichment is then effectively disabled.
pub(crate) fn build_client() -> Option<reqwest::Client> {
    match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(concat!("kosync-rs/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => Some(client),
        Err(err) => {
            tracing::error!(error = %err, "failed to build HTTP client; metadata enrichment disabled");
            None
        }
    }
}

/// Extract and enrich the metadata of an `EPUB` file.
///
/// This convenience entry point creates an `HTTP` client and allows external
/// lookups. Applications that need to control whether network access occurs
/// should use [`process_epub_metadata_with_client`] instead.
///
/// # Errors
///
/// Returns [`MetadataError::Io`] if the file cannot be read and
/// [`MetadataError::Epub`] if it is not a valid `EPUB`.
pub async fn process_epub_metadata(file_path: &Path) -> Result<EnrichedMetadata, MetadataError> {
    let client = build_client();
    process_epub_metadata_with_client(file_path, client.as_ref()).await
}

/// Extract and enrich the metadata of an `EPUB` file using an optional client.
///
/// The file is parsed locally first. When `client` is `Some`, an ISBN was
/// found, and the embedded genre list is sparse (fewer than two entries), the
/// Google Books `API` (with Open Library as a fallback) is queried by ISBN for
/// additional genres and a high-resolution cover image.
///
/// External lookups are best-effort: network or `API` failures never make the
/// extraction fail, they only limit the enrichment.
///
/// # Errors
///
/// Returns [`MetadataError::Io`] if the file cannot be read and
/// [`MetadataError::Epub`] if it is not a valid `EPUB`.
pub async fn process_epub_metadata_with_client(
    file_path: &Path,
    client: Option<&reqwest::Client>,
) -> Result<EnrichedMetadata, MetadataError> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(MetadataError::Io)?;
    let parsed =
        epub::parse(Cursor::new(data)).map_err(|err| MetadataError::Epub(err.to_string()))?;

    let mut metadata = EnrichedMetadata {
        isbn: parsed.isbn.clone(),
        genres: parsed.subjects.clone(),
        cover: None,
        title: parsed.title.clone(),
        author: parsed.authors.first().cloned(),
    };

    if metadata.genres.len() >= 2 {
        return Ok(metadata);
    }
    let (Some(client), Some(isbn)) = (client, metadata.isbn.clone()) else {
        return Ok(metadata);
    };

    if let Some(api) = lookup_by_isbn(client, &isbn).await {
        merge_genres(&mut metadata.genres, api.genres);
        if metadata.title.is_none() {
            metadata.title = api.title;
        }
        if metadata.author.is_none() {
            metadata.author = api.author;
        }
        if let Some(url) = api.cover_url {
            metadata.cover = download_cover(client, &url).await;
        }
    }

    Ok(metadata)
}

/// Enrich a stored publication in the background after import.
///
/// Fetches external metadata by ISBN when the stored genre list is sparse,
/// downloads a cover when the publication has none, and updates the database
/// row. All failures are logged and swallowed: enrichment is best-effort.
pub(crate) async fn enrich_publication(state: &AppState, document_hash: &str) {
    let Some(client) = state.http_client.as_ref() else {
        return;
    };

    let publication = match store::get_publication_by_hash(&state.pool, document_hash).await {
        Ok(Some(publication)) => publication,
        Ok(None) => return,
        Err(err) => {
            err.log();
            return;
        }
    };

    let mut genres: Vec<String> = serde_json::from_str(&publication.genres).unwrap_or_default();
    let needs_genres = genres.len() < 2;
    let needs_cover = publication.cover_path.is_none();
    if !needs_genres && !needs_cover {
        return;
    }

    let isbn = publication.isbn.clone().or_else(|| {
        publication
            .identifier
            .as_deref()
            .and_then(epub::normalize_isbn)
    });
    let Some(isbn) = isbn else {
        return;
    };

    let api = lookup_by_isbn(client, &isbn).await;

    let mut update = EnrichmentUpdate {
        genres: String::new(),
        isbn: Some(isbn),
        cover_path: None,
        cover_media_type: None,
        thumb_path: None,
        thumb_media_type: None,
        timestamp: now_unix(),
    };

    if let Some(api) = api {
        if needs_genres {
            merge_genres(&mut genres, api.genres);
        }
        if needs_cover
            && let Some(url) = api.cover_url
            && let Some((bytes, mime)) = download_cover(client, &url).await
        {
            let (cover_path, cover_media_type, thumb_path) =
                library::store_cover(state, document_hash, &bytes, &mime).await;
            update.cover_path = cover_path;
            update.cover_media_type = cover_media_type;
            update.thumb_media_type = thumb_path.as_ref().map(|_| "image/jpeg".to_owned());
            update.thumb_path = thumb_path;
        }
    }

    update.genres = serde_json::to_string(&genres).unwrap_or_else(|_| "[]".to_owned());

    match store::update_publication_enrichment(&state.pool, document_hash, &update).await {
        Ok(true) => {
            tracing::info!(document_hash, genres = ?genres, "publication metadata enriched");
        }
        Ok(false) => {}
        Err(err) => err.log(),
    }
}

/// Merge `incoming` genres into `existing`, deduplicating case-insensitively.
fn merge_genres(existing: &mut Vec<String>, incoming: Vec<String>) {
    let mut seen: HashSet<String> = existing.iter().map(|g| g.to_lowercase()).collect();
    for genre in incoming {
        let trimmed = genre.trim();
        if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
            existing.push(trimmed.to_owned());
        }
    }
}

/// Metadata fetched from an external book `API`.
#[derive(Debug, Default)]
struct ApiMetadata {
    /// Categories/genres reported by the API.
    genres: Vec<String>,
    /// URL of the highest-resolution cover image available.
    cover_url: Option<String>,
    /// The title reported by the API.
    title: Option<String>,
    /// The primary author reported by the API.
    author: Option<String>,
}

/// Look up a book by ISBN: Google Books first, Open Library as a fallback.
///
/// Returns `None` when neither API yields any usable metadata.
async fn lookup_by_isbn(client: &reqwest::Client, isbn: &str) -> Option<ApiMetadata> {
    let google = fetch_google_books(client, GOOGLE_BOOKS_URL, isbn).await;
    let open_library =
        fetch_open_library(client, OPEN_LIBRARY_URL, OPEN_LIBRARY_COVERS_URL, isbn).await;

    match (google, open_library) {
        (None, None) => None,
        (primary, fallback) => {
            let mut merged = primary.unwrap_or_default();
            if let Some(fallback) = fallback {
                merge_genres(&mut merged.genres, fallback.genres);
                if merged.cover_url.is_none() {
                    merged.cover_url = fallback.cover_url;
                }
                if merged.title.is_none() {
                    merged.title = fallback.title;
                }
                if merged.author.is_none() {
                    merged.author = fallback.author;
                }
            }
            Some(merged)
        }
    }
}

/// The Google Books volumes search response.
#[derive(Debug, Deserialize)]
struct GoogleResponse {
    /// The matching volumes.
    items: Option<Vec<GoogleItem>>,
}

/// A single Google Books volume.
#[derive(Debug, Deserialize)]
struct GoogleItem {
    /// The volume metadata.
    #[serde(rename = "volumeInfo")]
    volume_info: GoogleVolumeInfo,
}

/// Google Books volume metadata.
#[derive(Debug, Deserialize)]
struct GoogleVolumeInfo {
    /// The volume title.
    title: Option<String>,
    /// The volume authors.
    authors: Option<Vec<String>>,
    /// The volume categories (may contain `/`-separated paths).
    categories: Option<Vec<String>>,
    /// Cover image links at various resolutions.
    #[serde(rename = "imageLinks")]
    image_links: Option<GoogleImageLinks>,
}

/// Google Books cover image links, highest resolution first.
#[derive(Debug, Deserialize)]
struct GoogleImageLinks {
    /// Extra-large cover.
    #[serde(rename = "extraLarge")]
    extra_large: Option<String>,
    /// Large cover.
    large: Option<String>,
    /// Medium cover.
    medium: Option<String>,
    /// Small cover.
    small: Option<String>,
    /// Thumbnail cover.
    thumbnail: Option<String>,
    /// Small thumbnail cover.
    #[serde(rename = "smallThumbnail")]
    small_thumbnail: Option<String>,
}

impl GoogleImageLinks {
    /// The highest-resolution cover URL available, upgraded to `HTTPS`.
    fn best_url(&self) -> Option<String> {
        self.extra_large
            .as_ref()
            .or(self.large.as_ref())
            .or(self.medium.as_ref())
            .or(self.small.as_ref())
            .or(self.thumbnail.as_ref())
            .or(self.small_thumbnail.as_ref())
            .map(|url| url.replace("http://", "https://").replace("&edge=curl", ""))
    }
}

/// Query the Google Books `API` for a volume by ISBN.
async fn fetch_google_books(
    client: &reqwest::Client,
    base: &str,
    isbn: &str,
) -> Option<ApiMetadata> {
    let url = format!("{base}/volumes?q=isbn:{isbn}");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| tracing::warn!(error = %err, isbn, "Google Books request failed"))
        .ok()?
        .error_for_status()
        .map_err(|err| tracing::warn!(error = %err, isbn, "Google Books returned an error"))
        .ok()?;

    let body: GoogleResponse = response
        .json()
        .await
        .map_err(|err| tracing::warn!(error = %err, isbn, "invalid Google Books response"))
        .ok()?;

    let info = body.items?.into_iter().next()?.volume_info;

    // Google categories are often "/" -separated paths like
    // "Fiction / Fantasy / Epic"; split them into individual genres.
    let mut genres = Vec::new();
    if let Some(categories) = info.categories {
        for category in categories {
            merge_genres(
                &mut genres,
                category
                    .split('/')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect(),
            );
        }
    }

    Some(ApiMetadata {
        genres,
        cover_url: info.image_links.and_then(|links| links.best_url()),
        title: info.title,
        author: info.authors.and_then(|authors| authors.into_iter().next()),
    })
}

/// An Open Library subject, either a plain string or an object with a name.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OpenLibrarySubject {
    /// A plain subject string.
    Plain(String),
    /// A structured subject entry.
    Detailed {
        /// The subject name.
        name: String,
    },
}

/// The Open Library edition response.
#[derive(Debug, Deserialize)]
struct OpenLibraryBook {
    /// The edition title.
    title: Option<String>,
    /// The edition subjects.
    subjects: Option<Vec<OpenLibrarySubject>>,
    /// A free-text authorship statement (e.g. "by Ursula K. Le Guin").
    by_statement: Option<String>,
}

/// Query the Open Library `API` for an edition by ISBN.
async fn fetch_open_library(
    client: &reqwest::Client,
    base: &str,
    covers_base: &str,
    isbn: &str,
) -> Option<ApiMetadata> {
    let url = format!("{base}/isbn/{isbn}.json");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| tracing::warn!(error = %err, isbn, "Open Library request failed"))
        .ok()?
        .error_for_status()
        .map_err(|err| tracing::warn!(error = %err, isbn, "Open Library returned an error"))
        .ok()?;

    let book: OpenLibraryBook = response
        .json()
        .await
        .map_err(|err| tracing::warn!(error = %err, isbn, "invalid Open Library response"))
        .ok()?;

    let genres: Vec<String> = book
        .subjects
        .unwrap_or_default()
        .into_iter()
        .map(|subject| match subject {
            OpenLibrarySubject::Plain(name) | OpenLibrarySubject::Detailed { name } => name,
        })
        .collect();

    Some(ApiMetadata {
        genres,
        cover_url: Some(format!("{covers_base}/b/isbn/{isbn}-L.jpg")),
        title: book.title,
        author: book
            .by_statement
            .map(|s| s.trim().trim_start_matches("by ").trim().to_owned())
            .filter(|s| !s.is_empty()),
    })
}

/// Download a cover image, enforcing size and content-type limits.
///
/// Returns the image bytes and MIME type, or `None` if the response is not a
/// plausible cover image.
async fn download_cover(client: &reqwest::Client, url: &str) -> Option<(Vec<u8>, String)> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| tracing::warn!(error = %err, url, "cover download failed"))
        .ok()?
        .error_for_status()
        .map_err(|err| tracing::warn!(error = %err, url, "cover download returned an error"))
        .ok()?;

    if response
        .content_length()
        .is_some_and(|len| len > MAX_COVER_BYTES)
    {
        tracing::warn!(url, "cover image exceeds size limit");
        return None;
    }

    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned()
        })
        .filter(|value| value.starts_with("image/"))
        .or_else(|| mime_guess::from_path(url).first_raw().map(str::to_owned))?;

    let bytes = response
        .bytes()
        .await
        .map_err(|err| tracing::warn!(error = %err, url, "failed to read cover bytes"))
        .ok()?;

    if bytes.len() > MAX_COVER_BYTES_USIZE {
        tracing::warn!(url, "cover image exceeds size limit");
        return None;
    }
    if bytes.len() < MIN_COVER_BYTES {
        tracing::debug!(url, "discarding implausibly small cover image");
        return None;
    }

    Some((bytes.to_vec(), mime))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Start a local `HTTP` server serving `body` for any request, returning
    /// its base URL.
    async fn mock_server(content_type: &'static str, body: &'static str) -> String {
        use axum::routing::get;

        let app = axum::Router::new().fallback(get(move || async move {
            ([(axum::http::header::CONTENT_TYPE, content_type)], body)
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    const GOOGLE_FIXTURE: &str = r#"{
        "items": [{
            "volumeInfo": {
                "title": "The Left Hand of Darkness",
                "authors": ["Ursula K. Le Guin"],
                "categories": ["Fiction / Science Fiction / General", "Fiction / Literary"],
                "imageLinks": {
                    "smallThumbnail": "http://books.google.com/small.jpg",
                    "thumbnail": "http://books.google.com/thumb.jpg?zoom=1&edge=curl",
                    "large": "http://books.google.com/large.jpg"
                }
            }
        }]
    }"#;

    #[tokio::test]
    async fn google_books_response_parses_categories_and_best_cover() {
        let base = mock_server("application/json", GOOGLE_FIXTURE).await;
        let client = build_client().unwrap();
        let metadata = fetch_google_books(&client, &base, "9780441478125")
            .await
            .unwrap();

        assert_eq!(
            metadata.genres,
            vec!["Fiction", "Science Fiction", "General", "Literary"]
        );
        assert_eq!(
            metadata.cover_url.as_deref(),
            Some("https://books.google.com/large.jpg")
        );
        assert_eq!(metadata.title.as_deref(), Some("The Left Hand of Darkness"));
        assert_eq!(metadata.author.as_deref(), Some("Ursula K. Le Guin"));
    }

    #[tokio::test]
    async fn google_books_empty_result_yields_none() {
        let base = mock_server("application/json", r#"{"totalItems": 0}"#).await;
        let client = build_client().unwrap();
        assert!(
            fetch_google_books(&client, &base, "9780441478125")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn google_books_http_error_yields_none() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().fallback(axum::routing::get(|| async {
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                })),
            )
            .await
            .unwrap();
        });
        let client = build_client().unwrap();
        assert!(
            fetch_google_books(&client, &format!("http://{addr}"), "9780441478125")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn open_library_parses_plain_and_structured_subjects() {
        let fixture = r#"{
            "title": "The Left Hand of Darkness",
            "by_statement": "by Ursula K. Le Guin",
            "subjects": ["Science fiction", {"name": "Gethen (Imaginary place)", "url": ""}]
        }"#;
        let base = mock_server("application/json", fixture).await;
        let client = build_client().unwrap();
        let metadata = fetch_open_library(&client, &base, "http://covers.test", "9780441478125")
            .await
            .unwrap();

        assert_eq!(
            metadata.genres,
            vec!["Science fiction", "Gethen (Imaginary place)"]
        );
        assert_eq!(metadata.author.as_deref(), Some("Ursula K. Le Guin"));
        assert_eq!(
            metadata.cover_url.as_deref(),
            Some("http://covers.test/b/isbn/9780441478125-L.jpg")
        );
    }

    #[tokio::test]
    async fn download_cover_rejects_tiny_placeholder() {
        let base = mock_server("image/jpeg", "x").await;
        let client = build_client().unwrap();
        assert!(download_cover(&client, &base).await.is_none());
    }

    #[tokio::test]
    async fn download_cover_accepts_plausible_image() {
        let body = "x".repeat(1024);
        let base = mock_server("image/jpeg", Box::leak(body.into_boxed_str())).await;
        let client = build_client().unwrap();
        let (bytes, mime) = download_cover(&client, &base).await.unwrap();
        assert_eq!(bytes.len(), 1024);
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn merge_genres_deduplicates_case_insensitively() {
        let mut genres = vec!["Fantasy".to_owned()];
        merge_genres(
            &mut genres,
            vec![" fantasy ".to_owned(), "Epic".to_owned(), " ".to_owned()],
        );
        assert_eq!(genres, vec!["Fantasy", "Epic"]);
    }
}
