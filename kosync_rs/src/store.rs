//! Data access layer for users and documents.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqlitePool;

use crate::error::{ApiError, ErrorCode};
use crate::models::UserSummary;

/// A dynamically bound value for a filtered query.
enum BindValue {
    /// A text value.
    Text(String),
    /// An integer value.
    Int(i64),
}

/// Apply a list of [`BindValue`]s to a query builder.
macro_rules! bind_values {
    ($query:expr, $binds:expr) => {{
        let mut query = $query;
        for value in $binds {
            query = match value {
                BindValue::Text(text) => query.bind(text),
                BindValue::Int(int) => query.bind(int),
            };
        }
        query
    }};
}

/// A stored user record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct User {
    /// Internal user id.
    pub(crate) id: i64,
    /// The username.
    pub(crate) username: String,
    /// Stored password hash (MD5 hex, as sent by the client).
    pub(crate) password_hash: String,
    /// Whether the user may log in and sync.
    pub(crate) is_active: bool,
    /// Whether the user has administrator privileges.
    pub(crate) is_administrator: bool,
    /// Whether the user may upload books to the shared library.
    pub(crate) can_upload: bool,
}

impl User {
    /// Whether the user is allowed to upload books to the shared library.
    ///
    /// Administrators can always upload; other users need the `can_upload`
    /// permission granted by an administrator.
    pub(crate) fn may_upload(&self) -> bool {
        self.is_administrator || self.can_upload
    }
}

/// A stored document progress record.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
#[allow(clippy::struct_field_names)]
pub(crate) struct Document {
    /// The document identifier.
    pub(crate) document_hash: String,
    /// Reading progress position string.
    pub(crate) progress: String,
    /// Reading progress as a fraction.
    pub(crate) percentage: f64,
    /// Human-readable device name.
    pub(crate) device: String,
    /// Unique device identifier, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device_id: Option<String>,
    /// Server-assigned Unix timestamp (seconds).
    pub(crate) timestamp: i64,
    /// Document title sent via `KOReader` metadata, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    /// Authors sent via `KOReader` metadata, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) authors: Option<String>,
    /// File name sent via `KOReader` metadata, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) filename: Option<String>,
}

/// A recorded progress event for a document (append-only history).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct ProgressEvent {
    /// Internal event id.
    pub(crate) id: i64,
    /// The document identifier.
    pub(crate) document_hash: String,
    /// Reading progress position string.
    pub(crate) progress: String,
    /// Reading progress as a fraction.
    pub(crate) percentage: f64,
    /// Human-readable device name.
    pub(crate) device: String,
    /// Unique device identifier, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device_id: Option<String>,
    /// Server-assigned Unix timestamp (seconds).
    pub(crate) timestamp: i64,
}

/// Aggregated per-device statistics for a document.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DeviceContribution {
    /// Human-readable device name.
    pub(crate) device: String,
    /// Unique device identifier, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) device_id: Option<String>,
    /// Timestamp of the device's first event.
    pub(crate) first_seen: i64,
    /// Timestamp of the device's most recent event.
    pub(crate) last_seen: i64,
    /// Number of progress events recorded by the device.
    pub(crate) sync_count: i64,
    /// Sum of positive percentage increases attributed to the device.
    pub(crate) contribution: f64,
}

/// Aggregated progress timeline for a document.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DocumentTimeline {
    /// Timestamp of the earliest recorded event.
    pub(crate) first_recorded: Option<i64>,
    /// Timestamp of the most recent recorded event.
    pub(crate) last_recorded: Option<i64>,
    /// Total number of recorded events.
    pub(crate) sync_count: usize,
    /// Per-device contribution breakdown.
    pub(crate) devices: Vec<DeviceContribution>,
}

/// Errors that can occur while accessing the store.
#[derive(Debug)]
pub(crate) enum StoreError {
    /// A user with this username already exists.
    Exists,
    /// An underlying database error occurred.
    Db(sqlx::Error),
}

impl From<sqlx::Error> for StoreError {
    fn from(err: sqlx::Error) -> Self {
        Self::Db(err)
    }
}

impl StoreError {
    /// Log the underlying cause, if this is a database error.
    pub(crate) fn log(&self) {
        if let Self::Db(err) = self {
            tracing::error!(error = %err, "database error");
        }
    }

    /// Convert into a protocol [`ApiError`], logging the cause on the way.
    pub(crate) fn into_api_error(self) -> ApiError {
        self.log();
        match self {
            Self::Exists => ApiError::new(ErrorCode::UserExists),
            Self::Db(_) => ApiError::new(ErrorCode::Internal),
        }
    }
}

/// Create a new user with the given (pre-hashed) password.
///
/// # Errors
///
/// Returns [`StoreError::Exists`] if the username is taken, or
/// [`StoreError::Db`] on any storage failure.
pub(crate) async fn create_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
    can_upload: bool,
) -> Result<(), StoreError> {
    if get_user(pool, username).await?.is_some() {
        return Err(StoreError::Exists);
    }

    sqlx::query("INSERT INTO users (username, password_hash, can_upload) VALUES (?, ?, ?)")
        .bind(username)
        .bind(password_hash)
        .bind(can_upload)
        .execute(pool)
        .await?;

    Ok(())
}

/// Look up a user by username.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_user(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<User>, StoreError> {
    sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, is_active, is_administrator, can_upload FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::from)
}

