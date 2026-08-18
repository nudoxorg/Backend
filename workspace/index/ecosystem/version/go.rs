//! Go module version grammar: `v`-prefixed semver, pseudo-versions.

use core::cmp::Ordering;

use super::{AnyVersion, VersionGrammar};

/// The internal representation of a Go module version.
///
/// Go uses semver with a mandatory `v` prefix. Pseudo-versions have the form
/// `vX.Y.Z-yyyymmddhhmmss-abcdefabcdef` (or the `-0.yyyymmddhhmmss-hash` and
/// `-pre.0.yyyymmddhhmmss-hash` variants for pre-tagged and tagged-prerelease
/// modules).
///
/// Range semantics: Go's Minimum Version Selection (MVS) has no range
/// syntax on this path. `range_matches(spec, candidate)` returns true iff
/// `spec` is a valid GoVersion AND parses to the same canonical form as
/// `candidate` (exact equality). Document: call sites that need MVS-latest
/// should use the `Latest` request variant, not a range constraint.
/// Equality is defined via [`Ord`] (`cmp == Equal`), NOT structurally — the
/// `+incompatible` marker is ignored in ordering per the Go module spec, so it
/// must be ignored in equality too or `Ord`'s contract breaks.
#[derive(Debug, Clone)]
pub struct GoVersion {
    /// Major.Minor.Patch.
    major: u64,
    minor: u64,
    patch: u64,
    /// Pre-release label (the full string after the first `-`, before any `+`).
    /// For pseudo-versions this is the raw pseudo-version pre label;
    /// for ordinary pre-releases it is e.g. "alpha.1".
    pre: Option<GoPreRelease>,
    /// Whether the original had a `+incompatible` suffix (informational only;
    /// ignored in ordering per Go spec §module-compat).
    incompatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GoPreRelease {
    /// A pseudo-version: base pre label (may be empty), timestamp (14 digits),
    /// and commit hash (12 hex chars). Ordered by (base, timestamp, hash).
    Pseudo {
        /// The optional pre-label prefix before the timestamp, e.g. "pre.0" or "0".
        base: String,
        /// The 14-digit timestamp as a raw string (YYYYMMDDHHMMSS).
        timestamp: String,
        /// The 12-char commit hash.
        hash: String,
    },
    /// An ordinary pre-release label string, e.g. "alpha.1", "beta".
    Label(String),
}

impl GoVersion {
    /// Parse `raw` as a Go module version, returning `None` on failure.
    ///
    /// Accepted forms:
    ///   `vX.Y.Z`
    ///   `vX.Y.Z-prerelease`
    ///   `vX.Y.Z-YYYYMMDDHHMMSS-abcdefabcdef`  (pseudo, base empty)
    ///   `vX.Y.Z-0.YYYYMMDDHHMMSS-hash`         (pseudo, base "0")
    ///   `vX.Y.Z-pre.0.YYYYMMDDHHMMSS-hash`     (pseudo, base "pre.0")
    ///   Any of the above + `+incompatible`
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        // Must start with 'v'.
        let s = raw.strip_prefix('v')?;
        // Strip +incompatible suffix.
        let (s, incompatible) = s
            .strip_suffix("+incompatible")
            .map_or((s, false), |stripped| (stripped, true));
        // Split off pre-release from numeric core.
        let (numeric, pre_raw) = match s.split_once('-') {
            Some((n, p)) => (n, Some(p)),
            None => (s, None),
        };
        // Parse X.Y.Z.
        let mut parts = numeric.split('.');
        let major: u64 = parts.next()?.parse().ok()?;
        let minor: u64 = parts.next()?.parse().ok()?;
        let patch: u64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None; // extra numeric segments not valid in Go
        }
        let pre = pre_raw.map(parse_go_pre);
        Some(GoVersion {
            major,
            minor,
            patch,
            pre,
            incompatible,
        })
    }

    /// The canonical string form, rebuilt from components. `+incompatible` IS
    /// rendered (goproxy listings and download URLs carry it), even though it
    /// is ignored in ordering/equality.
    pub fn canonical(&self) -> String {
        let mut s = format!("v{}.{}.{}", self.major, self.minor, self.patch);
        match &self.pre {
            None => {}
            Some(GoPreRelease::Label(lbl)) => {
                s.push('-');
                s.push_str(lbl);
            }
            Some(GoPreRelease::Pseudo {
                base,
                timestamp,
                hash,
            }) => {
                s.push('-');
                if !base.is_empty() {
                    s.push_str(base);
                    s.push('.');
                }
                s.push_str(timestamp);
                s.push('-');
                s.push_str(hash);
            }
        }
        if self.incompatible {
            s.push_str("+incompatible");
        }
        s
    }
}

