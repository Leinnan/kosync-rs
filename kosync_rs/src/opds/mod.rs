//! OPDS 2.0 (JSON-LD) and OPDS 1.x (Atom) catalog, plus EPUB
//! upload/download and cover serving for the shared library.

#![allow(
    clippy::result_large_err,
    reason = "Axum handlers return complete HTTP responses for authentication and storage failures; boxing them adds allocation without a practical benefit."
)]

mod atom;
mod upload;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    basic_auth,
    error::ApiError,
    state::AppState,
    store::{self, Document, Publication, StoreError},
    util::now_unix,
};

/// OPDS 2.0 JSON-LD context for feeds.
const FEED_CONTEXT: [&str; 2] = [
    "https://readium.org/webpub-manifest/context.jsonld",
    "https://opds-spec.org/context.jsonld",
];

/// OPDS 2.0 JSON-LD context for a single publication.
const PUBLICATION_CONTEXT: &str = "https://readium.org/webpub-manifest/context.jsonld";

/// Media type for OPDS 2.0 feeds.
const OPDS_JSON_TYPE: &str = "application/opds+json";
/// Media type for a single OPDS publication.
const OPDS_PUBLICATION_TYPE: &str = "application/opds-publication+json";
/// Media type for `EPUB` acquisition files.
const EPUB_TYPE: &str = "application/epub+zip";

/// Number of publications returned per feed page.
const PER_PAGE: i64 = 20;

/// Build the OPDS router for the given maximum upload size.
pub(crate) fn router(max_upload_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/opds/v2/", get(nav_feed))
        .route("/opds/v2/publications", get(publication_feed))
        .route("/opds/v2/authors", get(author_navigation_feed))
        .route("/opds/v2/authors/{name}", get(author_publication_feed))
        .route("/opds/v2/genres", get(genre_navigation_feed))
        .route("/opds/v2/genres/{name}", get(genre_publication_feed))
        .route("/opds/v2/series", get(series_navigation_feed))
        .route("/opds/v2/series/{name}", get(series_publication_feed))
        .route(
            "/opds/v2/publications/{document_hash}",
            get(publication_detail).delete(upload::delete_publication),
        )
        .route("/opds/v2/publications/{document_hash}/file", get(download))
        .route("/opds/v2/publications/{document_hash}/cover", get(cover))
        .route(
            "/opds/v2/publications/{document_hash}/thumbnail",
            get(thumbnail),
        )
        .route("/opds/", get(atom::nav_feed))
        .route("/opds", get(atom::nav_feed))
        .route("/opds.xml", get(atom::nav_feed))
        .route("/opds/publications", get(atom::publication_feed))
        .route("/opds/authors", get(atom::author_navigation_feed))
        .route("/opds/authors/{name}", get(atom::author_publication_feed))
        .route("/opds/genres", get(atom::genre_navigation_feed))
        .route("/opds/genres/{name}", get(atom::genre_publication_feed))
        .route("/opds/series", get(atom::series_navigation_feed))
        .route("/opds/series/{name}", get(atom::series_publication_feed))
        .route(
            "/opds/upload",
            axum::routing::post(upload::upload)
                .layer(axum::extract::DefaultBodyLimit::max(max_upload_bytes)),
        )
}