/// Validated input for a progress upsert.
pub(crate) struct ProgressInput<'a> {
    /// The document identifier.
    pub(crate) document_hash: &'a str,
    /// Reading progress position string.
    pub(crate) progress: &'a str,
    /// Reading progress as a fraction.
    pub(crate) percentage: f64,
    /// Human-readable device name.
    pub(crate) device: &'a str,
    /// Unique device identifier, if known.
    pub(crate) device_id: Option<&'a str>,
    /// Server-assigned Unix timestamp (seconds).
    pub(crate) timestamp: i64,
    /// Document title sent via `KOReader` metadata, if known.
    pub(crate) title: Option<&'a str>,
    /// Authors sent via `KOReader` metadata, if known.
    pub(crate) authors: Option<&'a str>,
    /// File name sent via `KOReader` metadata, if known.
    pub(crate) filename: Option<&'a str>,
}

/// Insert or update the stored progress for a document and record an immutable
/// event in the progress history.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn record_progress(
    pool: &SqlitePool,
    user_id: i64,
    input: &ProgressInput<'_>,
) -> Result<(), StoreError> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO documents (user_id, document_hash, progress, percentage, device, device_id,
                                timestamp, title, authors, filename)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (user_id, document_hash)
         DO UPDATE SET progress = excluded.progress,
                       percentage = excluded.percentage,
                       device = excluded.device,
                       device_id = excluded.device_id,
                       timestamp = excluded.timestamp,
                       title = COALESCE(excluded.title, documents.title),
                       authors = COALESCE(excluded.authors, documents.authors),
                       filename = COALESCE(excluded.filename, documents.filename)",
    )
    .bind(user_id)
    .bind(input.document_hash)
    .bind(input.progress)
    .bind(input.percentage)
    .bind(input.device)
    .bind(input.device_id)
    .bind(input.timestamp)
    .bind(input.title)
    .bind(input.authors)
    .bind(input.filename)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO progress_events (user_id, document_hash, progress, percentage, device, device_id, timestamp)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(input.document_hash)
    .bind(input.progress)
    .bind(input.percentage)
    .bind(input.device)
    .bind(input.device_id)
    .bind(input.timestamp)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

/// Fetch stored progress for a document.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_progress(
    pool: &SqlitePool,
    user_id: i64,
    document_hash: &str,
) -> Result<Option<Document>, StoreError> {
    sqlx::query_as::<_, Document>(
        "SELECT document_hash, progress, percentage, device, device_id, timestamp,
                title, authors, filename
         FROM documents WHERE user_id = ? AND document_hash = ?",
    )
    .bind(user_id)
    .bind(document_hash)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::from)
}

/// Fetch a user's sync progress for a document by its metadata file name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_progress_by_filename(
    pool: &SqlitePool,
    user_id: i64,
    filename: &str,
) -> Result<Option<Document>, StoreError> {
    sqlx::query_as::<_, Document>(
        "SELECT document_hash, progress, percentage, device, device_id, timestamp,
                title, authors, filename
         FROM documents WHERE user_id = ? AND filename = ?",
    )
    .bind(user_id)
    .bind(filename)
    .fetch_optional(pool)
    .await
    .map_err(StoreError::from)
}

/// Fetch a user's sync progress for a publication, matching on the binary
/// digest first, then the file-name digest, then the literal file name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_progress_for_publication(
    pool: &SqlitePool,
    user_id: i64,
    publication: &Publication,
) -> Result<Option<Document>, StoreError> {
    if let Some(doc) = get_progress(pool, user_id, &publication.document_hash).await? {
        return Ok(Some(doc));
    }

    if let Some(name_hash) = publication.file_name_hash.as_deref()
        && let Some(doc) = get_progress(pool, user_id, name_hash).await?
    {
        return Ok(Some(doc));
    }

    if let Some(file_name) = publication.file_name.as_deref()
        && let Some(doc) = get_progress_by_filename(pool, user_id, file_name).await?
    {
        return Ok(Some(doc));
    }

    Ok(None)
}

/// Fetch the recorded progress history for a document, oldest first.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_document_history(
    pool: &SqlitePool,
    user_id: i64,
    document_hash: &str,
) -> Result<Vec<ProgressEvent>, StoreError> {
    let events = sqlx::query_as::<_, ProgressEvent>(
        "SELECT id, document_hash, progress, percentage, device, device_id, timestamp
         FROM progress_events WHERE user_id = ? AND document_hash = ?
         ORDER BY timestamp, id",
    )
    .bind(user_id)
    .bind(document_hash)
    .fetch_all(pool)
    .await?;

    Ok(events)
}

/// Compute a document timeline from its progress events.
///
/// Events are ordered by `(timestamp, id)` and each positive percentage
/// increase is attributed to the device that pushed the document forward,
/// using a running high-water mark.
pub(crate) fn compute_timeline(events: &[ProgressEvent]) -> DocumentTimeline {
    let mut sorted: Vec<&ProgressEvent> = events.iter().collect();
    sorted.sort_by_key(|a| (a.timestamp, a.id));

    let first_recorded = sorted.first().map(|e| e.timestamp);
    let last_recorded = sorted.last().map(|e| e.timestamp);

    let mut high_water = 0.0;
    let mut devices: Vec<DeviceContribution> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();

    for event in sorted {
        let key = event
            .device_id
            .clone()
            .unwrap_or_else(|| event.device.clone());

        let idx = if let Some(&i) = index_by_key.get(&key) {
            i
        } else {
            let i = devices.len();
            index_by_key.insert(key.clone(), i);
            devices.push(DeviceContribution {
                device: event.device.clone(),
                device_id: event.device_id.clone(),
                first_seen: event.timestamp,
                last_seen: event.timestamp,
                sync_count: 0,
                contribution: 0.0,
            });
            i
        };

        let device = &mut devices[idx];
        device.first_seen = device.first_seen.min(event.timestamp);
        device.last_seen = device.last_seen.max(event.timestamp);
        device.sync_count += 1;

        let delta = (event.percentage - high_water).max(0.0);
        if delta > 0.0 {
            device.contribution += delta;
            high_water = event.percentage;
        }
        high_water = high_water.max(event.percentage);
    }

    DocumentTimeline {
        first_recorded,
        last_recorded,
        sync_count: events.len(),
        devices,
    }
}

