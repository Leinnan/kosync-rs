//! Shared EPUB library pages: listing, detail, cover, download, and delete.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use tower_cookies::Cookies;

use crate::{library, state::AppState, store};

use super::{
    authors_string, base_context, cover_hue, current_user, format_size, format_timestamp, is_htmx,
    pluralize, render,
};

/// Number of books shown per page.
const PER_PAGE: i64 = 20;

/// Library listing query parameters.
#[derive(Debug, Deserialize)]
pub(super) struct BookQuery {
    page: Option<i64>,
    query: Option<String>,
    author: Option<String>,
    genre: Option<String>,
    series: Option<String>,
    language: Option<String>,
    status: Option<String>,
    sort: Option<String>,
    view: Option<String>,
}

/// A book card prepared for rendering in the listing grid.
#[derive(Debug, Serialize)]
struct BookCardView {
    document_hash: String,
    title: String,
    authors: String,
    series: String,
    /// Display position within the series, e.g. `2` or `2.5`.
    series_index: String,
    has_thumb: bool,
    has_progress: bool,
    percentage: f64,
    percent: String,
    /// Hue of the generated placeholder cover.
    hue: u16,
}

/// An active filter shown as a removable chip.
#[derive(Debug, Serialize)]
struct FilterChip {
    label: String,
    href: String,
}

/// Resolved, validated library listing filters.
#[derive(Debug)]
struct Filters {
    query: String,
    author: String,
    genre: String,
    series: String,
    language: String,
    status: String,
    sort: String,
    view: String,
}

impl Filters {
    /// Normalize raw query parameters into validated filters.
    fn from_query(query: &BookQuery) -> Self {
        Self {
            query: clean(query.query.as_deref()),
            author: clean(query.author.as_deref()),
            genre: clean(query.genre.as_deref()),
            series: clean(query.series.as_deref()),
            language: clean(query.language.as_deref()),
            status: normalize_status(query.status.as_deref()),
            sort: normalize_sort(query.sort.as_deref()),
            view: normalize_view(query.view.as_deref()),
        }
    }

    /// The store-level reading status.
    fn store_status(&self) -> store::ReadingStatus {
        match self.status.as_str() {
            "unread" => store::ReadingStatus::Unread,
            "in-progress" => store::ReadingStatus::InProgress,
            "finished" => store::ReadingStatus::Finished,
            _ => store::ReadingStatus::Any,
        }
    }

    /// The store-level sort order.
    fn store_sort(&self) -> store::PublicationSort {
        match self.sort.as_str() {
            "author" => store::PublicationSort::Author,
            "recent" => store::PublicationSort::Recent,
            "series" => store::PublicationSort::Series,
            _ => store::PublicationSort::Title,
        }
    }

    /// Non-empty `(name, value)` pairs that should be preserved in links.
    fn pairs(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = Vec::new();
        for (key, value) in [
            ("query", self.query.as_str()),
            ("author", self.author.as_str()),
            ("genre", self.genre.as_str()),
            ("series", self.series.as_str()),
            ("language", self.language.as_str()),
            ("status", self.status.as_str()),
        ] {
            if !value.is_empty() {
                pairs.push((key, value));
            }
        }
        if self.sort != "title" {
            pairs.push(("sort", self.sort.as_str()));
        }
        if self.view != "grid" {
            pairs.push(("view", self.view.as_str()));
        }
        pairs
    }

    /// A `&`-prefixed query suffix for pagination links, or an empty string.
    fn suffix(&self) -> String {
        let pairs = self.pairs();
        if pairs.is_empty() {
            return String::new();
        }
        let encoded: Vec<String> = pairs
            .iter()
            .map(|(key, value)| format!("{key}={}", url_encode(value)))
            .collect();
        format!("&{}", encoded.join("&"))
    }

    /// A `/books` URL with one filter removed.
    fn href_without(&self, omit: &str) -> String {
        let encoded: Vec<String> = self
            .pairs()
            .iter()
            .filter(|(key, _)| *key != omit)
            .map(|(key, value)| format!("{key}={}", url_encode(value)))
            .collect();
        if encoded.is_empty() {
            "/books".to_owned()
        } else {
            format!("/books?{}", encoded.join("&"))
        }
    }

