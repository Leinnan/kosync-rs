//! Paginated reading-progress dashboard.

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tower_cookies::Cookies;

use crate::{i18n::Tr, state::AppState, store};

use super::{
    BookLinkView, base_context, book_link, current_user, format, format_timestamp, is_htmx, render,
    title_tint,
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

/// Number of in-progress books listed beside the Continue panel.
const ALSO_READING: usize = 2;
/// Number of books on the dashboard's "Recently added" shelf.
const RECENT_BOOKS: i64 = 6;

/// Number of books gathered for the "Continue reading" panel.
const CONTINUE_LIMIT: usize = 4;

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
    /// Tint of the generated placeholder cover for unmatched documents.
    tint: String,
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
    tint: String,
    percentage: f64,
    synced: String,
    device: String,
}

/// One day in the dashboard activity heatmap.
#[derive(Debug, Serialize)]
struct HeatCell {
    /// Intensity class: `l0`..`l3`, or `pad` for the days before the first
    /// week starts.
    level: &'static str,
    /// The day and its sync count, for the tooltip.
    label: String,
}

/// Dashboard statistics prepared for rendering.
#[derive(Debug, Serialize)]
struct StatsView {
    finished: i64,
    in_progress: i64,
    syncs_7d: i64,
    syncs_30d: i64,
    devices_30d: i64,
    heat: Vec<HeatCell>,
}

/// Lay out daily sync counts as heatmap cells, one column per week.
///
/// Columns start on Monday, so leading padding cells line the first day up
/// with its weekday row. Counts are bucketed into four levels relative to the
/// busiest day.
fn heat_cells(tr: &Tr<'_>, daily: &[i64], first_day: i64) -> Vec<HeatCell> {
    let max = daily.iter().copied().max().unwrap_or(0).max(1);
    // Day 0 of the Unix epoch was a Thursday: three days after a Monday.
    let padding = usize::try_from((first_day + 3).rem_euclid(7)).unwrap_or(0);
    let mut cells: Vec<HeatCell> = (0..padding)
        .map(|_| HeatCell {
            level: "pad",
            label: String::new(),
        })
        .collect();
    for (day, &count) in (first_day..).zip(daily) {
        let level = match count {
            0 => "l0",
            c if c * 3 <= max => "l1",
            c if c * 3 <= max * 2 => "l2",
            _ => "l3",
        };
        let date = OffsetDateTime::from_unix_timestamp(day * 86_400)
            .map(|day| format::human_date(tr, day))
            .unwrap_or_default();
        cells.push(HeatCell {
            level,
            label: tr.args(
                "progress_heat_cell",
                &[("date", &date), ("syncs", &tr.n("count_syncs", count, &[]))],
            ),
        });
    }
    cells
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
    fn chips(&self, tr: &Tr<'_>) -> Vec<FilterChip> {
        let mut chips = Vec::new();
        if !self.query.is_empty() {
            chips.push(FilterChip {
                label: tr.args("chip_search", &[("value", &self.query)]),
                href: self.href_without("query"),
            });
        }
        if !self.status.is_empty() {
            let label = tr.t(if self.status == "finished" {
                "status_finished"
            } else {
                "status_in_progress"
            });
            chips.push(FilterChip {
                label: tr.args("chip_status", &[("value", &label)]),
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
async fn document_view(state: &AppState, tr: &Tr<'_>, doc: store::Document) -> DocumentView {
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

    let placeholder = if fallback_title.is_empty() {
        doc.document_hash.clone()
    } else {
        fallback_title.clone()
    };
    DocumentView {
        tint: title_tint(&placeholder),
        percent: format::pct_precise(tr, doc.percentage, 2),
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

    let tr = user.tr(&state);
    let mut views = Vec::with_capacity(documents.len());
    for doc in documents {
        views.push(document_view(&state, &tr, doc).await);
    }

    let mut ctx = base_context(&state, &user, "progress");
    ctx.insert("documents", &views);
    ctx.insert("total", &total);
    ctx.insert("total_label", &tr.n("count_documents", total, &[]));
    ctx.insert("page", &page);
    ctx.insert("total_pages", &total_pages);
    ctx.insert("has_prev", &(page > 1));
    ctx.insert("has_next", &(page < total_pages));
    ctx.insert("query", &filters.query);
    ctx.insert("status", &filters.status);
    ctx.insert("sort", &filters.sort);
    ctx.insert("query_suffix", &filters.suffix());
    ctx.insert("clear_url", &"/");
    ctx.insert("active_filters", &filters.chips(&tr));
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
        ctx.insert("stats", &stats_view(&state, &tr, user.id).await);
        let mut reading = continue_reading(&state, &tr, user.id).await.into_iter();
        if let Some(lead) = reading.next() {
            ctx.insert("continue_lead", &lead);
        }
        let also: Vec<ContinueView> = reading.by_ref().take(ALSO_READING).collect();
        ctx.insert("also_reading", &also);
        ctx.insert("more_reading", &reading.next().is_some());
        let recent: Vec<BookLinkView> = store::list_recent_publications(&state.pool, RECENT_BOOKS)
            .await
            .unwrap_or_default()
            .iter()
            .map(book_link)
            .collect();
        ctx.insert("recent_books", &recent);
    }

    render(&state, "progress.html", &ctx)
}

/// Load and lay out the dashboard statistics.
async fn stats_view(state: &AppState, tr: &Tr<'_>, user_id: i64) -> StatsView {
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
        heat: heat_cells(tr, &summary.daily, summary.first_day),
    }
}

/// The most recently synced unfinished documents that have a readable title.
async fn continue_reading(state: &AppState, tr: &Tr<'_>, user_id: i64) -> Vec<ContinueView> {
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
        let view = document_view(state, tr, doc).await;
        let item = match &view.book {
            Some(book) => ContinueView {
                book_hash: Some(book.document_hash.clone()),
                document_hash: view.document_hash.clone(),
                title: book.title.clone(),
                author: book.author.clone(),
                thumb_hash: book.has_thumb.then(|| book.document_hash.clone()),
                tint: book.tint.clone(),
                percentage: view.percentage,
                synced: view.synced.clone(),
                device: view.device.clone(),
            },
            None if !view.fallback_title.is_empty() => ContinueView {
                book_hash: None,
                document_hash: view.document_hash.clone(),
                tint: title_tint(&view.fallback_title),
                title: view.fallback_title.clone(),
                author: view.fallback_author.clone(),
                thumb_hash: None,
                percentage: view.percentage,
                synced: view.synced.clone(),
                device: view.device.clone(),
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use web_mev_i18n::LocaleCode;

    use super::heat_cells;
    use crate::i18n::I18n;

    #[test]
    fn heatmap_weeks_start_on_monday_and_levels_scale_to_the_busiest_day() {
        let i18n = I18n::load("en").unwrap();
        let tr = i18n.tr(&LocaleCode::try_from("en").unwrap());
        // Unix day 4 was Monday 5 January 1970; day 6 a Wednesday.
        let cells = heat_cells(&tr, &[0, 1, 2, 3, 6], 6);
        let levels: Vec<&str> = cells.iter().map(|cell| cell.level).collect();
        assert_eq!(levels, ["pad", "pad", "l0", "l1", "l1", "l2", "l3"]);
        assert_eq!(cells[0].label, "");
        assert!(cells[3].label.contains("1 sync"), "{}", cells[3].label);

        let monday = heat_cells(&tr, &[0; 7], 4);
        assert_eq!(monday.len(), 7, "a Monday start needs no padding");
    }
}
