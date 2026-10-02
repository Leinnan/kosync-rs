//! Paginated reading-progress dashboard.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tower_cookies::Cookies;

use crate::{state::AppState, store};

use super::{
    BookLinkView, base_context, book_link, cover_hue, current_user, format_timestamp, human_date,
    is_htmx, pluralize, render,
};

/// Dashboard query parameters.
#[derive(Debug, Deserialize)]
pub(super) struct ProgressQuery {
    page: Option<i64>,
    query: Option<String>,
    status: Option<String>,
    sort: Option<String>,
}

/// Number of documents shown per page.
const PER_PAGE: i64 = 20;

/// Number of books shown in the "Continue reading" rail.
const CONTINUE_LIMIT: usize = 8;

/// Number of recent unfinished documents scanned to fill the rail.
const CONTINUE_SCAN: i64 = 24;

/// A document row prepared for rendering in the dashboard template.
#[derive(Debug, Serialize)]
struct DocumentView {
    document_hash: String,
    percentage: f64,
    percent: String,
    device: String,
    synced: String,
    book: Option<BookLinkView>,
    /// Fallback display title (captured metadata) when no book matches.
    fallback_title: String,
    /// Fallback author (captured metadata) when no book matches.
    fallback_author: String,
    /// Hue of the generated placeholder cover for unmatched documents.
    hue: u16,
}

/// An in-progress document shown in the dashboard's "Continue reading" rail.
#[derive(Debug, Serialize)]
struct ContinueView {
    /// Library book hash when the document matches a publication.
    book_hash: Option<String>,
    document_hash: String,
    title: String,
    author: String,
    thumb_hash: Option<String>,
    hue: u16,
    percentage: f64,
    synced: String,
}

/// One day's bar in the dashboard activity sparkline.
#[derive(Debug, Serialize)]
struct BarView {
    x: i64,
    y: String,
    height: String,
    label: String,
    empty: bool,
}

/// Dashboard statistics prepared for rendering.
#[derive(Debug, Serialize)]
struct StatsView {
    finished: i64,
    in_progress: i64,
    syncs_7d: i64,
    syncs_30d: i64,
    devices_30d: i64,
    bars: Vec<BarView>,
}

/// Width of one sparkline slot in SVG user units.
const BAR_SLOT: i64 = 10;
/// Drawable sparkline height in SVG user units.
const BAR_HEIGHT: f64 = 44.0;

/// Lossless-enough conversion of a small count to `f64` for chart geometry.
fn count_f64(value: i64) -> f64 {
    f64::from(i32::try_from(value).unwrap_or(i32::MAX))
}

/// Lay out the daily sync counts as sparkline bars.
fn activity_bars(daily: &[i64], first_day: i64) -> Vec<BarView> {
    let max = daily.iter().copied().max().unwrap_or(0).max(1);
    let mut bars = Vec::with_capacity(daily.len());
    let mut x = 0;
    for (day, &count) in (first_day..).zip(daily) {
        let height = if count == 0 {
            2.0
        } else {
            (count_f64(count) / count_f64(max) * BAR_HEIGHT).max(4.0)
        };
        let date = OffsetDateTime::from_unix_timestamp(day * 86_400)
            .map(human_date)
            .unwrap_or_default();
        bars.push(BarView {
            x,
            y: format!("{:.1}", BAR_HEIGHT + 4.0 - height),
            height: format!("{height:.1}"),
            label: format!("{date}: {}", pluralize(count, "sync", "syncs")),
            empty: count == 0,
        });
        x += BAR_SLOT;
    }
    bars
}

/// An active filter shown as a removable chip.
#[derive(Debug, Serialize)]
struct FilterChip {
    label: String,
    href: String,
}

/// Resolved dashboard filters.
#[derive(Debug)]
struct Filters {
    query: String,
    status: String,
    sort: String,
}

impl Filters {
    /// Normalize raw query parameters.
    fn from_query(query: &ProgressQuery) -> Self {
        Self {
            query: clean(query.query.as_deref()),
            status: match query.status.as_deref() {
                Some("in-progress") => "in-progress".to_owned(),
                Some("finished") => "finished".to_owned(),
                _ => String::new(),
            },
            sort: match query.sort.as_deref() {
                Some("title") => "title".to_owned(),
                _ => "recent".to_owned(),
            },
        }
    }

    /// The store-level finished filter.
    fn finished(&self) -> Option<bool> {
        match self.status.as_str() {
            "in-progress" => Some(false),
            "finished" => Some(true),
            _ => None,
        }
    }

