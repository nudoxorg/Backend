//! NuGet version grammar: SemVer2 with interval-notation ranges.

use core::cmp::Ordering;

use super::{AnyVersion, VersionGrammar};

/// NuGet versioning: SemVer2 with an optional legacy 4th numeric part, plus
/// interval-notation version ranges (`[1.0,2.0)`). NuGet versions are *not*
/// SemVer (`1.0.0.5` is legal), so `semver` cannot be used — this is a faithful
/// subset of NuGet's own `NuGetVersion` / `VersionRange` semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NuGetVersion {
    parts: [u64; 4],
    /// Lowercased prerelease labels (`-alpha.1` → `["alpha", "1"]`); empty
    /// for a release version.
    pre: Vec<String>,
}

impl NuGetVersion {
    /// Parse a NuGet version string, or `None` if it isn't numeric-led.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        // Drop build metadata (`+sha`) — ignored in ordering.
        let core = text.split('+').next().unwrap_or(text);
        let (numeric, pre_str) = match core.split_once('-') {
            Some((n, p)) => (n, Some(p)),
            None => (core, None),
        };
        let mut parts = [0u64; 4];
        let mut count = 0usize;
        for (i, seg) in numeric.split('.').enumerate() {
            if i >= 4 || seg.is_empty() {
                return None;
            }
            parts[i] = seg.parse::<u64>().ok()?;
            count = i + 1;
        }
        if count == 0 {
            return None;
        }
        let pre = pre_str
            .map(|p| p.split('.').map(str::to_ascii_lowercase).collect())
            .unwrap_or_default();
        Some(NuGetVersion { parts, pre })
    }

    /// Whether this version carries a prerelease label.
    pub fn is_prerelease_inner(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl Ord for NuGetVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.parts.cmp(&other.parts) {
            Ordering::Equal => {}
            ord => return ord,
        }
        // A release outranks any prerelease of the same numeric core.
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            (false, false) => nuget_cmp_pre(&self.pre, &other.pre),
        }
    }
}

