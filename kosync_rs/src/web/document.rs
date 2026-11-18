//! Per-document progress history page.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
};
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tower_cookies::Cookies;
use web_mev_auth::CsrfForm;

use crate::{
    i18n::Tr,
    identity,
    state::AppState,
    store::{self, StoreError},
};

use super::{
    BookLinkView, Viewer, base_context, book_link, current_user, flash_next, format,
    format_timestamp, render, take_flash,
};

/// Number of device colours defined in the stylesheet (`.dev-0` … `.dev-5`).
const DEVICE_COLOURS: usize = 6;

/// Number of most recent sync events listed on the page.
const RECENT_EVENTS: usize = 20;

/// Chart canvas size and padding, in SVG user units.
const CHART_W: f64 = 640.0;
const CHART_H: f64 = 220.0;
const PAD_L: f64 = 40.0;
const PAD_R: f64 = 16.0;
const PAD_T: f64 = 12.0;
const PAD_B: f64 = 28.0;

/// A device row prepared for rendering in the detail template.
#[derive(Debug, Serialize)]
pub(super) struct DeviceView {
    device: String,
    device_id: Option<String>,
    first_seen: String,
    last_seen: String,
    sync_count: i64,
    contribution: String,
    /// Share of all forward progress, as a percentage (0–100).
    share: f64,
    /// Colour slot (`.dev-N`) used consistently across the page.
    colour: usize,
}

/// A plotted sync event.
#[derive(Debug, Serialize)]
struct PointView {
    x: String,
    y: String,
    colour: usize,
    label: String,
}

/// Progress-over-time chart geometry for the template.
#[derive(Debug, Serialize)]
struct ChartView {
    line: String,
    area: String,
    points: Vec<PointView>,
    start_label: String,
    end_label: String,
}

/// A recent sync event row.
#[derive(Debug, Serialize)]
pub(super) struct EventView {
    device: String,
    colour: usize,
    percentage: f64,
    /// Signed change against the previous event, e.g. `+4.2%`; empty if none.
    delta: String,
    /// `up`, `down`, or empty.
    trend: &'static str,
    synced: String,
}

/// Grouping key shared with [`store::compute_timeline`].
fn device_key(device: &str, device_id: Option<&str>) -> String {
    device_id.unwrap_or(device).to_owned()
}

/// Convert a Unix timestamp to `f64` for chart geometry.
#[allow(
    clippy::cast_precision_loss,
    reason = "Unix timestamps are far below 2^53, so the conversion is exact."
)]
fn ts_f64(ts: i64) -> f64 {
    ts as f64
}

/// Convert a small index to `f64` for chart geometry.
fn index_f64(index: usize) -> f64 {
    f64::from(u32::try_from(index).unwrap_or(u32::MAX))
}

/// Format a timestamp as a short absolute date for labels.
fn short_date(tr: &Tr<'_>, ts: i64) -> String {
    OffsetDateTime::from_unix_timestamp(ts)
        .map(|day| format::human_date(tr, day))
        .unwrap_or_default()
}

/// Lay out the progress history as an SVG line chart.
fn build_chart(
    tr: &Tr<'_>,
    events: &[store::ProgressEvent],
    colours: &HashMap<String, usize>,
) -> Option<ChartView> {
    let first = events.first()?;
    let last = events.last()?;
    let plot_w = CHART_W - PAD_L - PAD_R;
    let plot_h = CHART_H - PAD_T - PAD_B;
    let span = ts_f64(last.timestamp - first.timestamp);
    let count = events.len();

    let mut coords = Vec::with_capacity(count);
    let mut points = Vec::with_capacity(count);
    for (index, event) in events.iter().enumerate() {
        let t = if span > 0.0 {
            ts_f64(event.timestamp - first.timestamp) / span
        } else if count > 1 {
            index_f64(index) / index_f64(count - 1)
        } else {
            0.5
        };
        let x = PAD_L + t * plot_w;
        let y = PAD_T + (1.0 - event.percentage.clamp(0.0, 1.0)) * plot_h;
        coords.push((x, y));
        points.push(PointView {
            x: format!("{x:.1}"),
            y: format!("{y:.1}"),
            colour: colours
                .get(&device_key(&event.device, event.device_id.as_deref()))
                .copied()
                .unwrap_or(0),
            label: tr.args(
                "doc_point_label",
                &[
                    ("device", &event.device),
                    ("percent", &format::pct_precise(tr, event.percentage, 1)),
                    ("date", &short_date(tr, event.timestamp)),
                ],
            ),
        });
    }

    let line = coords
        .iter()
        .enumerate()
        .map(|(i, (x, y))| format!("{}{x:.1} {y:.1}", if i == 0 { "M" } else { "L" }))
        .collect::<Vec<_>>()
        .join(" ");
    let bottom = PAD_T + plot_h;
    let (first_x, _) = coords.first().copied().unwrap_or_default();
    let (last_x, _) = coords.last().copied().unwrap_or_default();
    let area = format!("{line} L{last_x:.1} {bottom:.1} L{first_x:.1} {bottom:.1} Z");

    Some(ChartView {
        line,
        area,
        points,
        start_label: short_date(tr, first.timestamp),
        end_label: short_date(tr, last.timestamp),
    })
}

