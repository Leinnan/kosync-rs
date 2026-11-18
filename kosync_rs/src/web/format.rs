//! Dates, durations, percentages, and sizes in the page's language.

use time::OffsetDateTime;

use crate::i18n::Tr;

/// A short absolute date, e.g. `Aug 13, 2026` or `13 sie 2026`.
pub(crate) fn human_date(tr: &Tr<'_>, dt: OffsetDateTime) -> String {
    let month = tr.t(&format!("month_short_{}", u8::from(dt.month())));
    tr.args(
        "date_short",
        &[
            ("day", &dt.day().to_string()),
            ("month", &month),
            ("year", &dt.year().to_string()),
        ],
    )
}

/// A relative phrase such as `2 hours ago`, or a date when it is more than
/// thirty days away.
pub(crate) fn relative_time(tr: &Tr<'_>, dt: OffsetDateTime) -> String {
    relative_to(tr, dt, OffsetDateTime::now_utc())
}

/// [`relative_time`] measured from `now`.
fn relative_to(tr: &Tr<'_>, dt: OffsetDateTime, now: OffsetDateTime) -> String {
    let secs = (now - dt).whole_seconds();
    let (future, secs) = (secs < 0, secs.abs());
    let amount = match secs {
        ..60 => return tr.t(if future { "time_soon" } else { "time_just_now" }),
        60..3_600 => tr.n("time_minutes", secs / 60, &[]),
        3_600..86_400 => tr.n("time_hours", secs / 3_600, &[]),
        86_400..2_592_000 => tr.n("time_days", secs / 86_400, &[]),
        _ => return human_date(tr, dt),
    };
    tr.args(
        if future { "time_in" } else { "time_ago" },
        &[("amount", &amount)],
    )
}

/// A progress fraction as a whole percentage, e.g. `0.437` → `43%`.
///
/// Values are floored so an almost-finished book never reads `100%`, except
/// at or above the 0.99 threshold the rest of the app treats as finished.
pub(crate) fn pct(tr: &Tr<'_>, fraction: f64) -> String {
    let whole = if fraction >= 0.99 {
        100.0
    } else {
        (fraction.max(0.0) * 100.0).floor()
    };
    tr.args("percent", &[("value", &format!("{whole:.0}"))])
}

/// A fraction as a percentage with `decimals` digits, e.g. `43.70%` or
/// `43,70%`.
pub(crate) fn pct_precise(tr: &Tr<'_>, fraction: f64, decimals: usize) -> String {
    let value = tr.decimal(&format!("{:.decimals$}", fraction * 100.0));
    tr.args("percent", &[("value", &value)])
}

/// A byte count as a human-readable size, e.g. `1.4 MB` or `1,4 MB`.
pub(crate) fn size(tr: &Tr<'_>, bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];

    let bytes = u64::try_from(bytes.max(0)).unwrap_or(0);
    let mut value = bytes;
    let mut unit = 0usize;

    while value >= 1000 && unit + 1 < UNITS.len() {
        value /= 1000;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        let divisor = 1000u64.pow(u32::try_from(unit).unwrap_or(0));
        let whole = bytes / divisor;
        let tenths = (bytes % divisor) * 10 / divisor;
        let number = tr.decimal(&format!("{whole}.{tenths}"));
        format!("{number} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use time::{Duration, macros::datetime};
    use web_mev_i18n::LocaleCode;

    use super::{human_date, pct, pct_precise, relative_to, size};
    use crate::i18n::I18n;

    fn with<R>(code: &str, check: impl FnOnce(&crate::i18n::Tr<'_>) -> R) -> R {
        let i18n = I18n::load("en").unwrap();
        let tr = i18n.tr(&LocaleCode::try_from(code).unwrap());
        check(&tr)
    }

    #[test]
    fn dates_follow_the_language() {
        let day = datetime!(2026-08-13 10:00 UTC);
        assert_eq!(with("en", |tr| human_date(tr, day)), "Aug 13, 2026");
        assert_eq!(with("pl", |tr| human_date(tr, day)), "13 sie 2026");
    }

    #[test]
    fn relative_times_use_plural_forms() {
        let now = datetime!(2026-08-13 10:00 UTC);
        let ago = |code: &str, minutes: i64| {
            with(code, |tr| {
                relative_to(tr, now - Duration::minutes(minutes), now)
            })
        };
        assert_eq!(ago("en", 0), "just now");
        assert_eq!(ago("en", 1), "1 minute ago");
        assert_eq!(ago("en", 5), "5 minutes ago");
        assert_eq!(ago("pl", 1), "1 minutę temu");
        assert_eq!(ago("pl", 3), "3 minuty temu");
        assert_eq!(ago("pl", 5), "5 minut temu");
        assert_eq!(ago("pl", 0), "przed chwilą");
        assert_eq!(ago("pl", 60 * 24 * 2), "2 dni temu");
        assert_eq!(
            with("pl", |tr| relative_to(tr, now + Duration::hours(2), now)),
            "za 2 godziny"
        );
    }

    #[test]
    fn numbers_use_the_decimal_separator() {
        assert_eq!(with("en", |tr| size(tr, 1_450_000)), "1.4 MB");
        assert_eq!(with("pl", |tr| size(tr, 1_450_000)), "1,4 MB");
        assert_eq!(with("en", |tr| pct_precise(tr, 0.4371, 2)), "43.71%");
        assert_eq!(with("pl", |tr| pct_precise(tr, 0.4371, 2)), "43,71%");
        assert_eq!(with("pl", |tr| pct(tr, 0.4371)), "43%");
    }
}