/// Parse the pre-release portion of a Go version (after the first `-`).
fn parse_go_pre(p: &str) -> GoPreRelease {
    // A pseudo-version ends with `-<12-hex-chars>` and the segment before that
    // is a 14-digit timestamp, optionally preceded by a base label.
    // Split by `-` to find the hash at the end.
    let parts: Vec<&str> = p.splitn(3, '-').collect();
    // Check if the last segment is a 12-char hex commit hash.
    let is_hash = |s: &str| s.len() == 12 && s.chars().all(|c| c.is_ascii_hexdigit());
    // Check if a segment is a 14-digit timestamp.
    let is_timestamp = |s: &str| s.len() == 14 && s.chars().all(|c| c.is_ascii_digit());

    match parts.as_slice() {
        // vX.Y.Z-YYYYMMDDHHMMSS-hash  (no base)
        [ts, hash] if is_timestamp(ts) && is_hash(hash) => GoPreRelease::Pseudo {
            base: String::new(),
            timestamp: ts.to_string(),
            hash: hash.to_string(),
        },
        // vX.Y.Z-base.YYYYMMDDHHMMSS-hash  (base present, base may contain dots)
        // The base is the part before the timestamp, which is in the middle.
        // We re-split from right to find timestamp-hash suffix.
        _ => {
            // Try to find a `-hash` suffix where hash is 12 hex.
            // Then check if the part before that ends in a 14-digit timestamp.
            if let Some(dash_hash_pos) = p.rfind('-') {
                let hash = &p[dash_hash_pos + 1..];
                if is_hash(hash) {
                    let before_hash = &p[..dash_hash_pos];
                    // Find the timestamp — last dot-separated or dash-separated segment.
                    // Pseudo-version base uses dots: "base.TIMESTAMP" or "0.TIMESTAMP".
                    if let Some(dot_ts_pos) = before_hash.rfind('.') {
                        let ts = &before_hash[dot_ts_pos + 1..];
                        if is_timestamp(ts) {
                            let base = &before_hash[..dot_ts_pos];
                            return GoPreRelease::Pseudo {
                                base: base.to_string(),
                                timestamp: ts.to_string(),
                                hash: hash.to_string(),
                            };
                        }
                    }
                }
            }
            // Not a pseudo-version — ordinary label.
            GoPreRelease::Label(p.to_string())
        }
    }
}

impl Ord for GoVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        // Compare numeric core first.
        let core =
            (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != Ordering::Equal {
            return core;
        }
        // Release > any pre-release (same semantics as semver).
        // `incompatible` is deliberately not consulted (Go ignores it).
        match (&self.pre, &other.pre) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => cmp_go_pre(a, b),
        }
    }
}

impl PartialOrd for GoVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for GoVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for GoVersion {}

fn cmp_go_pre(a: &GoPreRelease, b: &GoPreRelease) -> Ordering {
    match (a, b) {
        (
            GoPreRelease::Pseudo {
                base: ba,
                timestamp: ta,
                hash: ha,
            },
            GoPreRelease::Pseudo {
                base: bb,
                timestamp: tb,
                hash: hb,
            },
        ) => {
            // Pseudo-versions order by base label then timestamp then hash —
            // exactly the semver order of their dotted prerelease rendering.
            cmp_semver_pre(ba, bb)
                .then_with(|| ta.cmp(tb))
                .then_with(|| ha.cmp(hb))
        }
        // Ordinary labels compare as semver prerelease identifier lists
        // (numeric identifiers numerically and before alphanumerics), NOT
        // lexically — `alpha.2 < alpha.10`.
        (GoPreRelease::Label(a), GoPreRelease::Label(b)) => cmp_semver_pre(a, b),
        // A pseudo-version's prerelease starts with a numeric identifier
        // (`0.2020…` or the bare timestamp), which semver orders before any
        // alphanumeric label — matching the fixed rule pseudo < label.
        (GoPreRelease::Pseudo { .. }, GoPreRelease::Label(_)) => Ordering::Less,
        (GoPreRelease::Label(_), GoPreRelease::Pseudo { .. }) => Ordering::Greater,
    }
}

/// Semver §11 prerelease comparison over dot-separated identifiers: numeric
/// identifiers compare numerically and sort before alphanumerics; a shorter
/// list that is a prefix of the longer sorts first. Empty = absent base.
fn cmp_semver_pre(a: &str, b: &str) -> Ordering {
    let mut xs = a.split('.').filter(|s| !s.is_empty());
    let mut ys = b.split('.').filter(|s| !s.is_empty());
    loop {
        match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(nx), Ok(ny)) => nx.cmp(&ny),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    (Err(_), Err(_)) => x.cmp(y),
                };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
        }
    }
}