/// A single OPDS link.
#[derive(Debug, Serialize)]
struct OpdsLink {
    rel: String,
    href: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    typ: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

/// An author entry in a publication's metadata.
#[derive(Debug, Serialize)]
struct Author {
    name: String,
}

/// A subject entry in a publication's metadata.
#[derive(Debug, Serialize)]
struct Subject {
    name: String,
}

/// Series membership in a Web Publication Manifest.
#[derive(Debug, Serialize)]
struct BelongsTo {
    series: Series,
}

/// A named series and optional position within it.
#[derive(Debug, Serialize)]
struct Series {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<f64>,
}

/// Overlaid `KOReader` sync progress for a publication.
#[derive(Debug, Serialize)]
struct ProgressOverlay {
    document_hash: String,
    percentage: f64,
    progress: String,
    device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    timestamp: i64,
}

impl From<&Document> for ProgressOverlay {
    fn from(doc: &Document) -> Self {
        Self {
            document_hash: doc.document_hash.clone(),
            percentage: doc.percentage,
            progress: doc.progress.clone(),
            device: doc.device.clone(),
            device_id: doc.device_id.clone(),
            timestamp: doc.timestamp,
        }
    }
}

/// Publication metadata for a Web Publication Manifest.
#[derive(Debug, Serialize)]
struct PublicationMetadata {
    #[serde(rename = "@type")]
    typ: &'static str,
    title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    author: Vec<Author>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    subject: Vec<Subject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    belongs_to: Option<BelongsTo>,
    modified: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    kosync: Option<ProgressOverlay>,
}

/// A single OPDS publication (Web Publication Manifest).
#[derive(Debug, Serialize)]
struct OpdsPublication {
    #[serde(rename = "@context")]
    context: &'static str,
    metadata: PublicationMetadata,
    links: Vec<OpdsLink>,
}

/// Feed-level metadata.
#[derive(Debug, Serialize)]
struct FeedMetadata {
    title: String,
    #[serde(rename = "numberOfItems")]
    number_of_items: i64,
    #[serde(rename = "itemsPerPage")]
    items_per_page: i64,
    #[serde(rename = "currentPage")]
    current_page: i64,
    modified: String,
}

/// An OPDS 2.0 publication feed.
#[derive(Debug, Serialize)]
struct OpdsFeed {
    #[serde(rename = "@context")]
    context: Vec<&'static str>,
    metadata: FeedMetadata,
    links: Vec<OpdsLink>,
    publications: Vec<OpdsPublication>,
}

/// Pagination query parameters for feeds.
#[derive(Debug, Deserialize)]
struct PageQuery {
    #[serde(default)]
    page: Option<i64>,
    #[serde(default)]
    query: Option<String>,
}

/// Convert an [`ApiError`] into a response, advertising Basic auth on 401.
fn auth_error(err: ApiError) -> Response {
    let status = err.status();
    let mut response = err.into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"kosync\""),
        );
    }
    response
}

/// Authenticate the request using `HTTP Basic`, or return an error response.
async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<store::User, Response> {
    basic_auth::authenticate(&state.pool, headers)
        .await
        .map_err(auth_error)
}

/// Authenticate an administrator using `HTTP Basic`, or return an error response.
async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<store::User, Response> {
    basic_auth::require_admin(&state.pool, headers)
        .await
        .map_err(auth_error)
}

/// Authenticate a user permitted to upload books using `HTTP Basic`, or return
/// an error response. Administrators and users with the `can_upload`
/// permission are allowed.
async fn require_uploader(state: &AppState, headers: &HeaderMap) -> Result<store::User, Response> {
    basic_auth::require_uploader(&state.pool, headers)
        .await
        .map_err(auth_error)
}

/// Resolve the scheme and host of the incoming request for absolute links.
fn base_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("{scheme}://{host}")
}

/// Format a Unix timestamp as an RFC 3339 UTC string.
fn rfc3339(timestamp: i64) -> String {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()
        .and_then(|dt| dt.format(&Rfc3339).ok())
        .unwrap_or_default()
}

/// Look up the requesting user's sync progress for a publication, matching on
/// the binary digest first and the file-name digest as a fallback.
async fn progress_for(
    state: &AppState,
    user_id: i64,
    publication: &Publication,
) -> Option<Document> {
    store::get_progress_for_publication(&state.pool, user_id, publication)
        .await
        .ok()
        .flatten()
}

/// Build an OPDS publication view with links and optional progress overlay.
fn publication_view(
    base: &str,
    publication: &Publication,
    progress: Option<&Document>,
) -> OpdsPublication {
    let authors = parse_authors(&publication.authors);
    let subjects = parse_subjects(&publication.genres);

    let mut links = vec![
        OpdsLink {
            rel: "self".to_owned(),
            href: format!("{base}/opds/v2/publications/{}", publication.document_hash),
            typ: Some(OPDS_PUBLICATION_TYPE.to_owned()),
            title: None,
        },
        OpdsLink {
            rel: "http://opds-spec.org/acquisition".to_owned(),
            href: format!(
                "{base}/opds/v2/publications/{}/file",
                publication.document_hash
            ),
            typ: Some(EPUB_TYPE.to_owned()),
            title: None,
        },
    ];

    if publication.cover_path.is_some() {
        links.push(OpdsLink {
            rel: "http://opds-spec.org/image".to_owned(),
            href: format!(
                "{base}/opds/v2/publications/{}/cover",
                publication.document_hash
            ),
            typ: publication.cover_media_type.clone(),
            title: None,
        });
    }

    if publication.thumb_path.is_some() {
        links.push(OpdsLink {
            rel: "http://opds-spec.org/image/thumbnail".to_owned(),
            href: format!(
                "{base}/opds/v2/publications/{}/thumbnail",
                publication.document_hash
            ),
            typ: publication.thumb_media_type.clone(),
            title: None,
        });
    }

    OpdsPublication {
        context: PUBLICATION_CONTEXT,
        metadata: PublicationMetadata {
            typ: "http://schema.org/Book",
            title: publication.title.clone(),
            author: authors,
            subject: subjects,
            language: publication.language.clone(),
            identifier: publication.identifier.clone(),
            publisher: publication.publisher.clone(),
            published: publication.published.clone(),
            description: publication.description.clone(),
            belongs_to: publication.series.as_ref().map(|name| BelongsTo {
                series: Series {
                    name: name.clone(),
                    position: publication.series_index,
                },
            }),
            modified: rfc3339(publication.updated_at),
            kosync: progress.map(ProgressOverlay::from),
        },
        links,
    }
}

