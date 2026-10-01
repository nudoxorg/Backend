//! Versions as the marks read them: semver parts, caret classes (what Cargo
//! calls compatible), tick kinds, ages and the one-line semver reading
//! ("28 releases behind · two of them breaking · 15 months").
//!
//! Pure functions over version text and ISO dates; nothing here knows the
//! engine or GPUI, so every sentence a card says is testable on its own.

use std::cmp::Ordering;

/// One release as the registry records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseFact {
    /// The version as published (`1.1.5+spec-1.1.0`).
    pub v: String,
    /// When it was published (ISO 8601), when the registry says.
    pub at: Option<String>,
    /// Withdrawn by its author.
    pub yanked: bool,
}

impl ReleaseFact {
    /// A release at `v`, published `at`.
    #[must_use]
    pub fn new(v: impl Into<String>, at: Option<&str>, yanked: bool) -> Self {
        Self {
            v: v.into(),
            at: at.map(str::to_owned),
            yanked,
        }
    }
}

/// A version's semver parts (`v` prefixes and missing parts read as 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ver {
    /// Major.
    pub major: u64,
    /// Minor.
    pub minor: u64,
    /// Patch.
    pub patch: u64,
    /// Pre-release tag (`alpha.5`), empty for a release.
    pub pre: String,
}

/// Reads `v`'s parts; build metadata (`+spec-1.1.0`) is ignored.
#[must_use]
pub fn parse(v: &str) -> Ver {
    let core = v.split('+').next().unwrap_or(v);
    let (nums, pre) = core.split_once('-').unwrap_or((core, ""));
    let mut parts = nums
        .trim_start_matches('v')
        .split('.')
        .map(|n| n.parse::<u64>().unwrap_or(0));
    Ver {
        major: parts.next().unwrap_or(0),
        minor: parts.next().unwrap_or(0),
        patch: parts.next().unwrap_or(0),
        pre: pre.to_owned(),
    }
}

/// Semver order: numbers, then a pre-release before its release.
#[must_use]
pub fn cmp(a: &str, b: &str) -> Ordering {
    let (x, y) = (parse(a), parse(b));
    x.major
        .cmp(&y.major)
        .then(x.minor.cmp(&y.minor))
        .then(x.patch.cmp(&y.patch))
        .then_with(|| match (x.pre.is_empty(), y.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => x.pre.cmp(&y.pre),
        })
}

/// The version without build metadata (`1.1.5+spec-1.1.0` → `1.1.5`).
#[must_use]
pub fn short(v: &str) -> &str {
    v.split('+').next().unwrap_or(v)
}

/// The caret class a version belongs to: releases in one class are
/// compatible (`1`, `0.8`, `0.0.3`).
#[must_use]
pub fn caret(v: &str) -> String {
    let s = parse(v);
    if s.major > 0 {
        format!("{}", s.major)
    } else if s.minor > 0 {
        format!("0.{}", s.minor)
    } else {
        format!("0.0.{}", s.patch)
    }
}

/// How a release stands on the comb.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Tick {
    /// Same caret class and minor as the release before.
    Patch,
    /// Same caret class, a new minor.
    Minor,
    /// A new caret class: a breaking release.
    Breaking,
    /// A pre-release.
    Pre,
}

/// The tick kind of every version in `versions` (ascending).
#[must_use]
pub fn kinds(versions: &[&str]) -> Vec<Tick> {
    let mut previous: Option<&str> = None;
    versions
        .iter()
        .map(|v| {
            let s = parse(v);
            if !s.pre.is_empty() {
                return Tick::Pre;
            }
            let kind = match previous {
                None => Tick::Breaking,
                Some(p) if caret(p) != caret(v) => Tick::Breaking,
                Some(p) => {
                    let q = parse(p);
                    if q.major == s.major && q.minor == s.minor {
                        Tick::Patch
                    } else {
                        Tick::Minor
                    }
                }
            };
            previous = Some(v);
            kind
        })
        .collect()
}

/// Days since 1970-01-01 of an ISO date (`2014-11-11T05:37:45Z` or
/// `2026-09-27`), when it parses.
#[must_use]
pub fn days(iso: &str) -> Option<f64> {
    let (date, time) = iso.split_once('T').unwrap_or((iso, ""));
    let mut ymd = date.split('-').map(|n| n.parse::<i64>().ok());
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    // Howard Hinnant's days-from-civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let whole = era * 146_097 + doe - 719_468;
    let mut hms = time
        .trim_end_matches('Z')
        .split(':')
        .map(|n| n.parse::<f64>().unwrap_or(0.0));
    let secs = hms.next().unwrap_or(0.0) * 3600.0
        + hms.next().unwrap_or(0.0) * 60.0
        + hms.next().unwrap_or(0.0);
    #[allow(clippy::cast_precision_loss)]
    Some(whole as f64 + secs / 86_400.0)
}