/// Seconds in a UTC day.
const DAY_SECS: i64 = 86_400;

/// Number of days covered by [`ReadingStats::daily`].
pub(crate) const ACTIVITY_DAYS: i64 = 30;

/// Aggregated reading statistics for a user's dashboard.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ReadingStats {
    /// Documents at or above the finished threshold.
    pub(crate) finished: i64,
    /// Documents below the finished threshold.
    pub(crate) in_progress: i64,
    /// Sync events recorded in the last 7 days.
    pub(crate) syncs_7d: i64,
    /// Sync events recorded in the last [`ACTIVITY_DAYS`] days.
    pub(crate) syncs_30d: i64,
    /// Distinct devices that synced in the last [`ACTIVITY_DAYS`] days.
    pub(crate) devices_30d: i64,
    /// Unix day number (days since the epoch) of the first entry in `daily`.
    pub(crate) first_day: i64,
    /// Sync counts per UTC day, oldest first, ending on the day of `now`.
    pub(crate) daily: Vec<i64>,
}

/// Expand sparse `(day, count)` rows into a dense, zero-filled series of
/// `days` entries ending on `last_day` (inclusive).
pub(crate) fn fill_days(rows: &[(i64, i64)], last_day: i64, days: i64) -> Vec<i64> {
    let first_day = last_day - days + 1;
    let mut series = vec![0; usize::try_from(days.max(0)).unwrap_or(0)];
    for &(day, count) in rows {
        if let Ok(index) = usize::try_from(day - first_day)
            && let Some(slot) = series.get_mut(index)
        {
            *slot += count;
        }
    }
    series
}

/// Compute dashboard statistics for a user as of `now` (Unix seconds).
///
/// # Errors
///
/// Returns an error if any query fails.
pub(crate) async fn reading_stats(
    pool: &SqlitePool,
    user_id: i64,
    now: i64,
) -> Result<ReadingStats, StoreError> {
    let (finished, in_progress) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT COALESCE(SUM(percentage >= 0.99), 0), COALESCE(SUM(percentage < 0.99), 0)
         FROM documents WHERE user_id = ?",
    )
    .bind(user_id)
    .fetch_one(pool)
    .await?;

    let today = now.div_euclid(DAY_SECS);
    let first_day = today - ACTIVITY_DAYS + 1;
    let since_30d = first_day * DAY_SECS;
    let since_7d = now - 7 * DAY_SECS;

    let (syncs_7d, syncs_30d, devices_30d) = sqlx::query_as::<_, (i64, i64, i64)>(
        "SELECT COALESCE(SUM(timestamp >= ?), 0), COUNT(*),
                COUNT(DISTINCT COALESCE(device_id, device))
         FROM progress_events WHERE user_id = ? AND timestamp >= ?",
    )
    .bind(since_7d)
    .bind(user_id)
    .bind(since_30d)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, (i64, i64)>(
        "SELECT timestamp / 86400 AS day, COUNT(*) FROM progress_events
         WHERE user_id = ? AND timestamp >= ? GROUP BY day",
    )
    .bind(user_id)
    .bind(since_30d)
    .fetch_all(pool)
    .await?;

    Ok(ReadingStats {
        finished,
        in_progress,
        syncs_7d,
        syncs_30d,
        devices_30d,
        first_day,
        daily: fill_days(&rows, today, ACTIVITY_DAYS),
    })
}

/// List all users with their document counts.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_users(pool: &SqlitePool) -> Result<Vec<UserSummary>, StoreError> {
    let users = sqlx::query_as::<_, UserSummary>(
        "SELECT u.id, u.username, u.is_administrator, u.is_active, u.can_upload,
                COUNT(d.id) AS document_count, MAX(d.timestamp) AS last_synced
         FROM users u LEFT JOIN documents d ON d.user_id = u.id
         GROUP BY u.id
         ORDER BY u.id",
    )
    .fetch_all(pool)
    .await?;

    Ok(users)
}

/// Delete a user by username.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn delete_user(pool: &SqlitePool, username: &str) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM users WHERE username = ?")
        .bind(username)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// List all documents stored for a user, most recently synced first.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_documents(
    pool: &SqlitePool,
    user_id: i64,
) -> Result<Vec<Document>, StoreError> {
    let documents = sqlx::query_as::<_, Document>(
        "SELECT document_hash, progress, percentage, device, device_id, timestamp,
                title, authors, filename
         FROM documents WHERE user_id = ? ORDER BY timestamp DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;

    Ok(documents)
}

/// Sort order for the progress dashboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DocumentSort {
    /// Most recently synced first (default).
    #[default]
    Recent,
    /// Alphabetical by title, then most recently synced.
    Title,
}

/// Search, status, and sort options for the progress dashboard.
#[derive(Debug, Default)]
pub(crate) struct DocumentFilter<'a> {
    /// Free-text search over title, authors, filename, and hash.
    pub(crate) search: Option<&'a str>,
    /// `Some(true)` finished, `Some(false)` in progress, `None` for any.
    pub(crate) finished: Option<bool>,
    /// Sort order.
    pub(crate) sort: DocumentSort,
    /// The user whose documents are listed.
    pub(crate) user_id: i64,
}