/// Parse the stored authors JSON array into author entries.
fn parse_authors(authors: &str) -> Vec<Author> {
    serde_json::from_str::<Vec<String>>(authors)
        .unwrap_or_default()
        .into_iter()
        .map(|name| Author { name })
        .collect()
}

/// Parse the stored genres JSON array into OPDS subject entries.
fn parse_subjects(genres: &str) -> Vec<Subject> {
    serde_json::from_str::<Vec<String>>(genres)
        .unwrap_or_default()
        .into_iter()
        .map(|name| Subject { name })
        .collect()
}

/// Render the OPDS 2.0 root navigation feed.
async fn nav_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Response> {
    let _user = require_user(&state, &headers).await?;
    let base = base_url(&headers);

    let body = serde_json::json!({
        "@context": FEED_CONTEXT,
        "metadata": {
            "title": "kosync-rs library",
            "modified": rfc3339(now_unix()),
        },
        "links": [
            { "rel": "self", "href": format!("{base}/opds/v2/"), "type": OPDS_JSON_TYPE },
        ],
        "navigation": [
            {
                "title": "All books",
                "href": format!("{base}/opds/v2/publications"),
                "type": OPDS_JSON_TYPE,
            },
            {
                "title": "Authors",
                "href": format!("{base}/opds/v2/authors"),
                "type": OPDS_JSON_TYPE,
            },
            {
                "title": "Genres",
                "href": format!("{base}/opds/v2/genres"),
                "type": OPDS_JSON_TYPE,
            },
            {
                "title": "Series",
                "href": format!("{base}/opds/v2/series"),
                "type": OPDS_JSON_TYPE,
            },
        ],
    });

    Ok(Json(body))
}

/// Render the OPDS 2.0 publication feed.
async fn publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Json<OpdsFeed>, Response> {
    let user = require_user(&state, &headers).await?;
    let base = base_url(&headers);

    let search = query.query.as_deref().filter(|q| !q.trim().is_empty());
    let total = store::count_publications(&state.pool, search)
        .await
        .map_err(|e| store_error(&e))?;
    let page = page_for(total, query.page);
    let offset = (page - 1) * PER_PAGE;

    let publications = store::list_publications_paged(&state.pool, PER_PAGE, offset, search)
        .await
        .map_err(|e| store_error(&e))?;

    Ok(Json(
        acquisition_feed(
            &state,
            user.id,
            &base,
            "kosync-rs library".to_owned(),
            "/opds/v2/publications",
            search,
            total,
            page,
            publications,
        )
        .await,
    ))
}

/// Render the OPDS 2.0 author navigation feed.
async fn author_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Response> {
    let _user = require_user(&state, &headers).await?;
    let groups = store::list_authors(&state.pool)
        .await
        .map_err(|err| store_error(&err))?;
    Ok(Json(group_navigation(
        &base_url(&headers),
        "Authors",
        "authors",
        groups,
    )))
}

/// Render the OPDS 2.0 genre navigation feed.
async fn genre_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Response> {
    let _user = require_user(&state, &headers).await?;
    let groups = store::list_genres(&state.pool)
        .await
        .map_err(|err| store_error(&err))?;
    Ok(Json(group_navigation(
        &base_url(&headers),
        "Genres",
        "genres",
        groups,
    )))
}