/// `n` with thousands separators (`1,189`).
#[must_use]
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `n one` / `n ones`, the number with separators.
#[must_use]
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// Small counts as words (`two`), larger ones as digits.
#[must_use]
pub fn word(n: usize) -> String {
    const WORDS: [&str; 11] = [
        "no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    WORDS
        .get(n)
        .map_or_else(|| n.to_string(), |w| (*w).to_owned())
}

/// How long ago `iso` was at `now` (`17 days`, `15 months`, `3 years`).
#[must_use]
pub fn ago(iso: &str, now: &str) -> Option<String> {
    let d = days(now)? - days(iso)?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let round = |x: f64| x.round().max(0.0) as usize;
    Some(if d < 1.0 {
        "today".to_owned()
    } else if d < 45.0 {
        plural(round(d), "day", "days")
    } else if d < 365.0 * 2.0 {
        plural(round(d / 30.44), "month", "months")
    } else {
        plural(round(d / 365.25), "year", "years")
    })
}

/// The semver reading of a history against your pin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// Releases after the pin (`None` when not in your tree).
    pub behind: Option<usize>,
    /// Distinct breaking steps among them.
    pub breaking: usize,
    /// The newest release (not yanked, not a pre-release).
    pub latest: Option<String>,
    /// The line itself.
    pub words: String,
}

/// Reads `history` (any order) against `pin` at `now`.
#[must_use]
pub fn reading(history: &[ReleaseFact], pin: Option<&str>, now: &str) -> Reading {
    let mut released: Vec<&ReleaseFact> = history
        .iter()
        .filter(|r| !r.yanked && parse(&r.v).pre.is_empty())
        .collect();
    released.sort_by(|a, b| cmp(&a.v, &b.v));
    let latest = released.last().map(|r| r.v.clone());
    let Some(pin) = pin else {
        let newest = released
            .last()
            .and_then(|r| r.at.as_deref())
            .and_then(|at| ago(at, now))
            .map_or_else(String::new, |age| format!(" · the newest {age} ago"));
        let words = if history.is_empty() {
            "No releases".to_owned()
        } else {
            format!(
                "Not in your tree · {}{newest}",
                plural(history.len(), "release", "releases")
            )
        };
        return Reading {
            behind: None,
            breaking: 0,
            latest,
            words,
        };
    };
    let after: Vec<&&ReleaseFact> = released
        .iter()
        .filter(|r| cmp(&r.v, pin) == Ordering::Greater)
        .collect();
    let mut classes: Vec<String> = after.iter().map(|r| caret(&r.v)).collect();
    classes.sort();
    classes.dedup();
    classes.retain(|c| *c != caret(pin));
    let breaking = classes.len();
    if after.is_empty() {
        return Reading {
            behind: Some(0),
            breaking: 0,
            latest,
            words: "Up to date · the newest release".to_owned(),
        };
    }
    let first_after = after
        .iter()
        .filter_map(|r| {
            r.at.as_deref()
                .and_then(days)
                .map(|d| (d, r.at.as_deref().unwrap_or_default()))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .and_then(|(_, at)| ago(at, now));
    let broke = if breaking > 0 {
        format!("{} of them breaking", word(breaking))
    } else {
        "none of them breaking".to_owned()
    };
    let age = first_after.map_or_else(String::new, |age| format!(" · {age}"));
    Reading {
        behind: Some(after.len()),
        breaking,
        latest,
        words: format!(
            "{} behind · {broke}{age}",
            plural(after.len(), "release", "releases")
        ),
    }
}

/// `a, b and c` (or `a, b or c`).
#[must_use]
pub fn list(items: &[impl AsRef<str>], conj: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [init @ .., last] => format!(
            "{} {conj} {}",
            init.iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", "),
            last.as_ref()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{ReleaseFact, Tick, ago, caret, cmp, kinds, reading, thousands};
    use std::cmp::Ordering;

    #[test]
    fn caret_classes_and_order() {
        assert_eq!(caret("0.8.23"), "0.8");
        assert_eq!(caret("1.1.5+spec-1.1.0"), "1");
        assert_eq!(caret("0.0.3"), "0.0.3");
        assert_eq!(cmp("1.0.0-alpha.1", "1.0.0"), Ordering::Less);
        assert_eq!(cmp("0.10.0", "0.9.9"), Ordering::Greater);
        assert_eq!(
            kinds(&["0.8.0", "0.8.1", "0.9.0", "1.0.0-rc.1", "1.0.0", "1.1.0"]),
            vec![
                Tick::Breaking,
                Tick::Patch,
                Tick::Breaking,
                Tick::Pre,
                Tick::Breaking,
                Tick::Minor
            ]
        );
        assert_eq!(thousands(1189), "1,189");
    }

    #[test]
    fn ages_read_like_people_say_them() {
        assert_eq!(
            ago("2026-09-10T00:00:00Z", "2026-09-27").as_deref(),
            Some("17 days")
        );
        assert_eq!(
            ago("2025-06-20T00:00:00Z", "2026-09-27").as_deref(),
            Some("15 months")
        );
    }

    #[test]
    fn the_reading_counts_breaking_steps_not_releases() {
        let h = [
            ReleaseFact::new("0.8.23", Some("2025-05-01"), false),
            ReleaseFact::new("0.9.0", Some("2025-07-10"), false),
            ReleaseFact::new("0.9.1", Some("2025-07-11"), false),
            ReleaseFact::new("1.0.0", Some("2026-01-01"), false),
            ReleaseFact::new("1.0.1", Some("2026-02-01"), true),
        ];
        let r = reading(&h, Some("0.8.23"), "2026-09-27");
        assert_eq!(
            r.words,
            "3 releases behind · two of them breaking · 15 months"
        );
        let none = reading(&h, None, "2026-09-27");
        assert_eq!(
            none.words,
            "Not in your tree · 5 releases · the newest 9 months ago"
        );
    }
}
