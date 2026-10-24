//! Calendar dates as days since 1970-01-01, enough to tell how old a PR is.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn today() -> i64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    i64::try_from(secs / 86_400).unwrap_or_default()
}

/// The date of a timestamp like GitHub's `2026-10-24T03:54:25Z`.
pub fn parse(timestamp: &str) -> Option<i64> {
    let date = timestamp.get(..10)?;
    let mut parts = date.split('-').map(str::parse::<i64>);
    let (y, m, d) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    // Howard Hinnant's days_from_civil, with years starting in March.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `YYYY-MM-DD`, the inverse of [`parse`].
pub fn format(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2000-03-01"), Some(11_017));
        assert_eq!(parse("2026-10-24T03:54:25Z"), Some(20_750));
        for days in [0, 11_016, 11_017, 20_750, 20_877] {
            assert_eq!(parse(&format(days)), Some(days));
        }
        assert_eq!(parse("garbage"), None);
    }
}