/// The most recent events, newest first, with the change against the
/// chronologically previous event.
fn recent_events(
    tr: &Tr<'_>,
    events: &[store::ProgressEvent],
    colours: &HashMap<String, usize>,
    limit: usize,
) -> Vec<EventView> {
    let start = events.len().saturating_sub(limit);
    let mut views: Vec<EventView> = events
        .iter()
        .enumerate()
        .skip(start)
        .map(|(index, event)| {
            let previous = index.checked_sub(1).and_then(|i| events.get(i));
            let change = previous.map_or(0.0, |prev| event.percentage - prev.percentage);
            let (delta, trend) = if previous.is_none() || change.abs() < 0.0005 {
                (String::new(), "")
            } else if change > 0.0 {
                (format!("+{}", format::pct_precise(tr, change, 1)), "up")
            } else {
                (format!("−{}", format::pct_precise(tr, -change, 1)), "down")
            };
            EventView {
                device: event.device.clone(),
                colour: colours
                    .get(&device_key(&event.device, event.device_id.as_deref()))
                    .copied()
                    .unwrap_or(0),
                percentage: event.percentage,
                delta,
                trend,
                synced: format_timestamp(event.timestamp),
            }
        })
        .collect();
    views.reverse();
    views
}

/// Device rows for a timeline, plus the colour slot of each device key.
///
/// Colour slots follow the timeline's first-appearance device order, so a
/// device keeps its colour across the chart, the event list and the table.
fn device_views(
    tr: &Tr<'_>,
    timeline: &store::DocumentTimeline,
) -> (Vec<DeviceView>, HashMap<String, usize>) {
    let colours: HashMap<String, usize> = timeline
        .devices
        .iter()
        .enumerate()
        .map(|(index, dev)| {
            (
                device_key(&dev.device, dev.device_id.as_deref()),
                index % DEVICE_COLOURS,
            )
        })
        .collect();
    let total_contribution: f64 = timeline.devices.iter().map(|dev| dev.contribution).sum();
    let devices = timeline
        .devices
        .iter()
        .enumerate()
        .map(|(index, dev)| DeviceView {
            device: dev.device.clone(),
            device_id: dev.device_id.clone(),
            first_seen: format_timestamp(dev.first_seen),
            last_seen: format_timestamp(dev.last_seen),
            sync_count: dev.sync_count,
            contribution: format::pct_precise(tr, dev.contribution, 2),
            share: if total_contribution > 0.0 {
                dev.contribution / total_contribution * 100.0
            } else {
                0.0
            },
            colour: index % DEVICE_COLOURS,
        })
        .collect();
    (devices, colours)
}

/// A document's latest syncs and per-device shares, for pages that summarise
/// its history (the book page).
#[derive(Debug, Default, Serialize)]
pub(super) struct SyncSummary {
    events: Vec<EventView>,
    devices: Vec<DeviceView>,
}

/// Summarise the last `limit` syncs of `user_id`'s document `document_hash`.
pub(super) async fn sync_summary(
    state: &AppState,
    tr: &Tr<'_>,
    user_id: i64,
    document_hash: &str,
    limit: usize,
) -> SyncSummary {
    let history = store::get_document_history(&state.pool, user_id, document_hash)
        .await
        .unwrap_or_default();
    let timeline = store::compute_timeline(&history);
    let (devices, colours) = device_views(tr, &timeline);
    SyncSummary {
        events: recent_events(tr, &history, &colours, limit),
        devices,
    }
}

/// A document detail view prepared for rendering in the detail template.
#[derive(Debug, Serialize)]
struct DocumentDetailView {
    document_hash: String,
    percentage: f64,
    percent: String,
    progress: String,
    device: String,
    synced: String,
    first_recorded: String,
    last_recorded: String,
    sync_count: usize,
    devices: Vec<DeviceView>,
    chart: Option<ChartView>,
    events: Vec<EventView>,
    book: Option<BookLinkView>,
    /// Fallback display title (captured metadata) when no book matches.
    fallback_title: String,
    /// Fallback author (captured metadata) when no book matches.
    fallback_author: String,
    /// Library books to link an unmatched document to, best guess first.
    link_choices: Vec<LinkChoiceView>,
}

