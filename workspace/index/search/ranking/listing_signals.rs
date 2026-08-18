//! Pure extractors for listing-derived search signals (release counts, freshness).
//!
//! Used at package emit time so temporal quality and release stats do not wait
//! on a full resolve rewrite. Parsers never panic; malformed bodies → `None`.

use chrono::{DateTime, Utc};
use heart::ecosystem::Language;

/// Signals derived from a registry version-listing response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingSignals {
    /// Total published versions observed in the listing body.
    pub total: u32,
    /// Withdrawn/yanked/unlisted among them.
    pub withdrawn: u32,
    /// Whether the version currently being indexed is withdrawn.
    pub this_version_withdrawn: bool,
    /// Days since the newest `created_at` / `updated_at` / `time` stamp in the
    /// listing, when the body carries timestamps.
    pub last_release_days_ago: Option<u32>,
}

/// Parse listing signals from a raw registry body for `ecosystem`.
///
/// `version_raw` is the version string being indexed (for withdrawn status of
/// *this* version). `now` is injected for pure tests.
pub fn listing_signals_from_body(
    ecosystem: Language,
    body: &[u8],
    version_raw: &str,
    now: DateTime<Utc>,
) -> Option<ListingSignals> {
    match ecosystem {
        Language::Rust => parse_crates_io(body, version_raw, now),
        Language::Typescript => parse_npm(body, version_raw, now),
        Language::Python => parse_pypi(body, version_raw, now),
        // Go (list of tags), Java, C#: counts only via DynSpec-style arrays
        // when JSON is present; otherwise no temporal.
        Language::CSharp => parse_nuget_versions_array(body, version_raw, now),
        // Go (list of tags), Java, C#, and cpp (ls-remote-derived counts):
        // count-only when a JSON array is present, else no temporal signal.
        Language::Go | Language::Java | Language::Cpp => {
            parse_generic_version_count(body, version_raw)
        }
    }
}

fn days_ago(now: DateTime<Utc>, ts: DateTime<Utc>) -> u32 {
    let secs = (now - ts).num_seconds().max(0) as u64;
    (secs / 86_400) as u32
}

fn parse_rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// crates.io `/api/v1/crates/{name}` — versions[].num, yanked, created_at.
fn parse_crates_io(body: &[u8], version_raw: &str, now: DateTime<Utc>) -> Option<ListingSignals> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let versions = v.get("versions")?.as_array()?;
    if versions.is_empty() {
        return None;
    }
    let mut total = 0u32;
    let mut withdrawn = 0u32;
    let mut this_withdrawn = false;
    let mut newest: Option<DateTime<Utc>> = None;
    for entry in versions {
        let Some(num) = entry.get("num").and_then(|x| x.as_str()) else {
            continue;
        };
        total = total.saturating_add(1);
        let yanked = entry
            .get("yanked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if yanked {
            withdrawn = withdrawn.saturating_add(1);
        }
        if num == version_raw {
            this_withdrawn = yanked;
        }
        if let Some(ts) = entry
            .get("created_at")
            .and_then(|x| x.as_str())
            .and_then(parse_rfc3339)
        {
            newest = Some(newest.map_or(ts, |n| n.max(ts)));
        }
    }
    if total == 0 {
        return None;
    }
    Some(ListingSignals {
        total,
        withdrawn,
        this_version_withdrawn: this_withdrawn,
        last_release_days_ago: newest.map(|t| days_ago(now, t)),
    })
}

/// npm packument — `versions` object keys + `time` map.
fn parse_npm(body: &[u8], version_raw: &str, now: DateTime<Utc>) -> Option<ListingSignals> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let versions = v.get("versions")?.as_object()?;
    if versions.is_empty() {
        return None;
    }
    let total = versions.len() as u32;
    // npm deprecations are per-version strings; treat non-empty deprecated as withdrawn.
    let mut withdrawn = 0u32;
    let mut this_withdrawn = false;
    for (ver, meta) in versions {
        let dep = meta
            .get("deprecated")
            .and_then(|d| d.as_str())
            .is_some_and(|s| !s.is_empty());
        if dep {
            withdrawn = withdrawn.saturating_add(1);
        }
        if ver == version_raw {
            this_withdrawn = dep;
        }
    }
    let mut newest: Option<DateTime<Utc>> = None;
    if let Some(time) = v.get("time").and_then(|t| t.as_object()) {
        for (k, ts) in time {
            if k == "created" || k == "modified" {
                continue;
            }
            if let Some(dt) = ts.as_str().and_then(parse_rfc3339) {
                newest = Some(newest.map_or(dt, |n| n.max(dt)));
            }
        }
        // Fall back to modified.
        if newest.is_none() {
            newest = time
                .get("modified")
                .and_then(|x| x.as_str())
                .and_then(parse_rfc3339);
        }
    }
    Some(ListingSignals {
        total,
        withdrawn,
        this_version_withdrawn: this_withdrawn,
        last_release_days_ago: newest.map(|t| days_ago(now, t)),
    })
}