/// Render the OPDS 2.0 series navigation feed.
async fn series_navigation_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Response> {
    let _user = require_user(&state, &headers).await?;
    let groups = store::list_series(&state.pool)
        .await
        .map_err(|err| store_error(&err))?;
    Ok(Json(group_navigation(
        &base_url(&headers),
        "Series",
        "series",
        groups,
    )))
}

/// Render the OPDS 2.0 acquisition feed for a named author.
async fn author_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<OpdsFeed>, Response> {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Author,
        "Author",
        "authors",
    )
    .await
}

/// Render the OPDS 2.0 acquisition feed for a named genre.
async fn genre_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<OpdsFeed>, Response> {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Genre,
        "Genre",
        "genres",
    )
    .await
}

/// Render the OPDS 2.0 acquisition feed for a named series.
async fn series_publication_feed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(query): Query<PageQuery>,
) -> Result<Json<OpdsFeed>, Response> {
    grouped_publication_feed(
        state,
        headers,
        name,
        query,
        store::GroupKind::Series,
        "Series",
        "series",
    )
    .await
}

/// Render a navigation feed containing named publication groups.
fn group_navigation(
    base: &str,
    title: &str,
    segment: &str,
    groups: Vec<store::GroupCount>,
) -> Value {
    let endpoint = format!("/opds/v2/{segment}");
    let navigation: Vec<Value> = groups
        .into_iter()
        .map(|group| {
            serde_json::json!({
                "title": group.name,
                "href": format!("{base}{endpoint}/{}", percent_encode_path_segment(&group.name)),
                "type": OPDS_JSON_TYPE,
                "properties": { "numberOfItems": group.count },
            })
        })
        .collect();

    serde_json::json!({
        "@context": FEED_CONTEXT,
        "metadata": {
            "title": format!("kosync-rs library: {title}"),
            "numberOfItems": navigation.len(),
            "modified": rfc3339(now_unix()),
        },
        "links": [
            { "rel": "self", "href": format!("{base}{endpoint}"), "type": OPDS_JSON_TYPE },
        ],
        "navigation": navigation,
    })
}

/// Render the acquisition feed for one author, genre, or series.
async fn grouped_publication_feed(
    state: AppState,
    headers: HeaderMap,
    name: String,
    query: PageQuery,
    kind: store::GroupKind,
    label: &str,
    segment: &str,
) -> Result<Json<OpdsFeed>, Response> {
    let user = require_user(&state, &headers).await?;
    if name.trim().is_empty() {
        return Err(not_found());
    }

    let total = store::count_publications_in_group(&state.pool, kind, &name)
        .await
        .map_err(|err| store_error(&err))?;
    if total == 0 {
        return Err(not_found());
    }

    let page = page_for(total, query.page);
    let publications = store::list_publications_in_group(
        &state.pool,
        kind,
        &name,
        PER_PAGE,
        (page - 1) * PER_PAGE,
    )
    .await
    .map_err(|err| store_error(&err))?;

    let base = base_url(&headers);
    let endpoint = format!("/opds/v2/{segment}/{}", percent_encode_path_segment(&name));
    Ok(Json(
        acquisition_feed(
            &state,
            user.id,
            &base,
            format!("{label}: {name}"),
            &endpoint,
            None,
            total,
            page,
            publications,
        )
        .await,
    ))
}

/// Build a paginated OPDS acquisition feed from a page of publications.
#[allow(clippy::too_many_arguments)]
async fn acquisition_feed(
    state: &AppState,
    user_id: i64,
    base: &str,
    title: String,
    endpoint: &str,
    search: Option<&str>,
    total: i64,
    page: i64,
    publications: Vec<Publication>,
) -> OpdsFeed {
    let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    let mut views = Vec::with_capacity(publications.len());
    for publication in &publications {
        let progress = progress_for(state, user_id, publication).await;
        views.push(publication_view(base, publication, progress.as_ref()));
    }

    let mut links = vec![OpdsLink {
        rel: "self".to_owned(),
        href: page_href(base, endpoint, page, search),
        typ: Some(OPDS_JSON_TYPE.to_owned()),
        title: None,
    }];
    if page > 1 {
        links.push(OpdsLink {
            rel: "prev".to_owned(),
            href: page_href(base, endpoint, page - 1, search),
            typ: Some(OPDS_JSON_TYPE.to_owned()),
            title: None,
        });
    }
    if page < total_pages {
        links.push(OpdsLink {
            rel: "next".to_owned(),
            href: page_href(base, endpoint, page + 1, search),
            typ: Some(OPDS_JSON_TYPE.to_owned()),
            title: None,
        });
    }

    OpdsFeed {
        context: FEED_CONTEXT.to_vec(),
        metadata: FeedMetadata {
            title,
            number_of_items: total,
            items_per_page: PER_PAGE,
            current_page: page,
            modified: rfc3339(now_unix()),
        },
        links,
        publications: views,
    }
}