/// A library book offered in the "same book as…" picker.
#[derive(Debug, Serialize)]
struct LinkChoiceView {
    document_hash: String,
    label: String,
    /// Whether the document's own title names this book.
    suggested: bool,
}

/// Library books for linking `doc` by hand, those whose title matches the
/// document's first.
async fn link_choices(state: &AppState, doc: &store::Document) -> Vec<LinkChoiceView> {
    let title = doc
        .title
        .as_deref()
        .map_or_else(String::new, identity::fold);
    let mut choices: Vec<LinkChoiceView> = store::list_publication_choices(&state.pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|choice| {
            let authors: Vec<String> = serde_json::from_str(&choice.authors).unwrap_or_default();
            let label = if authors.is_empty() {
                choice.title.clone()
            } else {
                format!("{} — {}", choice.title, authors.join(", "))
            };
            LinkChoiceView {
                suggested: !title.is_empty() && identity::fold(&choice.title) == title,
                document_hash: choice.document_hash,
                label,
            }
        })
        .collect();
    // Stable, so books keep their title order within each group.
    choices.sort_by_key(|choice| !choice.suggested);
    choices
}

/// Render the per-document progress history page.
pub(super) async fn document_detail(
    State(state): State<AppState>,
    cookies: Cookies,
    Path(document_hash): Path<String>,
) -> Response {
    let Some(user) = current_user(&state, &cookies).await else {
        return Redirect::to("/login").into_response();
    };

    let Some(doc) = store::get_progress(&state.pool, user.id, &document_hash)
        .await
        .unwrap_or(None)
    else {
        return Redirect::to("/").into_response();
    };

    let history = store::get_document_history(&state.pool, user.id, &document_hash)
        .await
        .unwrap_or_default();
    let timeline = store::compute_timeline(&history);

    let tr = user.tr(&state);
    let (devices, colours) = device_views(&tr, &timeline);

    let book = store::find_publication_for_document(&state.pool, &doc)
        .await
        .unwrap_or(None)
        .map(|publication| book_link(&publication));
    let link_choices = if book.is_none() {
        link_choices(&state, &doc).await
    } else {
        Vec::new()
    };

    let fallback_title = doc
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_default();
    let fallback_author = doc.authors.clone().unwrap_or_default();

    let view = DocumentDetailView {
        percent: format::pct_precise(&tr, doc.percentage, 2),
        percentage: doc.percentage,
        progress: doc.progress,
        document_hash: doc.document_hash,
        device: doc.device,
        synced: format_timestamp(doc.timestamp),
        first_recorded: timeline
            .first_recorded
            .map_or_else(String::new, format_timestamp),
        last_recorded: timeline
            .last_recorded
            .map_or_else(String::new, format_timestamp),
        sync_count: timeline.sync_count,
        devices,
        chart: build_chart(&tr, &history, &colours),
        events: recent_events(&tr, &history, &colours, RECENT_EVENTS),
        book,
        fallback_title,
        fallback_author,
        link_choices,
    };

    let mut ctx = base_context(&state, &user, "progress");
    ctx.insert("document", &view);
    let sync_count = i64::try_from(view.sync_count).unwrap_or(i64::MAX);
    ctx.insert("sync_label", &tr.n("count_sync_events", sync_count, &[]));
    take_flash(&state, &cookies, &mut ctx);

    render(&state, "document.html", &ctx)
}

/// The "same book as…" form.
#[derive(Debug, Deserialize)]
pub(super) struct LinkForm {
    /// Document digest of the chosen library book.
    publication: String,
}

/// Declare that a synced document the server could not match is a library
/// book. Its identifier then resolves to the book for every reader, and the
/// progress recorded under it joins the book's.
pub(super) async fn link(
    State(state): State<AppState>,
    Path(document_hash): Path<String>,
    CsrfForm { ui, form }: CsrfForm<store::User, LinkForm>,
) -> Response {
    let cookies = ui.cookies;
    let viewer = Viewer::new(&state, ui.session);
    let back = format!("/documents/{document_hash}");

    match store::link_document(&state.pool, viewer.id, &document_hash, &form.publication).await {
        Ok(book_hash) => {
            flash_next(
                &state,
                &cookies,
                false,
                &viewer.tr(&state).t("doc_link_done"),
            );
            Redirect::to(&format!("/documents/{book_hash}")).into_response()
        }
        Err(err) => {
            let key = match err {
                StoreError::Exists => "doc_link_exists",
                StoreError::Stale => "doc_link_missing",
                StoreError::Db(_) => {
                    err.log();
                    "error_generic"
                }
            };
            flash_next(&state, &cookies, true, &viewer.tr(&state).t(key));
            Redirect::to(&back).into_response()
        }
    }
}
