//! Per-document progress history page.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Redirect, Response},
};
use std::collections::HashMap;

use serde::Serialize;
use time::OffsetDateTime;
use tower_cookies::Cookies;

use crate::{state::AppState, store};

use super::{
    BookLinkView, base_context, book_link, current_user, format_timestamp, human_date, pluralize,
    render,
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
struct DeviceView {
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
struct EventView {
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
fn short_date(ts: i64) -> String {
    OffsetDateTime::from_unix_timestamp(ts)
        .map(human_date)
        .unwrap_or_default()
}

/// Lay out the progress history as an SVG line chart.
fn build_chart(
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
            label: format!(
                "{} · {:.1}% · {}",
                event.device,
                event.percentage * 100.0,
                short_date(event.timestamp)
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
        start_label: short_date(first.timestamp),
        end_label: short_date(last.timestamp),
    })
}

/// The most recent events, newest first, with the change against the
/// chronologically previous event.
fn recent_events(
    events: &[store::ProgressEvent],
    colours: &HashMap<String, usize>,
) -> Vec<EventView> {
    let start = events.len().saturating_sub(RECENT_EVENTS);
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
                (format!("+{:.1}%", change * 100.0), "up")
            } else {
                (format!("−{:.1}%", -change * 100.0), "down")
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

    // Colour slots follow the timeline's first-appearance device order.
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
    let devices: Vec<DeviceView> = timeline
        .devices
        .iter()
        .enumerate()
        .map(|(index, dev)| DeviceView {
            device: dev.device.clone(),
            device_id: dev.device_id.clone(),
            first_seen: format_timestamp(dev.first_seen),
            last_seen: format_timestamp(dev.last_seen),
            sync_count: dev.sync_count,
            contribution: format!("{:.2}%", dev.contribution * 100.0),
            share: if total_contribution > 0.0 {
                dev.contribution / total_contribution * 100.0
            } else {
                0.0
            },
            colour: index % DEVICE_COLOURS,
        })
        .collect();

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

    let view = DocumentDetailView {
        percent: format!("{:.2}%", doc.percentage * 100.0),
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
        chart: build_chart(&history, &colours),
        events: recent_events(&history, &colours),
        book,
        fallback_title,
        fallback_author,
    };

    let mut ctx = base_context(&user, "progress");
    ctx.insert("document", &view);
    let sync_count = i64::try_from(view.sync_count).unwrap_or(i64::MAX);
    ctx.insert(
        "sync_label",
        &pluralize(sync_count, "sync event", "sync events"),
    );

    render(&state, "document.html", &ctx)
}