    /// Number of active filters that live in the collapsible filter panel.
    fn advanced_count(&self) -> usize {
        [
            &self.author,
            &self.genre,
            &self.series,
            &self.language,
            &self.status,
        ]
        .iter()
        .filter(|value| !value.is_empty())
        .count()
    }

    /// The active filters as removable chips.
    fn chips(&self) -> Vec<FilterChip> {
        let mut chips = Vec::new();
        let mut push = |label: String, omit: &str| {
            chips.push(FilterChip {
                label,
                href: self.href_without(omit),
            });
        };
        if !self.query.is_empty() {
            push(format!("Search: {}", self.query), "query");
        }
        if !self.author.is_empty() {
            push(format!("Author: {}", self.author), "author");
        }
        if !self.genre.is_empty() {
            push(format!("Genre: {}", self.genre), "genre");
        }
        if !self.series.is_empty() {
            push(format!("Series: {}", self.series), "series");
        }
        if !self.language.is_empty() {
            push(format!("Language: {}", self.language), "language");
        }
        if !self.status.is_empty() {
            push(format!("Status: {}", status_label(&self.status)), "status");
        }
        chips
    }
}

/// Trim, truncate, and reject empty search input.
fn clean(value: Option<&str>) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(120).collect())
        .unwrap_or_default()
}

/// Normalize a reading-status query parameter.
fn normalize_status(value: Option<&str>) -> String {
    match value {
        Some("unread") => "unread".to_owned(),
        Some("in-progress") => "in-progress".to_owned(),
        Some("finished") => "finished".to_owned(),
        _ => String::new(),
    }
}

/// A human-readable label for a reading-status value.
fn status_label(value: &str) -> &'static str {
    match value {
        "unread" => "Unread",
        "in-progress" => "In progress",
        "finished" => "Finished",
        _ => "Any",
    }
}

/// Normalize a sort query parameter.
fn normalize_sort(value: Option<&str>) -> String {
    match value {
        Some("author") => "author".to_owned(),
        Some("recent") => "recent".to_owned(),
        Some("series") => "series".to_owned(),
        _ => "title".to_owned(),
    }
}

/// Format a series position for display (`2.0` → `2`, `2.5` → `2.5`).
fn series_index_label(index: Option<f64>) -> String {
    index.map_or_else(String::new, |index| format!("{index}"))
}

/// Normalize a view query parameter.
fn normalize_view(value: Option<&str>) -> String {
    match value {
        Some("list") => "list".to_owned(),
        _ => "grid".to_owned(),
    }
}

/// A book detail view prepared for rendering in the detail template.
#[derive(Debug, Serialize)]
struct BookDetailView {
    document_hash: String,
    title: String,
    authors: String,
    series: Option<String>,
    genres: Vec<String>,
    language: Option<String>,
    identifier: Option<String>,
    publisher: Option<String>,
    published: Option<String>,
    description: Option<String>,
    file_size: String,
    created: String,
    updated: String,
    has_cover: bool,
    has_progress: bool,
    percentage: f64,
    percent: String,
    progress: String,
    device: String,
    synced: String,
    /// Hash of the user's synced document, linking to its sync history.
    history_hash: Option<String>,
    series_index: String,
    isbn: Option<String>,
    hue: u16,
}

/// Maximum number of sibling books shown in the "More in this series" rail.
const SERIES_LIMIT: i64 = 30;