/// Build the shared `WHERE` clause and bound values for a document filter.
fn document_filter_sql(filter: &DocumentFilter<'_>) -> (String, Vec<BindValue>) {
    let mut where_clause = String::from(" WHERE user_id = ?");
    let mut binds = vec![BindValue::Int(filter.user_id)];

    if let Some(term) = filter.search {
        let pattern = format!("%{term}%");
        where_clause.push_str(
            " AND (title LIKE ? OR authors LIKE ? OR filename LIKE ? OR document_hash LIKE ?)",
        );
        binds.push(BindValue::Text(pattern.clone()));
        binds.push(BindValue::Text(pattern.clone()));
        binds.push(BindValue::Text(pattern.clone()));
        binds.push(BindValue::Text(pattern));
    }

    match filter.finished {
        None => {}
        Some(true) => where_clause.push_str(" AND percentage >= 0.99"),
        Some(false) => where_clause.push_str(" AND percentage < 0.99"),
    }

    (where_clause, binds)
}

/// Map a document sort option to its SQL `ORDER BY` clause.
fn document_order_by(sort: DocumentSort) -> &'static str {
    match sort {
        DocumentSort::Recent => "ORDER BY timestamp DESC",
        DocumentSort::Title => "ORDER BY COALESCE(title, '') COLLATE NOCASE, timestamp DESC",
    }
}

/// Count a user's documents matching a filter.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn count_documents_filtered(
    pool: &SqlitePool,
    filter: &DocumentFilter<'_>,
) -> Result<i64, StoreError> {
    let (where_clause, binds) = document_filter_sql(filter);
    let sql = format!("SELECT COUNT(*) FROM documents{where_clause}");
    let query = bind_values!(
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)),
        binds
    );
    Ok(query.fetch_one(pool).await?)
}

/// List a page of a user's documents matching a filter.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_documents_filtered(
    pool: &SqlitePool,
    filter: &DocumentFilter<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Document>, StoreError> {
    let (where_clause, mut binds) = document_filter_sql(filter);
    let order = document_order_by(filter.sort);
    let sql = format!(
        "SELECT document_hash, progress, percentage, device, device_id, timestamp,
                title, authors, filename
         FROM documents{where_clause} {order} LIMIT ? OFFSET ?"
    );
    binds.push(BindValue::Int(limit));
    binds.push(BindValue::Int(offset));
    let query = bind_values!(
        sqlx::query_as::<_, Document>(sqlx::AssertSqlSafe(sql)),
        binds
    );
    Ok(query.fetch_all(pool).await?)
}