impl PartialOrd for NuGetVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Compare prerelease label lists dot-segment-wise (numeric segments
/// numerically, else case-insensitive lexically; numeric < alphanumeric).
fn nuget_cmp_pre(a: &[String], b: &[String]) -> Ordering {
    for i in 0..a.len().max(b.len()) {
        let ord = match (a.get(i), b.get(i)) {
            (Some(x), Some(y)) => nuget_cmp_pre_seg(x, y),
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

fn nuget_cmp_pre_seg(x: &str, y: &str) -> Ordering {
    match (x.parse::<u64>(), y.parse::<u64>()) {
        (Ok(nx), Ok(ny)) => nx.cmp(&ny),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => x.cmp(y),
    }
}

struct NuGetBound {
    version: Option<NuGetVersion>,
    inclusive: bool,
}

struct NuGetRange {
    lower: NuGetBound,
    upper: NuGetBound,
}

/// Parse a NuGet version-range spec. Supports interval notation
/// (`[1.0]`, `[1.0,2.0)`, `(1.0,)`, `(,2.0]`) and a bare version
/// (`1.2.3` = "≥ 1.2.3, the *minimum*", NOT exact — the documented rule).
fn nuget_parse_range(spec: &str) -> Option<NuGetRange> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let first = spec.chars().next().unwrap();
    let last = spec.chars().last().unwrap();
    let is_interval = matches!(first, '[' | '(') && matches!(last, ']' | ')');
    if !is_interval {
        // Bare version: minimum inclusive, unbounded above.
        let v = NuGetVersion::parse(spec)?;
        return Some(NuGetRange {
            lower: NuGetBound {
                version: Some(v),
                inclusive: true,
            },
            upper: NuGetBound {
                version: None,
                inclusive: false,
            },
        });
    }
    let inner = &spec[1..spec.len() - 1];
    let (lo_str, hi_str) = if let Some((lo, hi)) = inner.split_once(',') {
        (lo.trim(), hi.trim())
    } else {
        // `[1.0]` — an exact single version.
        let v = NuGetVersion::parse(inner.trim())?;
        return Some(NuGetRange {
            lower: NuGetBound {
                version: Some(v.clone()),
                inclusive: true,
            },
            upper: NuGetBound {
                version: Some(v),
                inclusive: true,
            },
        });
    };
    let lower = NuGetBound {
        version: if lo_str.is_empty() {
            None
        } else {
            Some(NuGetVersion::parse(lo_str)?)
        },
        inclusive: first == '[',
    };
    let upper = NuGetBound {
        version: if hi_str.is_empty() {
            None
        } else {
            Some(NuGetVersion::parse(hi_str)?)
        },
        inclusive: last == ']',
    };
    Some(NuGetRange { lower, upper })
}

impl VersionGrammar for NuGetVersion {
    fn parse(raw: &str) -> Option<Self> {
        NuGetVersion::parse(raw)
    }

    fn is_prerelease(&self) -> bool {
        self.is_prerelease_inner()
    }

    fn range_matches(spec: &str, candidate: &Self) -> bool {
        let Some(range) = nuget_parse_range(spec) else {
            return false;
        };
        if let Some(lo) = &range.lower.version {
            match candidate.cmp(lo) {
                Ordering::Less => return false,
                Ordering::Equal if !range.lower.inclusive => return false,
                _ => {}
            }
        }
        if let Some(hi) = &range.upper.version {
            match candidate.cmp(hi) {
                Ordering::Greater => return false,
                Ordering::Equal if !range.upper.inclusive => return false,
                _ => {}
            }
        }
        true
    }

    fn spec_is_valid(spec: &str) -> bool {
        nuget_parse_range(spec).is_some()
    }

    fn erase(self) -> AnyVersion {
        AnyVersion::NuGet(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nuget(s: &str) -> NuGetVersion {
        NuGetVersion::parse(s).expect("parses")
    }

    #[test]
    fn nuget_four_part_and_prerelease_ordering() {
        assert!(nuget("1.0.0.5") > nuget("1.0.0"));
        assert!(nuget("1.0.0") > nuget("1.0.0-rc.1"));
        assert!(nuget("1.0.0-alpha") < nuget("1.0.0-beta"));
        assert!(nuget("1.0.0-alpha.1") < nuget("1.0.0-alpha.2"));
        // Case-insensitive prerelease.
        assert_eq!(nuget("1.0.0-Alpha"), nuget("1.0.0-alpha"));
        // Build metadata dropped.
        assert_eq!(nuget("1.0.0+abc"), nuget("1.0.0+def"));
    }

    #[test]
    fn nuget_interval_notation() {
        assert!(NuGetVersion::range_matches("[1.0,2.0)", &nuget("1.5.0")));
        assert!(!NuGetVersion::range_matches("[1.0,2.0)", &nuget("2.0.0")));
        assert!(NuGetVersion::range_matches("[1.0,2.0]", &nuget("2.0.0")));
        assert!(!NuGetVersion::range_matches("(1.0,2.0)", &nuget("1.0.0")));
        assert!(NuGetVersion::range_matches("(1.0,)", &nuget("5.0.0")));
        assert!(NuGetVersion::range_matches("(,2.0]", &nuget("1.0.0")));
        assert!(NuGetVersion::range_matches("[1.0]", &nuget("1.0.0")));
        assert!(!NuGetVersion::range_matches("[1.0]", &nuget("1.0.1")));
    }

    #[test]
    fn nuget_bare_version_is_minimum_not_exact() {
        assert!(NuGetVersion::range_matches("1.2.3", &nuget("1.2.3")));
        assert!(NuGetVersion::range_matches("1.2.3", &nuget("2.0.0")));
        assert!(!NuGetVersion::range_matches("1.2.3", &nuget("1.0.0")));
    }

    #[test]
    fn nuget_leading_zeros_compare_numerically() {
        assert_eq!(nuget("01.002.3"), nuget("1.2.3"));
    }

    #[test]
    fn nuget_malformed_specs_rejected() {
        assert!(!NuGetVersion::spec_is_valid("not-a-version"));
        assert!(!NuGetVersion::spec_is_valid(""));
        assert!(NuGetVersion::spec_is_valid("[1.0,2.0)"));
        assert!(NuGetVersion::spec_is_valid("1.2.3"));
    }
}
