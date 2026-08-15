//! Versioned payloads and shared stable-before-prerelease version selection.

use serde::{Deserialize, Serialize};

use crate::identity::PackageVersion;

/// A payload paired with the package version it corresponds to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioned<T> {
    version: PackageVersion,
    object: T,
}

impl<T> Versioned<T> {
    /// Pair an object with its version.
    pub const fn new(version: PackageVersion, object: T) -> Self {
        Self { version, object }
    }

    /// The version this object corresponds to.
    pub const fn version(&self) -> &PackageVersion {
        &self.version
    }

    /// A shared reference to the payload.
    pub const fn get(&self) -> &T {
        &self.object
    }

    /// Consume into the payload, dropping the version tag.
    pub fn into_inner(self) -> T {
        self.object
    }

    /// Map the payload while preserving the version.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Versioned<U> {
        Versioned {
            version: self.version,
            object: f(self.object),
        }
    }
}

// ===========================================================================
// Shared version-selection machinery for all producer ecosystems.
//
// Each ecosystem has genuinely different version grammars (semver §11 vs
// Maven qualifier ranks vs PEP 440), so the grammars themselves live in each
// producer crate. What they all share is the stable-before-prerelease
// selection loop: prefer the greatest stable match from a tag set, and only
// fall back to the greatest prerelease when no stable candidate exists.
//
// Usage pattern:
//   1. Implement `VersionGrammar` for your ecosystem's version type.
//   2. Build a `VersionRequest` (or let the caller pass one in).
//   3. Call `resolve_from_tags` — it owns the selection loop and nothing else.
//
// (Formerly the standalone `version` crate, folded into `heart::version`.)
// ===========================================================================

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

/// An ecosystem-supplied *constraint predicate* over versions.
///
/// This is the general "does version `v` satisfy the request?" test, factored
/// out of the request enum so that every ecosystem's range grammar —
/// semver `VersionReq`, PEP 440 `VersionSpecifiers`, Go/Java numeric prefixes —
/// becomes a single case of the ONE [`VersionRequest`]: `Constraint(c)`.
///
/// # Why a trait, not `Box<dyn Fn(&V) -> bool>`
///
/// A boxed closure would collapse the three range grammars into one type, but
/// at the cost of `Clone`, `PartialEq`, `Eq`, and `Serialize` on the request —
/// which real call sites depend on (Go/Java tests `assert_eq!`/`matches!` on
/// request values; `registry::resolve::VersionRequest` is `Serialize`). By
/// making the constraint a *typed* associated parameter `C`, the request derives
/// `Clone`/`Eq`/`Serialize` exactly when `C` does. Concrete constraints
/// (`semver::VersionReq`, `uv_pep440::VersionSpecifiers`, a plain numeric
/// `Vec<u64>` prefix) are all `Clone + PartialEq`, so nothing is lost.
///
/// The trade-off is that a request that could carry *any* of several constraint
/// kinds must name a sum type as its `C` (see `registry::resolve`'s
/// `RangeConstraint`); a request that only ever carries one kind (Go, Java,
/// Python) names that concrete kind directly and keeps `#[derive(Eq)]`.
pub trait Constraint<V> {
    /// Whether `v` satisfies this constraint.
    fn matches(&self, v: &V) -> bool;
}

/// The trivial constraint that admits nothing — the default `C` for requests
/// that never use the `Constraint` case (so the two-case `Latest`/`Exact`
/// requests need not invent a constraint type). It can never be *constructed*
/// as an inhabited value, so its `matches` is unreachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoConstraint {}

impl<V> Constraint<V> for NoConstraint {
    fn matches(&self, _v: &V) -> bool {
        // `NoConstraint` is uninhabited, so no reference to it can exist.
        unreachable!("NoConstraint is uninhabited")
    }
}

/// A numeric-prefix constraint (`[1, 4]` matches every version whose leading
/// numeric segments are `1.4.*`). The shared, ecosystem-neutral form of the
/// Go/Java `Prefix` request; matching is delegated to the grammar's
/// [`VersionGrammar::prefix_matches`], so a bare `PrefixConstraint` on its own
/// (via [`Constraint::matches`]) is a no-op — always route it through a grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixConstraint(pub Vec<u64>);

impl<V> Constraint<V> for PrefixConstraint {
    /// A prefix cannot be evaluated without the grammar's `numeric_prefix`, so
    /// the standalone predicate is intentionally inert. The grammar's
    /// `matches_request` default special-cases prefixes and never calls this.
    fn matches(&self, _v: &V) -> bool {
        false
    }
}

/// A version request against an ecosystem's tag set.
///
/// The shape unifies all four producers plus the registry: `Latest` and `Exact`
/// keep their distinct *selection* semantics (newest-preferred / exact pin),
/// while every range grammar — semver, PEP 440, Go/Java numeric prefixes —
/// collapses into the single `Constraint(c)` case carrying an ecosystem-supplied
/// [`Constraint`] predicate.
///
/// * `V` must be `Ord` so the selection loop can pick the greatest match.
/// * `C` is the constraint type; it defaults to [`NoConstraint`] so requests
///   that only ever use `Latest`/`Exact` need not name one. The request derives
///   `Clone`/`PartialEq`/`Eq` exactly when `V` and `C` do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRequest<V, C = NoConstraint> {
    /// The newest version in the tag set (stable preferred).
    Latest,
    /// An exact version pin: match only this precise version.
    Exact(V),
    /// A general constraint/predicate: the newest version satisfying `C`
    /// (stable preferred). Semver ranges, PEP 440 specifiers, and Go/Java
    /// numeric prefixes are all instances of this one case.
    Constraint(C),
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
/// * `matches_request` has a default implementation that dispatches `Latest`,
///   `Exact`, and `Constraint` (delegating to the constraint's own predicate).
///   Grammars whose constraint needs the grammar itself to evaluate — notably
///   [`PrefixConstraint`], or Go-style major-discipline filtering — override it.
pub trait VersionGrammar {
    /// The parsed version type for this ecosystem.
    type V: Ord + Clone;

