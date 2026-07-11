//! Shared version-selection machinery for all producer ecosystems.
//!
//! Each ecosystem has genuinely different version grammars (semver §11 vs
//! Maven qualifier ranks vs PEP 440), so the grammars themselves live in each
//! producer crate. What they all share is the **stable-before-prerelease
//! selection loop**: prefer the greatest stable match from a tag set, and only
//! fall back to the greatest prerelease when no stable candidate exists.
//!
//! # Usage pattern
//!
//! 1. Implement [`VersionGrammar`] for your ecosystem's version type.
//! 2. Build a [`VersionRequest`] (or let the caller pass one in).
//! 3. Call [`resolve_from_tags`] — it owns the selection loop and nothing else.
//!
//! The ecosystem-specific parsing (Go subdirectory prefixes, Java artifact-id
//! tag shapes, Python `v`-stripping) belongs in each crate's `VersionGrammar`
//! implementation, not here.

/// Context provided to `VersionGrammar::parse_tag` alongside the raw tag string.
///
/// Holds the ecosystem-neutral metadata that tag parsers often need —
/// the module/artifact identifier and an optional relative subdirectory.
/// Fields that don't apply to an ecosystem are simply ignored by that grammar.
#[derive(Debug, Clone, Default)]
pub struct TagContext<'a> {
    /// Primary module/package/artifact identifier (e.g. Go module path,
    /// Maven artifactId, Python package name).  Empty string when not applicable.
    pub identifier: &'a str,

    /// Repository-relative subdirectory for the module (Go only: `"tools"` for
    /// a module living under `tools/` in the repo).  Empty string at the root.
    pub subdir: &'a str,
}

/// A version request against an ecosystem's tag set.
///
/// The shape is drawn from Go's `VersionRequest` — the cleanest of the four —
/// with the version type made generic so each ecosystem supplies its own.
///
/// * `V` must be `Ord` so the selection loop can pick the greatest match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRequest<V> {
    /// The newest version in the tag set (stable preferred).
    Latest,
    /// An exact version pin: match only this precise version.
    Exact(V),
    /// A prefix range: the newest version whose leading numeric segments match.
    Prefix(Vec<u64>),
}

/// An ecosystem's version grammar — how to turn a raw git tag string into a
/// typed version, and how to classify a version as a prerelease.
///
/// # Implementation contract
///
/// * `parse_tag` must return `None` for any tag that does not belong to the
///   module identified by `ctx` (wrong prefix, wrong major, unparseable, etc.).
///   The returned `(V, String)` pair is `(parsed_version, original_tag_string)`.
/// * `is_prerelease` must be consistent with `V: Ord` — a stable version must
///   compare greater than all prereleases of the same release series.
/// * `matches_request` has a default implementation that works for grammars
///   where `Prefix` means "all leading numeric segments equal after zero-padding".
///   Override it if your grammar needs different prefix semantics.
pub trait VersionGrammar {
    /// The parsed version type for this ecosystem.
    type V: Ord + Clone;

    /// Try to parse `raw_tag` as a version belonging to the module identified
    /// by `ctx`. Return `None` to skip the tag entirely.
    fn parse_tag<'a>(&self, raw_tag: &'a str, ctx: &TagContext<'_>) -> Option<(Self::V, &'a str)>;

    /// Whether `v` is a prerelease (alpha/beta/rc/snapshot/dev/etc.).
    fn is_prerelease(&self, v: &Self::V) -> bool;

    /// Whether `v` satisfies `request` given the filtering context `ctx`.
    ///
    /// The default implementation handles `Latest` (always), `Exact` (equality),
    /// and `Prefix` (leading numeric segments match with zero-padding).  Grammars
    /// that need Go-style major-discipline filtering should override this.
    fn matches_request(&self, v: &Self::V, request: &VersionRequest<Self::V>, _ctx: &TagContext<'_>) -> bool {
        match request {
            VersionRequest::Latest => true,
            VersionRequest::Exact(want) => v == want,
            VersionRequest::Prefix(prefix) => self.prefix_matches(v, prefix),
        }
    }

    /// Extract the leading numeric segments of `v` for prefix matching.
    ///
    /// The default returns an empty slice (no prefix match).  Grammars that
    /// support prefix requests must override this.
    fn numeric_prefix(&self, _v: &Self::V) -> Vec<u64> {
        vec![]
    }

    /// Default prefix matching: every element of `prefix` must equal the
    /// corresponding element of `numeric_prefix(v)` (zero-padded on the right).
    fn prefix_matches(&self, v: &Self::V, prefix: &[u64]) -> bool {
        let nums = self.numeric_prefix(v);
        prefix.iter().enumerate().all(|(i, p)| nums.get(i).copied().unwrap_or(0) == *p)
    }
}