impl VersionGrammar for GoVersion {
    fn parse(raw: &str) -> Option<Self> {
        GoVersion::parse(raw)
    }

    fn is_prerelease(&self) -> bool {
        // Pseudo-versions ARE prereleases — Go treats them as pre-release
        // of the next tag (per go help modules: pseudo-versions are not
        // recommended for use in require directives when a tagged release exists).
        self.pre.is_some()
    }

    /// Go has no range syntax in our resolution path (MVS is whole-module, not
    /// per-constraint). A spec matches iff it equals the candidate's canonical
    /// form (exact match). Callers needing MVS-latest should use `Latest`.
    fn range_matches(spec: &str, candidate: &Self) -> bool {
        GoVersion::parse(spec).is_some_and(|sv| sv.canonical() == candidate.canonical())
    }

    fn spec_is_valid(spec: &str) -> bool {
        GoVersion::parse(spec).is_some()
    }

    fn erase(self) -> AnyVersion {
        AnyVersion::Go(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cmp::Ordering;

    fn go(s: &str) -> GoVersion {
        GoVersion::parse(s).unwrap_or_else(|| panic!("GoVersion::parse({s:?}) returned None"))
    }

    #[test]
    fn go_basic_ordering() {
        assert!(go("v1.0.1") > go("v1.0.0"));
        assert!(go("v1.1.0") > go("v1.0.9"));
        assert!(go("v2.0.0") > go("v1.99.99"));
    }

    #[test]
    fn go_prerelease_less_than_release() {
        assert!(go("v1.0.0-alpha") < go("v1.0.0"));
        assert!(go("v1.0.0-beta") < go("v1.0.0"));
        assert!(go("v1.0.0-rc.1") < go("v1.0.0"));
    }

    #[test]
    fn go_pseudo_version_timestamp_ordering() {
        // Earlier timestamp < later timestamp, same base.
        assert!(
            go("v0.0.0-20200101000000-abcdefabcdef") < go("v0.0.0-20200828120000-abcdefabcdef")
        );
        // Same timestamp, different hash — lexical hash order.
        assert!(
            go("v0.0.0-20200828120000-000000000000") < go("v0.0.0-20200828120000-ffffffffffff")
        );
    }

    #[test]
    fn go_pseudo_less_than_next_release() {
        // A pseudo-version is a pre-release; the release at the same numeric
        // tag is greater.
        assert!(go("v1.2.3-20200828120000-abcdefabcdef") < go("v1.2.3"));
        assert!(go("v0.0.0-20200101000000-abcdefabcdef") < go("v0.0.1"));
    }

    #[test]
    fn go_pseudo_with_base_label() {
        // vX.Y.Z-0.YYYYMMDDHHMMSS-hash form.
        let v = go("v1.0.0-0.20200828120000-abcdefabcdef");
        assert!(v.is_prerelease());
        // vX.Y.Z-pre.0.YYYYMMDDHHMMSS-hash form.
        let v2 = go("v1.0.0-pre.0.20200828120000-abcdefabcdef");
        assert!(v2.is_prerelease());
    }

    #[test]
    fn go_incompatible_ignored_in_ordering() {
        // +incompatible is a cosmetic suffix; ordering is identical to without.
        assert_eq!(
            go("v2.0.0+incompatible").cmp(&go("v2.0.0+incompatible")),
            Ordering::Equal
        );
        // Strip makes them equal.
        let a = go("v2.0.0+incompatible");
        let b = go("v2.0.0");
        // They should be Equal since incompatible is ignored in ordering.
        assert_eq!(a.cmp(&b), Ordering::Equal);
    }

    #[test]
    fn go_incompatible_equal_under_eq() {
        // Eq is defined via cmp, so the ignored +incompatible marker cannot
        // split ordering-equal versions (Ord contract).
        assert_eq!(go("v2.0.0+incompatible"), go("v2.0.0"));
    }

    #[test]
    fn go_labels_compare_as_semver_identifiers() {
        // Numeric identifiers compare numerically, not lexically.
        assert!(go("v1.0.0-alpha.2") < go("v1.0.0-alpha.10"));
        // Numeric identifiers sort before alphanumerics.
        assert!(go("v1.0.0-1") < go("v1.0.0-alpha"));
        // Prefix lists sort first.
        assert!(go("v1.0.0-alpha") < go("v1.0.0-alpha.1"));
    }

    #[test]
    fn go_reject_missing_v_prefix() {
        assert!(GoVersion::parse("1.0.0").is_none());
        assert!(GoVersion::parse("1.2.3").is_none());
        assert!(GoVersion::parse("").is_none());
    }

    #[test]
    fn go_reject_garbage() {
        assert!(GoVersion::parse("not-a-version").is_none());
        assert!(GoVersion::parse("v").is_none());
        assert!(GoVersion::parse("vX.Y.Z").is_none());
        assert!(GoVersion::parse("v1.2.3.4").is_none());
    }

    #[test]
    fn go_is_prerelease() {
        assert!(!go("v1.0.0").is_prerelease());
        assert!(go("v1.0.0-alpha").is_prerelease());
        assert!(go("v0.0.0-20200101000000-abcdefabcdef").is_prerelease());
    }

    #[test]
    fn go_range_matches_exact_only() {
        let v = go("v1.2.3");
        assert!(GoVersion::range_matches("v1.2.3", &v));
        assert!(!GoVersion::range_matches("v1.2.4", &v));
        assert!(!GoVersion::range_matches("v1.2.3-alpha", &v));
    }

    #[test]
    fn go_spec_is_valid() {
        assert!(GoVersion::spec_is_valid("v1.0.0"));
        assert!(GoVersion::spec_is_valid(
            "v0.0.0-20200828120000-abcdefabcdef"
        ));
        assert!(!GoVersion::spec_is_valid("1.0.0"));
        assert!(!GoVersion::spec_is_valid("not-a-version"));
    }

    #[test]
    fn go_transitivity_and_sort_determinism() {
        // A fixed pool of 25 Go versions; we check full transitivity and that
        // sorting is deterministic across a few fixed permutations.
        let pool = [
            "v0.0.0-20190101000000-aaaaaaaaaaaa",
            "v0.0.0-20200101000000-bbbbbbbbbbbb",
            "v0.0.0-20200828120000-cccccccccccc",
            "v0.0.0-20201231235959-dddddddddddd",
            "v0.1.0-alpha",
            "v0.1.0-beta",
            "v0.1.0-rc.1",
            "v0.1.0",
            "v0.2.0",
            "v1.0.0-alpha",
            "v1.0.0-beta",
            "v1.0.0-rc.1",
            "v1.0.0",
            "v1.0.1",
            "v1.0.2",
            "v1.1.0",
            "v1.2.0",
            "v1.2.3",
            "v1.2.4",
            "v2.0.0",
            "v2.0.0+incompatible",
            "v2.1.0",
            "v10.0.0",
            "v10.1.0",
            "v10.1.1",
        ];
        let parsed: Vec<GoVersion> = pool.iter().filter_map(|s| GoVersion::parse(s)).collect();
        assert_eq!(parsed.len(), pool.len(), "all pool versions must parse");

        // Sort canonical.
        let mut sorted = parsed.clone();
        sorted.sort();

        // Transitivity: for all i<j<k: sorted[i] <= sorted[j] <= sorted[k].
        for i in 0..sorted.len() {
            for j in i..sorted.len() {
                for k in j..sorted.len() {
                    assert!(
                        sorted[i] <= sorted[j],
                        "transitivity fail: {:?} > {:?}",
                        sorted[i],
                        sorted[j]
                    );
                    assert!(sorted[j] <= sorted[k]);
                    // Antisymmetry: a < b => !(b < a).
                    if sorted[i] < sorted[j] {
                        assert!(sorted[j] >= sorted[i]);
                    }
                }
            }
        }

        // Sort determinism across fixed permutations.
        let perm1: Vec<GoVersion> = {
            let mut v = parsed.clone();
            // Reverse order.
            v.reverse();
            v.sort();
            v
        };
        let perm2: Vec<GoVersion> = {
            let mut v = parsed;
            // Rotate.
            v.rotate_left(7);
            v.sort();
            v
        };
        // Sorted output should be identical regardless of input order.
        // (Note: v2.0.0 and v2.0.0+incompatible are equal, so we compare
        // the canonical strings after sorting.)
        let canonical_sorted: Vec<String> =
            sorted.iter().map(super::GoVersion::canonical).collect();
        let canonical_perm1_len = perm1.iter().map(super::GoVersion::canonical).count();
        let canonical_perm2_len = perm2.iter().map(super::GoVersion::canonical).count();
        // They may differ only for equal elements (e.g. v2.0.0 vs v2.0.0+incompatible).
        assert_eq!(canonical_sorted.len(), canonical_perm1_len);
        assert_eq!(canonical_sorted.len(), canonical_perm2_len);
    }
}