/// Delete a single document for a user.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn delete_document(
    pool: &SqlitePool,
    user_id: i64,
    document_hash: &str,
) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM documents WHERE user_id = ? AND document_hash = ?")
        .bind(user_id)
        .bind(document_hash)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Toggle the active flag for a user, returning the new value.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn toggle_active(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<bool>, StoreError> {
    let result = sqlx::query_scalar::<_, bool>(
        "UPDATE users SET is_active = NOT is_active WHERE username = ? RETURNING is_active",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

/// Toggle the upload permission for a user, returning the new value.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn toggle_can_upload(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<bool>, StoreError> {
    let result = sqlx::query_scalar::<_, bool>(
        "UPDATE users SET can_upload = NOT can_upload WHERE username = ? RETURNING can_upload",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(result)
}

/// Update the stored password hash for a user.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn set_password(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
) -> Result<bool, StoreError> {
    let result = sqlx::query("UPDATE users SET password_hash = ? WHERE username = ?")
        .bind(password_hash)
        .bind(username)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// A stored `EPUB` publication in the shared OPDS library.
#[derive(Debug, Clone, sqlx::FromRow)]
pub(crate) struct Publication {
    /// Internal publication id.
    pub(crate) id: i64,
    /// `KOReader` partial `MD5` document digest (the sync join key).
    pub(crate) document_hash: String,
    /// `MD5` of the original file name (for the "filename" matching mode).
    pub(crate) file_name_hash: Option<String>,
    /// Original file name of the uploaded `EPUB`.
    pub(crate) file_name: Option<String>,
    /// The document title.
    pub(crate) title: String,
    /// Authors serialized as a JSON array string.
    pub(crate) authors: String,
    /// Publication language code.
    pub(crate) language: Option<String>,
    /// Persistent identifier (ISBN or similar).
    pub(crate) identifier: Option<String>,
    /// Publisher name.
    pub(crate) publisher: Option<String>,
    /// Publication date.
    pub(crate) published: Option<String>,
    /// Free-text description.
    pub(crate) description: Option<String>,
    /// Genres (subjects) serialized as a JSON array string.
    pub(crate) genres: String,
    /// Normalized ISBN, if one could be extracted or looked up.
    pub(crate) isbn: Option<String>,
    /// Series name, if the publication belongs to one.
    pub(crate) series: Option<String>,
    /// Position within the series, if known.
    pub(crate) series_index: Option<f64>,
    /// Path of the `EPUB` file, relative to the books directory.
    pub(crate) file_path: String,
    /// Size of the `EPUB` file in bytes.
    pub(crate) file_size: i64,
    /// Path of the cover image, relative to the books directory.
    pub(crate) cover_path: Option<String>,
    /// MIME type of the cover image.
    pub(crate) cover_media_type: Option<String>,
    /// Path of the generated cover thumbnail, relative to the books directory.
    pub(crate) thumb_path: Option<String>,
    /// MIME type of the cover thumbnail.
    pub(crate) thumb_media_type: Option<String>,
    /// Server-assigned creation timestamp (seconds).
    pub(crate) created_at: i64,
    /// Server-assigned last-update timestamp (seconds).
    pub(crate) updated_at: i64,
}

/// Validated input for inserting a publication.
#[derive(Debug)]
pub(crate) struct PublicationInput<'a> {
    /// `KOReader` partial `MD5` document digest.
    pub(crate) document_hash: &'a str,
    /// `MD5` of the original file name.
    pub(crate) file_name_hash: Option<&'a str>,
    /// Original file name of the uploaded `EPUB`.
    pub(crate) file_name: Option<&'a str>,
    /// The document title.
    pub(crate) title: &'a str,
    /// Authors serialized as a JSON array string.
    pub(crate) authors: &'a str,
    /// Publication language code.
    pub(crate) language: Option<&'a str>,
    /// Persistent identifier.
    pub(crate) identifier: Option<&'a str>,
    /// Publisher name.
    pub(crate) publisher: Option<&'a str>,
    /// Publication date.
    pub(crate) published: Option<&'a str>,
    /// Free-text description.
    pub(crate) description: Option<&'a str>,
    /// Genres (subjects) serialized as a JSON array string.
    pub(crate) genres: &'a str,
    /// Normalized ISBN, if known.
    pub(crate) isbn: Option<&'a str>,
    /// Series name, if any.
    pub(crate) series: Option<&'a str>,
    /// Position within the series, if known.
    pub(crate) series_index: Option<f64>,
    /// Path of the `EPUB` file, relative to the books directory.
    pub(crate) file_path: &'a str,
    /// Size of the `EPUB` file in bytes.
    pub(crate) file_size: i64,
    /// Path of the cover image, relative to the books directory.
    pub(crate) cover_path: Option<&'a str>,
    /// MIME type of the cover image.
    pub(crate) cover_media_type: Option<&'a str>,
    /// Path of the cover thumbnail, relative to the books directory.
    pub(crate) thumb_path: Option<&'a str>,
    /// MIME type of the cover thumbnail.
    pub(crate) thumb_media_type: Option<&'a str>,
    /// Server-assigned timestamp (seconds).
    pub(crate) timestamp: i64,
}

macro_rules! publication_query {
    ($tail:literal) => {
        concat!(
            "SELECT publications.id AS id, document_hash, file_name_hash, file_name, title, authors, language, ",
            "identifier, publisher, published, description, genres, isbn, series, series_index, ",
            "file_path, file_size, cover_path, cover_media_type, thumb_path, thumb_media_type, ",
            "created_at, updated_at FROM publications ",
            $tail
        )
    };
}

/// Insert a new publication, rejecting duplicates by `document_hash`.
///
/// # Errors
///
/// Returns [`StoreError::Exists`] if a publication with the same digest already
/// exists, or [`StoreError::Db`] on any storage failure.
pub(crate) async fn insert_publication(
    pool: &SqlitePool,
    input: &PublicationInput<'_>,
) -> Result<i64, StoreError> {
    if get_publication_by_hash(pool, input.document_hash)
        .await?
        .is_some()
    {
        return Err(StoreError::Exists);
    }

    let result = sqlx::query(
        "INSERT INTO publications
         (document_hash, file_name_hash, file_name, title, authors, language,
          identifier, publisher, published, description, genres, isbn, series, series_index,
          file_path, file_size, cover_path, cover_media_type, thumb_path, thumb_media_type,
          created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(input.document_hash)
    .bind(input.file_name_hash)
    .bind(input.file_name)
    .bind(input.title)
    .bind(input.authors)
    .bind(input.language)
    .bind(input.identifier)
    .bind(input.publisher)
    .bind(input.published)
    .bind(input.description)
    .bind(input.genres)
    .bind(input.isbn)
    .bind(input.series)
    .bind(input.series_index)
    .bind(input.file_path)
    .bind(input.file_size)
    .bind(input.cover_path)
    .bind(input.cover_media_type)
    .bind(input.thumb_path)
    .bind(input.thumb_media_type)
    .bind(input.timestamp)
    .bind(input.timestamp)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

/// Fetch a publication by its `KOReader` document digest.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_publication_by_hash(
    pool: &SqlitePool,
    document_hash: &str,
) -> Result<Option<Publication>, StoreError> {
    sqlx::query_as::<_, Publication>(publication_query!("WHERE document_hash = ?"))
        .bind(document_hash)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::from)
}

/// Fetch a publication by its file-name digest (the `KOReader` "Filename"
/// matching method sends `md5(basename)` as the document identifier).
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_publication_by_file_name_hash(
    pool: &SqlitePool,
    file_name_hash: &str,
) -> Result<Option<Publication>, StoreError> {
    sqlx::query_as::<_, Publication>(publication_query!("WHERE file_name_hash = ?"))
        .bind(file_name_hash)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::from)
}

/// Fetch a publication by its original file name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn get_publication_by_file_name(
    pool: &SqlitePool,
    file_name: &str,
) -> Result<Option<Publication>, StoreError> {
    sqlx::query_as::<_, Publication>(publication_query!("WHERE file_name = ?"))
        .bind(file_name)
        .fetch_optional(pool)
        .await
        .map_err(StoreError::from)
}

/// Resolve the publication matching a synced document.
///
/// Tries the binary digest first, then the file-name digest, then the literal
/// file name, mirroring [`get_progress_for_publication`].
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn find_publication_for_document(
    pool: &SqlitePool,
    document: &Document,
) -> Result<Option<Publication>, StoreError> {
    if let Some(publication) = get_publication_by_hash(pool, &document.document_hash).await? {
        return Ok(Some(publication));
    }

    if let Some(publication) =
        get_publication_by_file_name_hash(pool, &document.document_hash).await?
    {
        return Ok(Some(publication));
    }

    if let Some(filename) = document.filename.as_deref()
        && let Some(publication) = get_publication_by_file_name(pool, filename).await?
    {
        return Ok(Some(publication));
    }

    Ok(None)
}

/// Count publications, optionally filtered by a title/author/description search.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn count_publications(
    pool: &SqlitePool,
    search: Option<&str>,
) -> Result<i64, StoreError> {
    let count = match search {
        Some(term) => {
            let pattern = format!("%{term}%");
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM publications
                 WHERE title LIKE ? OR authors LIKE ? OR description LIKE ?",
            )
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern)
            .fetch_one(pool)
            .await?
        }
        None => {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM publications")
                .fetch_one(pool)
                .await?
        }
    };
    Ok(count)
}