/// Render the paginated, filtered book listing.
pub(super) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<BookQuery>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let filters = Filters::from_query(&query);
    let filter = store::PublicationFilter {
        search: non_empty(&filters.query),
        author: non_empty(&filters.author),
        genre: non_empty(&filters.genre),
        series: non_empty(&filters.series),
        language: non_empty(&filters.language),
        status: filters.store_status(),
        sort: filters.store_sort(),
        user_id: user.id,
    };

    let total = store::count_publications_filtered(&state.pool, &filter)
        .await
        .unwrap_or(0);
    let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);
    let page = query.page.unwrap_or(1).max(1).min(total_pages);
    let offset = (page - 1) * PER_PAGE;

    let publications = store::list_publications_filtered(&state.pool, &filter, PER_PAGE, offset)
        .await
        .unwrap_or_default();

    let mut books = Vec::with_capacity(publications.len());
    for publication in &publications {
        books.push(card_view(&state, user.id, publication).await);
    }

    let mut ctx = base_context(&user, "books");
    ctx.insert("books", &books);
    ctx.insert("total", &total);
    ctx.insert("total_label", &pluralize(total, "book", "books"));
    ctx.insert("page", &page);
    ctx.insert("total_pages", &total_pages);
    ctx.insert("has_prev", &(page > 1));
    ctx.insert("has_next", &(page < total_pages));
    ctx.insert("query", &filters.query);
    ctx.insert("author", &filters.author);
    ctx.insert("genre", &filters.genre);
    ctx.insert("series", &filters.series);
    ctx.insert("language", &filters.language);
    ctx.insert("status", &filters.status);
    ctx.insert("sort", &filters.sort);
    ctx.insert("view", &filters.view);
    ctx.insert("query_suffix", &filters.suffix());
    ctx.insert("clear_url", &"/books");
    ctx.insert("active_filters", &filters.chips());
    ctx.insert("advanced_filter_count", &filters.advanced_count());

    // Facet options for the filter toolbar.
    let author_options = store::list_authors(&state.pool).await.unwrap_or_default();
    let genre_options = store::list_genres(&state.pool).await.unwrap_or_default();
    let series_options = store::list_series(&state.pool).await.unwrap_or_default();
    let language_options = store::list_languages(&state.pool).await.unwrap_or_default();
    ctx.insert("author_options", &author_options);
    ctx.insert("genre_options", &genre_options);
    ctx.insert("series_options", &series_options);
    ctx.insert("language_options", &language_options);

    if page > 1 {
        ctx.insert("prev_page", &(page - 1));
    }
    if page < total_pages {
        ctx.insert("next_page", &(page + 1));
    }

    if is_htmx(&headers) {
        render(&state, "books_grid.html", &ctx)
    } else {
        render(&state, "books.html", &ctx)
    }
}

/// Build a listing card for a publication, including the user's progress.
async fn card_view(
    state: &AppState,
    user_id: i64,
    publication: &store::Publication,
) -> BookCardView {
    let progress = store::get_progress_for_publication(&state.pool, user_id, publication)
        .await
        .unwrap_or(None);
    BookCardView {
        document_hash: publication.document_hash.clone(),
        title: publication.title.clone(),
        authors: authors_string(&publication.authors),
        series: publication.series.clone().unwrap_or_default(),
        series_index: series_index_label(publication.series_index),
        has_thumb: publication.thumb_path.is_some(),
        hue: cover_hue(&publication.title),
        has_progress: progress.is_some(),
        percentage: progress.as_ref().map_or(0.0, |doc| doc.percentage),
        percent: progress
            .as_ref()
            .map_or_else(String::new, |doc| format!("{:.2}%", doc.percentage * 100.0)),
    }
}

/// Borrow a filter value only when it is non-empty.
fn non_empty(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}

