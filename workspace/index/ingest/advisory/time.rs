//! RFC3339 instants as unix milliseconds, shared by the OSV and RustSec
//! parsers.

/// `YYYY-MM-DDTHH:MM:SS` with a trailing `Z` or numeric offset, as unix millis.
/// A date that is not that shape is ignored.
pub(in crate::ingest::advisory) fn rfc3339_millis(raw: &str) -> Option<i64> {
    let (date, time) = raw.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '-']).next()?;
    let mut time_parts = time.split(':');
    let hour: u32 = time_parts.next()?.parse().ok()?;
    let minute: u32 = time_parts.next()?.parse().ok()?;
    let second: u32 = time_parts.next()?.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_before_year(year) + days_before_month(year, month) + i64::from(day - 1);
    let secs = days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second);
    Some(secs.saturating_mul(1_000))
}

fn days_before_year(year: i64) -> i64 {
    let y = year - 1;
    y * 365 + y / 4 - y / 100 + y / 400
}

fn days_before_month(year: i64, month: u32) -> i64 {
    const CUMULATIVE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut days = CUMULATIVE[(month - 1) as usize];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    if leap && month > 2 {
        days += 1;
    }
    days
}