/// List a page of publications ordered by title, optionally search-filtered.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_publications_paged(
    pool: &SqlitePool,
    limit: i64,
    offset: i64,
    search: Option<&str>,
) -> Result<Vec<Publication>, StoreError> {
    let publications = match search {
        Some(term) => {
            let pattern = format!("%{term}%");
            sqlx::query_as::<_, Publication>(publication_query!(
                "WHERE title LIKE ? OR authors LIKE ? OR description LIKE ?
                     ORDER BY title LIMIT ? OFFSET ?"
            ))
            .bind(&pattern)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, Publication>(publication_query!("ORDER BY title LIMIT ? OFFSET ?"))
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await?
        }
    };

    Ok(publications)
}

/// Sort order for the filtered library listing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PublicationSort {
    /// Alphabetical by title (default).
    #[default]
    Title,
    /// Alphabetical by author, then title.
    Author,
    /// Most recently added first.
    Recent,
    /// Grouped by series, then series position.
    Series,
}

/// Reading-status filter relative to a user's synced progress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ReadingStatus {
    /// No status restriction.
    #[default]
    Any,
    /// The user has no synced progress for the publication.
    Unread,
    /// The user has synced progress below the finished threshold.
    InProgress,
    /// The user has synced progress at or above the finished threshold.
    Finished,
}

/// Metadata, reading-status, and sort options for the library listing.
#[derive(Debug, Default)]
pub(crate) struct PublicationFilter<'a> {
    /// Free-text search over title, authors, and description.
    pub(crate) search: Option<&'a str>,
    /// Exact author (matches one entry of the JSON `authors` array).
    pub(crate) author: Option<&'a str>,
    /// Exact genre (matches one entry of the JSON `genres` array).
    pub(crate) genre: Option<&'a str>,
    /// Exact series name.
    pub(crate) series: Option<&'a str>,
    /// Exact language code.
    pub(crate) language: Option<&'a str>,
    /// Reading status relative to `user_id`.
    pub(crate) status: ReadingStatus,
    /// Sort order.
    pub(crate) sort: PublicationSort,
    /// The user whose progress defines the reading status.
    pub(crate) user_id: i64,
}

/// Build the shared `WHERE` clause and bound values for a publication filter.
fn publication_filter_sql(filter: &PublicationFilter<'_>) -> (String, Vec<BindValue>) {
    use std::fmt::Write as _;

    /// SQL fragment matching a publication to a user's synced document.
    const MATCH: &str = "(d.document_hash = publications.document_hash \
         OR (publications.file_name_hash IS NOT NULL AND d.document_hash = publications.file_name_hash) \
         OR (publications.file_name IS NOT NULL AND d.filename = publications.file_name))";

    let mut where_clause = String::from(" WHERE 1 = 1");
    let mut binds = Vec::new();

    if let Some(term) = filter.search {
        let pattern = format!("%{term}%");
        where_clause.push_str(" AND (title LIKE ? OR authors LIKE ? OR description LIKE ?)");
        binds.push(BindValue::Text(pattern.clone()));
        binds.push(BindValue::Text(pattern.clone()));
        binds.push(BindValue::Text(pattern));
    }
    if let Some(author) = filter.author {
        where_clause.push_str(
            " AND EXISTS (SELECT 1 FROM json_each(publications.authors) WHERE value = ?)",
        );
        binds.push(BindValue::Text(author.to_owned()));
    }
    if let Some(genre) = filter.genre {
        where_clause
            .push_str(" AND EXISTS (SELECT 1 FROM json_each(publications.genres) WHERE value = ?)");
        binds.push(BindValue::Text(genre.to_owned()));
    }
    if let Some(series) = filter.series {
        where_clause.push_str(" AND series = ?");
        binds.push(BindValue::Text(series.to_owned()));
    }
    if let Some(language) = filter.language {
        where_clause.push_str(" AND language = ?");
        binds.push(BindValue::Text(language.to_owned()));
    }

    match filter.status {
        ReadingStatus::Any => {}
        ReadingStatus::Unread => {
            let _ = write!(
                where_clause,
                " AND NOT EXISTS (SELECT 1 FROM documents d WHERE d.user_id = ? AND {MATCH})"
            );
            binds.push(BindValue::Int(filter.user_id));
        }
        ReadingStatus::InProgress => {
            let _ = write!(
                where_clause,
                " AND EXISTS (SELECT 1 FROM documents d WHERE d.user_id = ? AND d.percentage < 0.99 AND {MATCH})"
            );
            binds.push(BindValue::Int(filter.user_id));
        }
        ReadingStatus::Finished => {
            let _ = write!(
                where_clause,
                " AND EXISTS (SELECT 1 FROM documents d WHERE d.user_id = ? AND d.percentage >= 0.99 AND {MATCH})"
            );
            binds.push(BindValue::Int(filter.user_id));
        }
    }

    (where_clause, binds)
}

