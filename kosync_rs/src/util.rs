//! Small shared helpers.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current Unix timestamp in seconds, or `0` if the clock is before the epoch.
pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// Whether `field` is a valid sync key field: non-empty and free of colons.
pub(crate) fn is_valid_key_field(field: &str) -> bool {
    !field.is_empty() && !field.contains(':')
}

/// Whether `field` is a valid plain field: non-empty.
pub(crate) fn is_valid_field(field: &str) -> bool {
    !field.is_empty()
}