/// Percent-encode a string for use in a URL query component.
fn url_encode(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Render the book detail page.
pub(super) async fn detail(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(publication) = store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .unwrap_or(None)
    else {
        return Redirect::to("/books").into_response();
    };

    let progress = store::get_progress_for_publication(&state.pool, user.id, &publication)
        .await
        .unwrap_or(None);

    let view = BookDetailView {
        document_hash: publication.document_hash.clone(),
        title: publication.title.clone(),
        authors: authors_string(&publication.authors),
        series: publication.series.clone(),
        genres: serde_json::from_str::<Vec<String>>(&publication.genres).unwrap_or_default(),
        language: publication.language.clone(),
        identifier: publication.identifier.clone(),
        publisher: publication.publisher.clone(),
        published: publication.published.clone(),
        description: publication.description.clone(),
        file_size: format_size(publication.file_size),
        created: format_timestamp(publication.created_at),
        updated: format_timestamp(publication.updated_at),
        has_cover: publication.cover_path.is_some(),
        has_progress: progress.is_some(),
        percentage: progress.as_ref().map_or(0.0, |doc| doc.percentage),
        percent: progress
            .as_ref()
            .map_or_else(String::new, |doc| format!("{:.2}%", doc.percentage * 100.0)),
        progress: progress
            .as_ref()
            .map_or_else(String::new, |doc| doc.progress.clone()),
        device: progress
            .as_ref()
            .map_or_else(String::new, |doc| doc.device.clone()),
        synced: progress
            .as_ref()
            .map_or_else(String::new, |doc| format_timestamp(doc.timestamp)),
        history_hash: progress.as_ref().map(|doc| doc.document_hash.clone()),
        series_index: series_index_label(publication.series_index),
        isbn: publication.isbn.clone(),
        hue: cover_hue(&publication.title),
    };

    let mut series_books = Vec::new();
    if let Some(series) = publication.series.as_deref() {
        let filter = store::PublicationFilter {
            series: Some(series),
            sort: store::PublicationSort::Series,
            user_id: user.id,
            ..store::PublicationFilter::default()
        };
        let siblings = store::list_publications_filtered(&state.pool, &filter, SERIES_LIMIT, 0)
            .await
            .unwrap_or_default();
        // A series of one is just this book: skip the rail.
        if siblings.len() > 1 {
            for sibling in &siblings {
                series_books.push(card_view(&state, user.id, sibling).await);
            }
        }
    }

    let mut ctx = base_context(&user, "books");
    ctx.insert("book", &view);
    ctx.insert("series_books", &series_books);

    render(&state, "book.html", &ctx)
}

/// Serve the full-size cover image.
pub(super) async fn cover(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    serve_image(&state, &cookies, &document_hash, false).await
}

/// Serve the generated cover thumbnail.
pub(super) async fn thumbnail(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    serve_image(&state, &cookies, &document_hash, true).await
}

/// Serve the cover or thumbnail for a publication.
async fn serve_image(
    state: &AppState,
    cookies: &Cookies,
    document_hash: &str,
    thumbnail: bool,
) -> Response {
    if current_user(state, cookies).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let Some(publication) = store::get_publication_by_hash(&state.pool, document_hash)
        .await
        .unwrap_or(None)
    else {
        return StatusCode::NOT_FOUND.into_response();
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
        return StatusCode::NOT_FOUND.into_response();
    };

    match library::file_body(state, path).await {
        Ok(body) => (
            [
                (header::CONTENT_TYPE, media_type.to_owned()),
                (header::CACHE_CONTROL, "public, max-age=3600".to_owned()),
            ],
            body,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(error = %err, path, "failed to read cover image");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Stream an `EPUB` file to the client.
pub(super) async fn download(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    let Some(_user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(publication) = store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .unwrap_or(None)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let body = match library::file_body(&state, &publication.file_path).await {
        Ok(body) => body,
        Err(err) => {
            tracing::error!(error = %err, "failed to read EPUB file");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let filename = format!("{}.epub", library::sanitize_filename(&publication.title));
    (
        [
            (header::CONTENT_TYPE, "application/epub+zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        body,
    )
        .into_response()
}

/// Delete a publication and its stored files (admin only).
pub(super) async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    if !user.is_administrator {
        return Redirect::to("/books").into_response();
    }

    if let Some(publication) = store::get_publication_by_hash(&state.pool, &document_hash)
        .await
        .unwrap_or(None)
    {
        library::remove_file(&state, &publication.file_path).await;
        if let Some(path) = &publication.cover_path {
            library::remove_file(&state, path).await;
        }
        if let Some(path) = &publication.thumb_path {
            library::remove_file(&state, path).await;
        }
        store::delete_publication(&state.pool, publication.id)
            .await
            .ok();
    }

    if is_htmx(&headers) {
        StatusCode::OK.into_response()
    } else {
        Redirect::to("/books").into_response()
    }
}