/// Clamp a requested page to the valid range for `total` publications.
fn page_for(total: i64, requested: Option<i64>) -> i64 {
    let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    requested.unwrap_or(1).max(1).min(total_pages)
}

/// Build an absolute paginated feed URL, retaining an optional search query.
fn page_href(base: &str, endpoint: &str, page: i64, search: Option<&str>) -> String {
    match search {
        Some(query) => format!(
            "{base}{endpoint}?page={page}&query={}",
            percent_encode_path_segment(query)
        ),
        None => format!("{base}{endpoint}?page={page}"),
    }
}

/// Percent-encode a value for safe use as one URL path segment or query value.
fn percent_encode_path_segment(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    encoded
}

/// Render a single OPDS publication manifest.
async fn publication_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document_hash): Path<String>,
) -> Result<Json<OpdsPublication>, Response> {
    let user = require_user(&state, &headers).await?;
    let base = base_url(&headers);

    let publication = store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .map_err(|e| store_error(&e))?
        .ok_or_else(not_found)?;

    let progress = progress_for(&state, user.id, &publication).await;

    Ok(Json(publication_view(
        &base,
        &publication,
        progress.as_ref(),
    )))
}

/// Stream an `EPUB` file to the client.
async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document_hash): Path<String>,
) -> Response {
    let _user = match require_user(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let publication = match store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .map_err(|e| store_error(&e))
    {
        Ok(Some(publication)) => publication,
        Ok(None) => return not_found(),
        Err(response) => return response,
    };

    let mut response = serve_file(&state, &publication.file_path, EPUB_TYPE).await;
    let name = download_name(&publication);
    let value = format!(
        "attachment; filename=\"{}\"",
        crate::library::sanitize_filename(&name)
    );
    if let Ok(value) = HeaderValue::from_str(&value) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    response
}

/// Serve a stored cover image.
async fn cover(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document_hash): Path<String>,
) -> Response {
    serve_publication_image(&state, &headers, &document_hash, false).await
}

/// Serve a generated cover thumbnail.
async fn thumbnail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(document_hash): Path<String>,
) -> Response {
    serve_publication_image(&state, &headers, &document_hash, true).await
}

/// Serve the cover or thumbnail for a publication.
async fn serve_publication_image(
    state: &AppState,
    headers: &HeaderMap,
    document_hash: &str,
    thumbnail: bool,
) -> Response {
    let _user = match require_user(state, headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    let publication = match store::get_publication_by_hash(&state.pool, document_hash)
        .await
        .map_err(|e| store_error(&e))
    {
        Ok(Some(publication)) => publication,
        Ok(None) => return not_found(),
        Err(response) => return response,
    };

    let (path, media_type) = if thumbnail {
        (
            publication.thumb_path.as_deref(),
            publication.thumb_media_type.as_deref(),
        )
    } else {
        (
            publication.cover_path.as_deref(),
            publication.cover_media_type.as_deref(),
        )
    };

    let (Some(path), Some(media_type)) = (path, media_type) else {
        return not_found();
    };

    serve_file(state, path, media_type).await
}

/// Stream a stored file as an HTTP response.
async fn serve_file(state: &AppState, path: &str, media_type: &str) -> Response {
    match crate::library::file_body(state, path).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, media_type.to_owned()),
                (header::CACHE_CONTROL, "public, max-age=3600".to_owned()),
            ],
            body,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, path, "failed to read stored file");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "message": "Failed to read file" })),
            )
                .into_response()
        }
    }
}

/// Build a download file name from a publication title.
fn download_name(publication: &Publication) -> String {
    let base = if publication.title.trim().is_empty() {
        publication.document_hash.clone()
    } else {
        publication.title.clone()
    };
    format!("{base}.epub")
}

/// Map a storage error into an internal server error response.
fn store_error(err: &StoreError) -> Response {
    err.log();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "message": "Internal server error" })),
    )
        .into_response()
}

/// A `404` not-found response for missing publications or files.
fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "message": "Not found" })),
    )
        .into_response()
}