/// The single stable-before-prerelease selection loop.
///
/// Given a slice of raw git tag strings, a version request, a tag context, and
/// a grammar implementation, returns the original tag string of the winning
/// version — or `None` when no tag satisfies the request.
///
/// # Selection algorithm
///
/// 1. Parse every tag through `grammar.parse_tag`; skip tags that return `None`.
/// 2. Filter to tags that satisfy `request` via `grammar.matches_request`.
/// 3. Among matching candidates, prefer the greatest **stable** version; if
///    none exist, fall back to the greatest **prerelease**.
///
/// The return value is the original tag string (not the parsed version), so
/// callers can check out or reference that exact tag.
pub fn resolve_from_tags<G>(
    tags: &[impl AsRef<str>],
    request: &VersionRequest<G::V>,
    ctx: &TagContext<'_>,
    grammar: &G,
) -> Option<String>
where
    G: VersionGrammar,
{
    // Collect (parsed_version, original_tag_str) for every tag that both
    // parses and satisfies the request.
    let mut candidates: Vec<(G::V, String)> = tags
        .iter()
        .filter_map(|t| {
            let raw = t.as_ref();
            let (v, tag_str) = grammar.parse_tag(raw, ctx)?;
            if grammar.matches_request(&v, request, ctx) {
                Some((v, tag_str.to_owned()))
            } else {
                None
            }
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    // Stable-before-prerelease: prefer the greatest stable; fall back to the
    // greatest prerelease only when no stable candidate exists.
    let pick = candidates
        .iter()
        .filter(|(v, _)| !grammar.is_prerelease(v))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .or_else(|| candidates.iter().max_by(|(a, _), (b, _)| a.cmp(b)));

    pick.map(|(_, tag)| tag.clone())
}

// ---------------------------------------------------------------------------
// Composable primitive used by callers that already have typed candidates
// ---------------------------------------------------------------------------

/// Pick the best version from a pre-filtered candidate list.
///
/// This is the stable-before-prerelease core extracted as a free function for
/// callers (e.g. `registry::resolve::select`) that already hold typed
/// `PackageVersion` objects rather than raw tag strings. `candidates` is a
/// slice of `(version, payload)` pairs; `is_prerelease` classifies each
/// version. Returns a reference to the winning pair's payload, or `None` when
/// the slice is empty.
///
/// Semantics are identical to the inner loop in [`resolve_from_tags`]:
/// prefer the greatest stable version, fall back to the greatest prerelease
/// only when no stable candidate exists.
pub fn pick_best<V, T>(
    candidates: &[(V, T)],
    is_prerelease: impl Fn(&V) -> bool,
) -> Option<&T>
where
    V: Ord,
{
    if candidates.is_empty() {
        return None;
    }
    candidates
        .iter()
        .filter(|(v, _)| !is_prerelease(v))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .or_else(|| candidates.iter().max_by(|(a, _), (b, _)| a.cmp(b)))
        .map(|(_, t)| t)
}

// ---------------------------------------------------------------------------
// Unit tests for the shared selection loop
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Minimal grammar for testing (bare u32 versions) ----

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct SimpleVer {
        major: u32,
        minor: u32,
        patch: u32,
        pre: Option<String>, // Some("rc1") = prerelease
    }

    struct SimpleGrammar;

    impl VersionGrammar for SimpleGrammar {
        type V = SimpleVer;

        fn parse_tag<'a>(&self, raw: &'a str, _ctx: &TagContext<'_>) -> Option<(SimpleVer, &'a str)> {
            let rest = raw.strip_prefix('v').unwrap_or(raw);
            let (core, pre) = match rest.split_once('-') {
                Some((c, p)) => (c, Some(p.to_string())),
                None => (rest, None),
            };
            let mut parts = core.split('.');
            let major: u32 = parts.next()?.parse().ok()?;
            let minor: u32 = parts.next()?.parse().ok()?;
            let patch: u32 = parts.next()?.parse().ok()?;
            if parts.next().is_some() { return None; }
            Some((SimpleVer { major, minor, patch, pre }, raw))
        }

        fn is_prerelease(&self, v: &SimpleVer) -> bool {
            v.pre.is_some()
        }

        fn numeric_prefix(&self, v: &SimpleVer) -> Vec<u64> {
            vec![v.major as u64, v.minor as u64, v.patch as u64]
        }
    }

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn latest_prefers_stable_over_prerelease() {
        let t = tags(&["v1.0.0-rc1", "v1.0.0", "v0.9.0"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Latest, &ctx, &SimpleGrammar);
        assert_eq!(result, Some("v1.0.0".to_string()));
    }

    #[test]
    fn prerelease_fallback_when_no_stable() {
        let t = tags(&["v0.1.0-alpha", "v0.1.0-beta"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Latest, &ctx, &SimpleGrammar);
        // beta > alpha lexicographically as string (and our SimpleVer Ord on pre)
        // Actually SimpleVer derives Ord, pre: Some("beta") > Some("alpha") alphabetically.
        assert!(result.is_some());
        // either alpha or beta but must be one of them
        let r = result.unwrap();
        assert!(r == "v0.1.0-alpha" || r == "v0.1.0-beta");
    }

    #[test]
    fn exact_pin_matches_only_exact_version() {
        let t = tags(&["v1.0.0", "v1.0.1", "v2.0.0"]);
        let ctx = TagContext::default();
        let g = SimpleGrammar;
        let (want, _) = g.parse_tag("v1.0.1", &ctx).unwrap();
        let result = resolve_from_tags(&t, &VersionRequest::Exact(want), &ctx, &g);
        assert_eq!(result, Some("v1.0.1".to_string()));
    }

    #[test]
    fn exact_pin_no_match_returns_none() {
        let t = tags(&["v1.0.0", "v2.0.0"]);
        let ctx = TagContext::default();
        let g = SimpleGrammar;
        let (want, _) = g.parse_tag("v1.5.0", &ctx).unwrap();
        let result = resolve_from_tags(&t, &VersionRequest::Exact(want), &ctx, &g);
        assert_eq!(result, None);
    }

    #[test]
    fn prefix_picks_newest_in_range() {
        let t = tags(&["v1.4.2", "v1.4.9", "v1.4.1", "v2.0.0"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Prefix(vec![1, 4]), &ctx, &SimpleGrammar);
        assert_eq!(result, Some("v1.4.9".to_string()));
    }

    #[test]
    fn prefix_stable_beats_prerelease_in_same_prefix() {
        let t = tags(&["v1.4.9-rc1", "v1.4.8"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Prefix(vec![1, 4]), &ctx, &SimpleGrammar);
        // v1.4.8 is stable and should win over v1.4.9-rc1
        assert_eq!(result, Some("v1.4.8".to_string()));
    }

    #[test]
    fn empty_tag_list_returns_none() {
        let t: Vec<String> = vec![];
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Latest, &ctx, &SimpleGrammar);
        assert_eq!(result, None);
    }

    #[test]
    fn no_parseable_tags_returns_none() {
        let t = tags(&["not-a-version", "also-bad"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(&t, &VersionRequest::Latest, &ctx, &SimpleGrammar);
        assert_eq!(result, None);
    }
}