    /// Non-empty `(name, value)` pairs to preserve in links.
    fn pairs(&self) -> Vec<(&'static str, &str)> {
        let mut pairs = Vec::new();
        if !self.query.is_empty() {
            pairs.push(("query", self.query.as_str()));
        }
        if !self.status.is_empty() {
            pairs.push(("status", self.status.as_str()));
        }
        if self.sort != "recent" {
            pairs.push(("sort", self.sort.as_str()));
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

    /// A `/` URL with one filter removed.
    fn href_without(&self, omit: &str) -> String {
        let encoded: Vec<String> = self
            .pairs()
            .iter()
            .filter(|(key, _)| *key != omit)
            .map(|(key, value)| format!("{key}={}", url_encode(value)))
            .collect();
        if encoded.is_empty() {
            "/".to_owned()
        } else {
            format!("/?{}", encoded.join("&"))
        }
    }

    /// The active filters as removable chips.
    fn chips(&self) -> Vec<FilterChip> {
        let mut chips = Vec::new();
        if !self.query.is_empty() {
            chips.push(FilterChip {
                label: format!("Search: {}", self.query),
                href: self.href_without("query"),
            });
        }
        if !self.status.is_empty() {
            let label = if self.status == "finished" {
                "Finished"
            } else {
                "In progress"
            };
            chips.push(FilterChip {
                label: format!("Status: {label}"),
                href: self.href_without("status"),
            });
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

/// Build a document view, resolving any matching publication.
async fn document_view(state: &AppState, doc: store::Document) -> DocumentView {
    let book = store::find_publication_for_document(&state.pool, &doc)
        .await
        .unwrap_or(None)
        .map(|publication| book_link(&publication));

    let fallback_title = doc
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_default();
    let fallback_author = doc.authors.clone().unwrap_or_default();

    DocumentView {
        hue: cover_hue(if fallback_title.is_empty() {
            &doc.document_hash
        } else {
            &fallback_title
        }),
        percent: format!("{:.2}%", doc.percentage * 100.0),
        synced: format_timestamp(doc.timestamp),
        document_hash: doc.document_hash,
        percentage: doc.percentage,
        device: doc.device,
        book,
        fallback_title,
        fallback_author,
    }
}

/// Render the paginated reading-progress dashboard.
pub(super) async fn progress_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<ProgressQuery>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let filters = Filters::from_query(&query);
    let filter = store::DocumentFilter {
        search: non_empty(&filters.query),
        finished: filters.finished(),
        sort: match filters.sort.as_str() {
            "title" => store::DocumentSort::Title,
            _ => store::DocumentSort::Recent,
        },
        user_id: user.id,
    };

    let total = store::count_documents_filtered(&state.pool, &filter)
        .await
        .unwrap_or(0);
    let total_pages = ((total + PER_PAGE - 1) / PER_PAGE).max(1);

    let page = query.page.unwrap_or(1).max(1).min(total_pages);
    let offset = (page - 1) * PER_PAGE;

    let documents = store::list_documents_filtered(&state.pool, &filter, PER_PAGE, offset)
        .await
        .unwrap_or_default();

    let mut views = Vec::with_capacity(documents.len());
    for doc in documents {
        views.push(document_view(&state, doc).await);
    }

    let mut ctx = base_context(&user, "progress");
    ctx.insert("documents", &views);
    ctx.insert("total", &total);
    ctx.insert("total_label", &pluralize(total, "document", "documents"));
    ctx.insert("page", &page);
    ctx.insert("total_pages", &total_pages);
    ctx.insert("has_prev", &(page > 1));
    ctx.insert("has_next", &(page < total_pages));
    ctx.insert("query", &filters.query);
    ctx.insert("status", &filters.status);
    ctx.insert("sort", &filters.sort);
    ctx.insert("query_suffix", &filters.suffix());
    ctx.insert("clear_url", &"/");
    ctx.insert("active_filters", &filters.chips());
    if page > 1 {
        ctx.insert("prev_page", &(page - 1));
    }
    if page < total_pages {
        ctx.insert("next_page", &(page + 1));
    }

    if is_htmx(&headers) {
        return render(&state, "progress_table.html", &ctx);
    }

    // The dashboard widgets only appear on the unfiltered first page.
    let show_dashboard = page == 1 && filters.query.is_empty() && filters.status.is_empty();
    ctx.insert("show_dashboard", &show_dashboard);
    if show_dashboard {
        ctx.insert("stats", &stats_view(&state, user.id).await);
        ctx.insert("continue_reading", &continue_reading(&state, user.id).await);
    }

    render(&state, "progress.html", &ctx)
}

/// Load and lay out the dashboard statistics.
async fn stats_view(state: &AppState, user_id: i64) -> StatsView {
    let summary = store::reading_stats(
        &state.pool,
        user_id,
        OffsetDateTime::now_utc().unix_timestamp(),
    )
    .await
    .unwrap_or_default();
    StatsView {
        finished: summary.finished,
        in_progress: summary.in_progress,
        syncs_7d: summary.syncs_7d,
        syncs_30d: summary.syncs_30d,
        devices_30d: summary.devices_30d,
        bars: activity_bars(&summary.daily, summary.first_day),
    }
}

/// The most recently synced unfinished documents that have a readable title.
async fn continue_reading(state: &AppState, user_id: i64) -> Vec<ContinueView> {
    let filter = store::DocumentFilter {
        search: None,
        finished: Some(false),
        sort: store::DocumentSort::Recent,
        user_id,
    };
    let documents = store::list_documents_filtered(&state.pool, &filter, CONTINUE_SCAN, 0)
        .await
        .unwrap_or_default();

    let mut items = Vec::new();
    for doc in documents {
        let view = document_view(state, doc).await;
        let item = match &view.book {
            Some(book) => ContinueView {
                book_hash: Some(book.document_hash.clone()),
                document_hash: view.document_hash.clone(),
                title: book.title.clone(),
                author: book.author.clone(),
                thumb_hash: book.has_thumb.then(|| book.document_hash.clone()),
                hue: book.hue,
                percentage: view.percentage,
                synced: view.synced.clone(),
            },
            None if !view.fallback_title.is_empty() => ContinueView {
                book_hash: None,
                document_hash: view.document_hash.clone(),
                hue: cover_hue(&view.fallback_title),
                title: view.fallback_title.clone(),
                author: view.fallback_author.clone(),
                thumb_hash: None,
                percentage: view.percentage,
                synced: view.synced.clone(),
            },
            None => continue,
        };
        items.push(item);
        if items.len() >= CONTINUE_LIMIT {
            break;
        }
    }
    items
}