    /// The constraint type this grammar's `Constraint` requests carry.
    ///
    /// Fixing the constraint as an associated type (rather than a method-level
    /// generic) lets `matches_request` match *concretely* on the constraint —
    /// e.g. a [`PrefixConstraint`] grammar can reach its own
    /// [`numeric_prefix`](Self::numeric_prefix). Grammars that never use the
    /// `Constraint` case set this to [`NoConstraint`].
    type C: Constraint<Self::V>;

    /// Try to parse `raw_tag` as a version belonging to the module identified
    /// by `ctx`. Return `None` to skip the tag entirely.
    fn parse_tag<'a>(&self, raw_tag: &'a str, ctx: &TagContext<'_>) -> Option<(Self::V, &'a str)>;

    /// Whether `v` is a prerelease (alpha/beta/rc/snapshot/dev/etc.).
    fn is_prerelease(&self, v: &Self::V) -> bool;

    /// Whether `v` satisfies `request` given the filtering context `ctx`.
    ///
    /// The default implementation handles `Latest` (always), `Exact` (equality),
    /// and `Constraint` (delegating to the ecosystem predicate's
    /// [`Constraint::matches`]).  Grammars whose constraint is [`PrefixConstraint`]
    /// — or that need Go-style major-discipline filtering — override this; a bare
    /// `PrefixConstraint::matches` is inert because prefix comparison needs the
    /// grammar's [`numeric_prefix`](Self::numeric_prefix).
    fn matches_request(
        &self,
        v: &Self::V,
        request: &VersionRequest<Self::V, Self::C>,
        _ctx: &TagContext<'_>,
    ) -> bool {
        match request {
            VersionRequest::Latest => true,
            VersionRequest::Exact(want) => v == want,
            VersionRequest::Constraint(constraint) => constraint.matches(v),
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
        prefix
            .iter()
            .enumerate()
            .all(|(i, p)| nums.get(i).copied().unwrap_or(0) == *p)
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
    request: &VersionRequest<G::V, G::C>,
    ctx: &TagContext<'_>,
    grammar: &G,
) -> Option<String>
where
    G: VersionGrammar,
{
    // Collect (parsed_version, original_tag_str) for every tag that both
    // parses and satisfies the request.
    let candidates: Vec<(G::V, String)> = tags
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
pub fn pick_best<V, T>(candidates: &[(V, T)], is_prerelease: impl Fn(&V) -> bool) -> Option<&T>
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

#[cfg(test)]
mod selection_tests {
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
        type C = PrefixConstraint;

        fn parse_tag<'a>(
            &self,
            raw: &'a str,
            _ctx: &TagContext<'_>,
        ) -> Option<(SimpleVer, &'a str)> {
            let rest = raw.strip_prefix('v').unwrap_or(raw);
            let (core, pre) = match rest.split_once('-') {
                Some((c, p)) => (c, Some(p.to_string())),
                None => (rest, None),
            };
            let mut parts = core.split('.');
            let major: u32 = parts.next()?.parse().ok()?;
            let minor: u32 = parts.next()?.parse().ok()?;
            let patch: u32 = parts.next()?.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            Some((
                SimpleVer {
                    major,
                    minor,
                    patch,
                    pre,
                },
                raw,
            ))
        }

        fn is_prerelease(&self, v: &SimpleVer) -> bool {
            v.pre.is_some()
        }

        // This grammar's constraint case is a numeric prefix, so route it
        // through `prefix_matches` (which uses `numeric_prefix`) rather than
        // the inert standalone `PrefixConstraint::matches`.
        fn matches_request(
            &self,
            v: &SimpleVer,
            request: &VersionRequest<SimpleVer, PrefixConstraint>,
            _ctx: &TagContext<'_>,
        ) -> bool {
            match request {
                VersionRequest::Latest => true,
                VersionRequest::Exact(want) => v == want,
                VersionRequest::Constraint(PrefixConstraint(prefix)) => {
                    self.prefix_matches(v, prefix)
                }
            }
        }

        fn numeric_prefix(&self, v: &SimpleVer) -> Vec<u64> {
            vec![u64::from(v.major), u64::from(v.minor), u64::from(v.patch)]
        }
    }

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(std::string::ToString::to_string).collect()
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
        assert!(result.is_some());
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
        let result = resolve_from_tags(
            &t,
            &VersionRequest::Constraint(PrefixConstraint(vec![1, 4])),
            &ctx,
            &SimpleGrammar,
        );
        assert_eq!(result, Some("v1.4.9".to_string()));
    }

    #[test]
    fn prefix_stable_beats_prerelease_in_same_prefix() {
        let t = tags(&["v1.4.9-rc1", "v1.4.8"]);
        let ctx = TagContext::default();
        let result = resolve_from_tags(
            &t,
            &VersionRequest::Constraint(PrefixConstraint(vec![1, 4])),
            &ctx,
            &SimpleGrammar,
        );
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
