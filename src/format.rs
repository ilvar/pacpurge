//! Human-readable rendering of sizes and timestamps.
//!
//! Deliberately integer-only. The strict subset bans `as` casts, and there is
//! no `From<u64> for f64`, so byte sizes are scaled with integer arithmetic
//! rather than converted to floating point. The result is also exactly
//! reproducible, which matters for the golden tests.

/// Binary size units, largest last.
const UNITS: [(u64, &str); 5] = [
    (1, "B"),
    (1 << 10, "KiB"),
    (1 << 20, "MiB"),
    (1 << 30, "GiB"),
    (1 << 40, "TiB"),
];

/// Render a byte count as e.g. `1.5 GiB`.
///
/// Values below a kibibyte are rendered without a fractional part. Everything
/// else gets one decimal place, truncated rather than rounded so that a
/// reported figure is never larger than the real one.
pub fn bytes(value: u64) -> String {
    let mut chosen = (1u64, "B");
    for unit in UNITS {
        let (scale, _label) = unit;
        if value >= scale {
            chosen = unit;
        }
    }

    let (scale, label) = chosen;
    if scale == 1 {
        return format!("{value} {label}");
    }

    let whole = value / scale;
    let remainder = value.saturating_sub(whole.saturating_mul(scale));
    let tenths = remainder.saturating_mul(10) / scale;
    format!("{whole}.{tenths} {label}")
}

/// Render a byte count right-padded to a fixed width for table columns.
pub fn bytes_padded(value: u64, width: usize) -> String {
    let rendered = bytes(value);
    format!("{rendered:>width$}")
}

/// Seconds in a day.
const DAY: i64 = 86_400;

/// Render the age of `timestamp` relative to `now` as e.g. `41d` or `2.1y`.
///
/// Returns `-` for a missing timestamp and `?` for one in the future, which
/// happens on systems whose clock has moved backwards.
pub fn age(now: i64, timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "-".to_owned();
    };

    let Some(elapsed) = now.checked_sub(timestamp) else {
        return "?".to_owned();
    };

    if elapsed < 0 {
        return "?".to_owned();
    }

    let days = elapsed / DAY;
    if days < 1 {
        return "today".to_owned();
    }
    if days < 365 {
        return format!("{days}d");
    }

    let years = days / 365;
    let tenths = (days % 365) * 10 / 365;
    format!("{years}.{tenths}y")
}

/// Whole days between `timestamp` and `now`, saturating at zero.
pub fn days_since(now: i64, timestamp: i64) -> i64 {
    now.saturating_sub(timestamp).max(0) / DAY
}

/// Render a Unix timestamp as an ISO-8601 calendar date in UTC.
///
/// Uses Howard Hinnant's `civil_from_days` algorithm so that no date library
/// is needed. Only the date is shown; the time of day is noise at the
/// granularity this tool reports on.
pub fn date(timestamp: Option<i64>) -> String {
    let Some(timestamp) = timestamp else {
        return "-".to_owned();
    };

    let days = timestamp.div_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since the Unix epoch into a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Truncate `text` to `width` display columns, marking the cut with an ellipsis.
///
/// Counts `char`s rather than grapheme clusters. Package names and paths are
/// ASCII in practice, and avoiding a Unicode segmentation dependency keeps the
/// binary small.
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if text.chars().count() <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }

    let kept: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Render a percentage of `total` as an integer, guarding against a zero total.
pub fn percent(part: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    part.saturating_mul(100) / total
}

#[cfg(test)]
mod tests {
    use super::{age, bytes, civil_from_days, date, days_since, percent, truncate};

    #[test]
    fn bytes_uses_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024 / 2), "1.5 GiB");
    }

    #[test]
    fn bytes_truncates_rather_than_rounds() {
        // 1.99 KiB must never be reported as 2.0 KiB: a reclaim figure that
        // rounds up promises space that is not there.
        assert_eq!(bytes(2038), "1.9 KiB");
    }

    #[test]
    fn age_reports_days_then_years() {
        let now = 1_700_000_000;
        assert_eq!(age(now, None), "-");
        assert_eq!(age(now, Some(now)), "today");
        assert_eq!(age(now, Some(now - 86_400 * 41)), "41d");
        assert_eq!(age(now, Some(now - 86_400 * 400)), "1.0y");
        assert_eq!(age(now, Some(now + 500)), "?");
    }

    #[test]
    fn days_since_saturates_at_zero() {
        assert_eq!(days_since(100, 200), 0);
        assert_eq!(days_since(86_400 * 3, 0), 3);
    }

    #[test]
    fn civil_dates_match_known_values() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        // 2024 is a leap year; day 60 of the year must be February 29.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn dates_render_iso() {
        assert_eq!(date(None), "-");
        assert_eq!(date(Some(0)), "1970-01-01");
        assert_eq!(date(Some(1_700_000_000)), "2023-11-14");
    }

    #[test]
    fn truncate_marks_the_cut() {
        assert_eq!(truncate("ripgrep", 10), "ripgrep");
        assert_eq!(truncate("ripgrep", 7), "ripgrep");
        assert_eq!(truncate("ripgrep", 4), "rip…");
        assert_eq!(truncate("ripgrep", 1), "…");
        assert_eq!(truncate("ripgrep", 0), "");
    }

    #[test]
    fn percent_guards_zero_total() {
        assert_eq!(percent(5, 0), 0);
        assert_eq!(percent(1, 4), 25);
    }
}