/// Map a sort option to its SQL `ORDER BY` clause.
fn publication_order_by(sort: PublicationSort) -> &'static str {
    match sort {
        PublicationSort::Title => "ORDER BY title COLLATE NOCASE",
        PublicationSort::Author => "ORDER BY authors COLLATE NOCASE, title COLLATE NOCASE",
        PublicationSort::Recent => "ORDER BY created_at DESC, title COLLATE NOCASE",
        PublicationSort::Series => {
            "ORDER BY (series IS NULL), series COLLATE NOCASE, series_index, title COLLATE NOCASE"
        }
    }
}

/// Count publications matching a filter.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn count_publications_filtered(
    pool: &SqlitePool,
    filter: &PublicationFilter<'_>,
) -> Result<i64, StoreError> {
    let (where_clause, binds) = publication_filter_sql(filter);
    let sql = format!("SELECT COUNT(*) FROM publications{where_clause}");
    let query = bind_values!(
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql)),
        binds
    );
    Ok(query.fetch_one(pool).await?)
}

/// List a page of publications matching a filter.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_publications_filtered(
    pool: &SqlitePool,
    filter: &PublicationFilter<'_>,
    limit: i64,
    offset: i64,
) -> Result<Vec<Publication>, StoreError> {
    let (where_clause, mut binds) = publication_filter_sql(filter);
    let order = publication_order_by(filter.sort);
    let sql = format!(
        "{} {where_clause} {order} LIMIT ? OFFSET ?",
        publication_query!("")
    );
    binds.push(BindValue::Int(limit));
    binds.push(BindValue::Int(offset));
    let query = bind_values!(
        sqlx::query_as::<_, Publication>(sqlx::AssertSqlSafe(sql)),
        binds
    );
    Ok(query.fetch_all(pool).await?)
}

/// List all distinct, non-empty publication languages with their counts.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_languages(pool: &SqlitePool) -> Result<Vec<GroupCount>, StoreError> {
    sqlx::query_as::<_, GroupCount>(
        "SELECT language AS name, COUNT(*) AS count FROM publications
         WHERE language IS NOT NULL AND language <> ''
         GROUP BY language ORDER BY language COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::from)
}

/// Delete a publication by id, returning whether a row was removed.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn delete_publication(pool: &SqlitePool, id: i64) -> Result<bool, StoreError> {
    let result = sqlx::query("DELETE FROM publications WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// A named grouping of publications (author, genre, or series) with its size.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub(crate) struct GroupCount {
    /// The group name (author, genre, or series).
    pub(crate) name: String,
    /// The number of publications in the group.
    pub(crate) count: i64,
}

/// List all authors with their publication counts, ordered by name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_authors(pool: &SqlitePool) -> Result<Vec<GroupCount>, StoreError> {
    sqlx::query_as::<_, GroupCount>(
        "SELECT value AS name, COUNT(*) AS count
         FROM publications, json_each(publications.authors)
         GROUP BY value ORDER BY value COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::from)
}

/// List all genres with their publication counts, ordered by name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_genres(pool: &SqlitePool) -> Result<Vec<GroupCount>, StoreError> {
    sqlx::query_as::<_, GroupCount>(
        "SELECT value AS name, COUNT(*) AS count
         FROM publications, json_each(publications.genres)
         GROUP BY value ORDER BY value COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::from)
}

/// List all series with their publication counts, ordered by name.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_series(pool: &SqlitePool) -> Result<Vec<GroupCount>, StoreError> {
    sqlx::query_as::<_, GroupCount>(
        "SELECT series AS name, COUNT(*) AS count
         FROM publications WHERE series IS NOT NULL
         GROUP BY series ORDER BY series COLLATE NOCASE",
    )
    .fetch_all(pool)
    .await
    .map_err(StoreError::from)
}

/// The grouping dimension used for grouped publication feeds.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GroupKind {
    /// Group by a single author entry in the JSON `authors` array.
    Author,
    /// Group by a single genre entry in the JSON `genres` array.
    Genre,
    /// Group by the `series` column.
    Series,
}

/// Count the publications in a group.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn count_publications_in_group(
    pool: &SqlitePool,
    kind: GroupKind,
    name: &str,
) -> Result<i64, StoreError> {
    let count = match kind {
        GroupKind::Author => {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM publications, json_each(publications.authors)
                 WHERE value = ?",
            )
            .bind(name)
            .fetch_one(pool)
            .await?
        }
        GroupKind::Genre => {
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM publications, json_each(publications.genres)
                 WHERE value = ?",
            )
            .bind(name)
            .fetch_one(pool)
            .await?
        }
        GroupKind::Series => {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM publications WHERE series = ?")
                .bind(name)
                .fetch_one(pool)
                .await?
        }
    };
    Ok(count)
}