/// PyPI JSON API — `releases` object; upload_time_iso_8601 on files.
fn parse_pypi(body: &[u8], version_raw: &str, now: DateTime<Utc>) -> Option<ListingSignals> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let releases = v.get("releases")?.as_object()?;
    if releases.is_empty() {
        return None;
    }
    let total = releases.len() as u32;
    let this_withdrawn = releases
        .get(version_raw)
        .and_then(|files| files.as_array())
        .is_some_and(std::vec::Vec::is_empty);
    // Yanked releases on PyPI are often empty arrays.
    let withdrawn = releases
        .values()
        .filter(|files| files.as_array().is_some_and(std::vec::Vec::is_empty))
        .count() as u32;
    let mut newest: Option<DateTime<Utc>> = None;
    for files in releases.values() {
        let Some(arr) = files.as_array() else {
            continue;
        };
        for f in arr {
            if let Some(dt) = f
                .get("upload_time_iso_8601")
                .or_else(|| f.get("upload_time"))
                .and_then(|x| x.as_str())
                .and_then(|s| {
                    parse_rfc3339(s).or_else(|| {
                        // PyPI sometimes uses "2019-01-01T00:00:00"
                        DateTime::parse_from_str(&format!("{s}+00:00"), "%Y-%m-%dT%H:%M:%S%z")
                            .ok()
                            .map(|d| d.with_timezone(&Utc))
                    })
                })
            {
                newest = Some(newest.map_or(dt, |n| n.max(dt)));
            }
        }
    }
    Some(ListingSignals {
        total,
        withdrawn,
        this_version_withdrawn: this_withdrawn,
        last_release_days_ago: newest.map(|t| days_ago(now, t)),
    })
}

/// NuGet-style `{"versions":["1.0.0",...]}` — counts only, no timestamps.
fn parse_nuget_versions_array(
    body: &[u8],
    version_raw: &str,
    _now: DateTime<Utc>,
) -> Option<ListingSignals> {
    let v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let versions = v.get("versions")?.as_array()?;
    if versions.is_empty() {
        return None;
    }
    let total = versions.len() as u32;
    let this_withdrawn = !versions.iter().any(|x| x.as_str() == Some(version_raw));
    Some(ListingSignals {
        total,
        withdrawn: 0,
        this_version_withdrawn: this_withdrawn,
        last_release_days_ago: None,
    })
}

/// Best-effort: newline list or JSON array of version strings — counts only.
fn parse_generic_version_count(body: &[u8], _version_raw: &str) -> Option<ListingSignals> {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body)
        && let Some(arr) = v.as_array()
    {
        let total = arr.len() as u32;
        if total == 0 {
            return None;
        }
        return Some(ListingSignals {
            total,
            withdrawn: 0,
            this_version_withdrawn: false,
            last_release_days_ago: None,
        });
    }
    let text = std::str::from_utf8(body).ok()?;
    let total = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .count() as u32;
    if total == 0 {
        return None;
    }
    Some(ListingSignals {
        total,
        withdrawn: 0,
        this_version_withdrawn: false,
        last_release_days_ago: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn crates_io_counts_yanked_and_freshness() {
        let body = br#"{
			"versions": [
				{"num": "1.0.0", "yanked": false, "created_at": "2024-05-01T00:00:00Z"},
				{"num": "0.9.0", "yanked": true, "created_at": "2023-01-01T00:00:00Z"}
			]
		}"#;
        let s = listing_signals_from_body(Language::Rust, body, "0.9.0", fixed_now()).unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.withdrawn, 1);
        assert!(s.this_version_withdrawn);
        assert_eq!(s.last_release_days_ago, Some(31)); // May 1 → June 1
    }

    #[test]
    fn crates_io_malformed_is_none() {
        assert!(
            listing_signals_from_body(Language::Rust, b"not json", "1.0.0", fixed_now()).is_none()
        );
    }

    #[test]
    fn npm_packument_time_map() {
        let body = br#"{
			"versions": {
				"1.0.0": {},
				"2.0.0": {"deprecated": "old"}
			},
			"time": {
				"created": "2020-01-01T00:00:00.000Z",
				"modified": "2024-05-20T00:00:00.000Z",
				"1.0.0": "2020-01-01T00:00:00.000Z",
				"2.0.0": "2024-05-20T00:00:00.000Z"
			}
		}"#;
        let s =
            listing_signals_from_body(Language::Typescript, body, "2.0.0", fixed_now()).unwrap();
        assert_eq!(s.total, 2);
        assert_eq!(s.withdrawn, 1);
        assert!(s.this_version_withdrawn);
        assert_eq!(s.last_release_days_ago, Some(12));
    }

    #[test]
    fn go_newline_list_counts_only() {
        let body = b"v1.0.0\nv1.1.0\n\nv2.0.0\n";
        let s = listing_signals_from_body(Language::Go, body, "v1.0.0", fixed_now()).unwrap();
        assert_eq!(s.total, 3);
        assert!(s.last_release_days_ago.is_none());
    }
}