/// List a page of publications in a group. Series groups are ordered by
/// series position, everything else by title.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn list_publications_in_group(
    pool: &SqlitePool,
    kind: GroupKind,
    name: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Publication>, StoreError> {
    let publications =
        match kind {
            GroupKind::Author => sqlx::query_as::<_, Publication>(publication_query!(
                ", json_each(publications.authors) WHERE value = ? ORDER BY title LIMIT ? OFFSET ?"
            ))
            .bind(name)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?,
            GroupKind::Genre => sqlx::query_as::<_, Publication>(publication_query!(
                ", json_each(publications.genres) WHERE value = ? ORDER BY title LIMIT ? OFFSET ?"
            ))
            .bind(name)
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await?,
            GroupKind::Series => {
                sqlx::query_as::<_, Publication>(publication_query!(
                    "WHERE series = ? ORDER BY series_index, title LIMIT ? OFFSET ?"
                ))
                .bind(name)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await?
            }
        };

    Ok(publications)
}

/// The result of a background metadata enrichment pass.
pub(crate) struct EnrichmentUpdate {
    /// Genres (subjects) serialized as a JSON array string.
    pub(crate) genres: String,
    /// Normalized ISBN, if one was found.
    pub(crate) isbn: Option<String>,
    /// Path of a downloaded cover image, relative to the books directory.
    pub(crate) cover_path: Option<String>,
    /// MIME type of the downloaded cover image.
    pub(crate) cover_media_type: Option<String>,
    /// Path of the cover thumbnail, relative to the books directory.
    pub(crate) thumb_path: Option<String>,
    /// MIME type of the cover thumbnail.
    pub(crate) thumb_media_type: Option<String>,
    /// Server-assigned update timestamp (seconds).
    pub(crate) timestamp: i64,
}

/// Apply the result of a background metadata enrichment to a publication.
///
/// Genres and ISBN are always written (the caller merges embedded and
/// external values); cover columns are only touched when `cover_path` is set,
/// so an existing embedded cover is never overwritten.
///
/// # Errors
///
/// Returns an error if the query fails.
pub(crate) async fn update_publication_enrichment(
    pool: &SqlitePool,
    document_hash: &str,
    update: &EnrichmentUpdate,
) -> Result<bool, StoreError> {
    let result = if update.cover_path.is_some() {
        sqlx::query(
            "UPDATE publications SET genres = ?, isbn = ?, cover_path = ?, cover_media_type = ?,
                    thumb_path = ?, thumb_media_type = ?, updated_at = ?
             WHERE document_hash = ?",
        )
        .bind(&update.genres)
        .bind(update.isbn.as_deref())
        .bind(update.cover_path.as_deref())
        .bind(update.cover_media_type.as_deref())
        .bind(update.thumb_path.as_deref())
        .bind(update.thumb_media_type.as_deref())
        .bind(update.timestamp)
        .bind(document_hash)
        .execute(pool)
        .await?
    } else {
        sqlx::query(
            "UPDATE publications SET genres = ?, isbn = COALESCE(?, isbn), updated_at = ?
             WHERE document_hash = ?",
        )
        .bind(&update.genres)
        .bind(update.isbn.as_deref())
        .bind(update.timestamp)
        .bind(document_hash)
        .execute(pool)
        .await?
    };
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{ProgressEvent, compute_timeline, fill_days};

    fn event(id: i64, ts: i64, pct: f64, device: &str, device_id: Option<&str>) -> ProgressEvent {
        ProgressEvent {
            id,
            document_hash: "doc".to_owned(),
            progress: String::new(),
            percentage: pct,
            device: device.to_owned(),
            device_id: device_id.map(str::to_owned),
            timestamp: ts,
        }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn contributions_are_attributed_to_forward_progress() {
        let events = vec![
            event(1, 100, 0.4, "kindle", Some("dev1")),
            event(2, 200, 0.7, "kobo", Some("dev2")),
            event(3, 300, 0.5, "kindle", Some("dev1")),
        ];
        let t = compute_timeline(&events);
        assert_eq!(t.first_recorded, Some(100));
        assert_eq!(t.last_recorded, Some(300));
        assert_eq!(t.sync_count, 3);
        assert_eq!(t.devices.len(), 2);

        let kindle = t
            .devices
            .iter()
            .find(|d| d.device_id.as_deref() == Some("dev1"))
            .unwrap();
        let kobo = t
            .devices
            .iter()
            .find(|d| d.device_id.as_deref() == Some("dev2"))
            .unwrap();
        assert!(close(kindle.contribution, 0.4));
        assert!(close(kobo.contribution, 0.3));
        assert_eq!(kindle.sync_count, 2);
        assert_eq!(kobo.sync_count, 1);
    }

    #[test]
    fn devices_without_id_group_by_name() {
        let events = vec![
            event(1, 100, 0.2, "kindle", None),
            event(2, 200, 0.6, "kindle", None),
        ];
        let t = compute_timeline(&events);
        assert_eq!(t.devices.len(), 1);
        assert!(close(t.devices[0].contribution, 0.6));
    }

    #[test]
    fn empty_history_yields_empty_timeline() {
        let t = compute_timeline(&[]);
        assert_eq!(t.first_recorded, None);
        assert_eq!(t.last_recorded, None);
        assert_eq!(t.sync_count, 0);
        assert!(t.devices.is_empty());
    }

    #[test]
    fn fill_days_zero_fills_and_drops_out_of_range_rows() {
        let rows = [(98, 2), (100, 5), (90, 7), (101, 1)];
        assert_eq!(fill_days(&rows, 100, 3), vec![2, 0, 5]);
    }

    #[test]
    fn fill_days_handles_empty_input() {
        assert_eq!(fill_days(&[], 10, 4), vec![0, 0, 0, 0]);
    }
}
