//! The C/C++ registry-less version grammar ([`CppVersion`]).
//!
//! Unlike registry ecosystems, `cpp` has no single version authority. Tags are
//! the primary signal, but they are loose (`1.2.3`, `v1.2`, `release-3`), vcpkg
//! contributes `version-date` pins, untagged commits need Go-style
//! pseudo-versions, and everything else falls back to a raw lexical string.
//!
//! Total order across kinds (REGISTRYLESS §3.3, RL-4):
//! `Tag > Date > Pseudo > Raw`, natural order within a kind.

use core::cmp::Ordering;

use smol_str::SmolStr;

use crate::ecosystem::version::{AnyVersion, VersionGrammar};

/// A C/C++ package version under the four-kind grammar (REGISTRYLESS §3.3).
///
/// Kinds are totally ordered `Tag > Date > Pseudo > Raw`; within a kind the
/// natural (semantic) order applies. Equality is defined via [`Ord`] so the
/// `Ord`/`Eq` contract cannot drift.
#[derive(Debug, Clone)]
pub enum CppVersion {
    /// A loose semver-ish tag: `[v]MAJOR[.MINOR[.PATCH[.EXTRA]]][-pre][+build]`.
    Tag(TagVersion),
    /// A vcpkg `version-date`: `YYYY-MM-DD` with an optional port-version.
    Date {
        /// Four-digit calendar year.
        year: u16,
        /// Month, `1..=12`.
        month: u8,
        /// Day, `1..=31`.
        day: u8,
        /// vcpkg `port-version` (the `#N` suffix); `0` when absent.
        port_version: u32,
    },
    /// A Go-grammar pseudo-version for an untagged commit (three forms).
    Pseudo {
        /// The nearest-ancestor tag this pseudo-version derives from, when one
        /// exists (Go forms 2 and 3). `None` is Go form 1 (`vX.0.0-…`, no
        /// earlier tag; the major is not retained — see [`parse_pseudo`]).
        /// When `Some`, form 3's prerelease label (the `<pre>` in
        /// `vX.Y.Z-pre.0.<ts>-<hash>`) is folded into the tag's own `pre`
        /// field so it participates in ordering and round-trips through
        /// [`CppVersion::canonical`].
        base: Option<Box<TagVersion>>,
        /// Commit timestamp, UTC, as a 14-digit `YYYYMMDDHHMMSS` value.
        timestamp: u64,
        /// The 12-hex-character commit-hash prefix.
        hash12: SmolStr,
    },
    /// A lexical last-resort version (branch names, exotic tags).
    Raw(SmolStr),
}

/// The kind rank used for the cross-kind total order (`Tag` highest).
fn kind_rank(version: &CppVersion) -> u8 {
    match version {
        CppVersion::Tag(_) => 3,
        CppVersion::Date { .. } => 2,
        CppVersion::Pseudo { .. } => 1,
        CppVersion::Raw(_) => 0,
    }
}

impl CppVersion {
    /// The total canonical string rendering across all four kinds.
    ///
    /// - `Tag`   → [`TagVersion::canonical`] (`vMAJOR.MINOR.PATCH[.EXTRA][-pre]`).
    /// - `Date`  → `YYYY.MM.DD[.port]` (normalized: zero-padded, dot-separated,
    ///   the `port_version` appended as a fourth field only when non-zero).
    /// - `Pseudo`→ the exact Go pseudo-version string it parsed from
    ///   ([`synthesize_pseudo_version`] over its base/timestamp/hash).
    /// - `Raw`   → the verbatim lexical string.
    ///
    /// # Round-trip contract
    ///
    /// For `Tag`, `Pseudo`, and `Raw`, `parse_lossless(v.canonical())` yields a
    /// value equal to `v` (same kind, same order class). `Date` renders to a
    /// *normalized* `YYYY.MM.DD` form that is NOT the vcpkg `YYYY-MM-DD` input
    /// grammar, so it does not re-parse back to `Date` — it is a stable display
    /// form, deliberately outside the parse round-trip (the `.`-separated shape
    /// avoids colliding with the `-`-separated vcpkg date the parser consumes).
    pub fn canonical(&self) -> String {
        match self {
            CppVersion::Tag(tag) => tag.canonical(),
            CppVersion::Date {
                year,
                month,
                day,
                port_version,
            } => {
                let mut string = format!("{year:04}.{month:02}.{day:02}");
                if *port_version != 0 {
                    string.push('.');
                    string.push_str(&port_version.to_string());
                }
                string
            }
            CppVersion::Pseudo {
                base,
                timestamp,
                hash12,
            } => {
                // Reconstruct the exact Go pseudo-version string from its parts, as
                // they were parsed (NOT via `synthesize_pseudo_version`, which
                // *bumps* an ancestor release — that is the id-derivation direction,
                // not the inverse of parsing). `base` holds the displayed numeric
                // core; `None` is Go form 1 (`v0.0.0-<ts>-<hash>`). `Some(core)` is
                // form 2 (`v<core>-0.<ts>-<hash>`) when `core` carries no
                // prerelease, or form 3 (`v<core>.0.<ts>-<hash>`, i.e. the `0.<ts>`
                // continues the tag's own `-pre` dot-list with `.` rather than
                // opening a new `-`-separated segment) when it does.
                base.as_ref().map_or_else(
                    || format!("v0.0.0-{timestamp}-{hash12}"),
                    |core| {
                        let separator = if core.has_prerelease() { "." } else { "-" };
                        format!("{}{separator}0.{timestamp}-{hash12}", core.canonical())
                    },
                )
            }
            CppVersion::Raw(raw) => raw.to_string(),
        }
    }

    /// Parse a raw version string into the highest-fidelity kind that accepts
    /// it. Never fails: an otherwise unparseable string becomes [`CppVersion::Raw`].
    pub fn parse_lossless(raw: &str) -> CppVersion {
        let trimmed = raw.trim();
        if let Some(date) = parse_version_date(trimmed) {
            return date;
        }
        if let Some(pseudo) = parse_pseudo(trimmed) {
            return pseudo;
        }
        if let Some(tag) = TagVersion::parse(trimmed) {
            return CppVersion::Tag(tag);
        }
        CppVersion::Raw(SmolStr::from(trimmed))
    }
}

impl Ord for CppVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let rank = kind_rank(self).cmp(&kind_rank(other));
        if rank != Ordering::Equal {
            return rank;
        }
        match (self, other) {
            (CppVersion::Tag(a), CppVersion::Tag(b)) => a.cmp(b),
            (
                CppVersion::Date {
                    year: ya,
                    month: ma,
                    day: da,
                    port_version: pa,
                },
                CppVersion::Date {
                    year: yb,
                    month: mb,
                    day: db,
                    port_version: pb,
                },
            ) => (ya, ma, da, pa).cmp(&(yb, mb, db, pb)),
            (
                CppVersion::Pseudo {
                    base: ba,
                    timestamp: ta,
                    hash12: ha,
                },
                CppVersion::Pseudo {
                    base: bb,
                    timestamp: tb,
                    hash12: hb,
                },
            ) => base_cmp(ba, bb)
                .then_with(|| ta.cmp(tb))
                .then_with(|| ha.cmp(hb)),
            (CppVersion::Raw(a), CppVersion::Raw(b)) => a.cmp(b),
            // Unreachable: equal ranks imply equal kinds for the four variants.
            _ => Ordering::Equal,
        }
    }
}

/// Order two optional pseudo-version base tags: `None` (Go form 1) sorts before
/// any concrete ancestor tag.
fn base_cmp(a: &Option<Box<TagVersion>>, b: &Option<Box<TagVersion>>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.cmp(y),
    }
}

impl PartialOrd for CppVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for CppVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for CppVersion {}

impl VersionGrammar for CppVersion {
    /// Total by construction: [`CppVersion::parse_lossless`] never returns
    /// `None`, so `parse` is always `Some`. (`None` stays part of the trait
    /// surface only to satisfy the shared signature; other ecosystems reject.)
    fn parse(raw: &str) -> Option<Self> {
        Some(CppVersion::parse_lossless(raw))
    }

    fn is_prerelease(&self) -> bool {
        match self {
            CppVersion::Pseudo { .. } => true,
            CppVersion::Tag(tag) => tag.has_prerelease(),
            CppVersion::Date { .. } | CppVersion::Raw(_) => false,
        }
    }

    /// Exact-equality by default (`cmp == Equal`), plus caret semantics
    /// (`^1.2.3`) when both the spec and the candidate parse as [`TagVersion`]
    /// — mirroring go.rs's deliberate minimalism (no invented range syntax).
    fn range_matches(spec: &str, candidate: &Self) -> bool {
        if let Some(rest) = spec.trim().strip_prefix('^')
            && let (Some(spec_tag), CppVersion::Tag(cand_tag)) =
                (TagVersion::parse(rest), candidate)
        {
            return caret_matches(&spec_tag, cand_tag);
        }
        CppVersion::parse_lossless(spec) == *candidate
    }

    /// Always well-formed: every string is a valid `cpp` version (worst case
    /// [`CppVersion::Raw`]).
    fn spec_is_valid(_spec: &str) -> bool {
        true
    }

    fn erase(self) -> AnyVersion {
        AnyVersion::Cpp(self)
    }
}

/// Caret compatibility: `candidate` is compatible with `^spec` when it is `>=`
/// spec and shares the left-most non-zero version component (npm caret rule).
fn caret_matches(spec: &TagVersion, candidate: &TagVersion) -> bool {
    if candidate < spec {
        return false;
    }
    // Determine the compatibility component: the first non-zero of
    // (major, minor, patch); versions below that component's next increment
    // are compatible.
    if spec.major != 0 {
        candidate.major == spec.major
    } else if spec.minor != 0 {
        candidate.major == 0 && candidate.minor == spec.minor
    } else {
        candidate.major == 0 && candidate.minor == 0 && candidate.patch == spec.patch
    }
}

// ── TagVersion ──────────────────────────────────────────────────────────────

/// A loose, semver-ish tag version: an optional `v` prefix, one to four numeric
/// components, an optional `-prerelease` label, and an ignored `+build` suffix.
///
/// Looseness (vs strict semver): missing minor/patch default to `0`, and a
/// fourth `EXTRA` component is accepted (common in C libraries, e.g. OpenSSL's
/// `1.1.1w`-style four-part schemes render as three numeric parts plus a
/// prerelease). Ordering follows semver: numeric core first, then a release
/// outranking any prerelease, prerelease labels compared as dotted identifier
/// lists (numeric numerically, before alphanumerics).
#[derive(Debug, Clone)]
pub struct TagVersion {
    /// Major component.
    pub major: u64,
    /// Minor component (`0` when omitted).
    pub minor: u64,
    /// Patch component (`0` when omitted).
    pub patch: u64,
    /// Fourth numeric component (`0` when omitted); some C libs carry it.
    pub extra: u64,
    /// The prerelease label after the first `-` (before any `+`), lowercased;
    /// empty for a release.
    pre: String,
}

impl TagVersion {
    /// Parse a loose tag version, or `None` when the numeric lead is absent.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        // Optional leading `v`/`V`, or a leading `release-`/`rel-` style word is
        // NOT stripped here (that stays a Raw). Only a bare `v` prefix is loose.
        let body = raw
            .strip_prefix('v')
            .or_else(|| raw.strip_prefix('V'))
            .unwrap_or(raw);
        if body.is_empty() {
            return None;
        }
        // Split off build metadata (ignored) then prerelease.
        let body = body.split('+').next().unwrap_or(body);
        let (numeric, pre_raw) = match body.split_once('-') {
            Some((n, p)) => (n, p),
            None => (body, ""),
        };
        // Numeric core: 1..=4 dot-separated integer components; first must exist.
        let mut components = [0u64; 4];
        let mut count = 0usize;
        for (index, segment) in numeric.split('.').enumerate() {
            if index >= 4 {
                return None; // more than four numeric components: not a loose tag
            }
            components[index] = segment.parse::<u64>().ok()?;
            count = index + 1;
        }
        if count == 0 {
            return None;
        }
        Some(TagVersion {
            major: components[0],
            minor: components[1],
            patch: components[2],
            extra: components[3],
            pre: pre_raw.to_ascii_lowercase(),
        })
    }

    /// Whether this tag carries a prerelease label.
    pub fn has_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    /// The canonical `vMAJOR.MINOR.PATCH[.EXTRA][-pre]` rendering.
    pub fn canonical(&self) -> String {
        let mut string = format!("v{}.{}.{}", self.major, self.minor, self.patch);
        if self.extra != 0 {
            string.push('.');
            string.push_str(&self.extra.to_string());
        }
        if !self.pre.is_empty() {
            string.push('-');
            string.push_str(&self.pre);
        }
        string
    }
}

impl Ord for TagVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let core = (self.major, self.minor, self.patch, self.extra).cmp(&(
            other.major,
            other.minor,
            other.patch,
            other.extra,
        ));
        if core != Ordering::Equal {
            return core;
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater, // release > prerelease
            (false, true) => Ordering::Less,
            (false, false) => cmp_dotted_identifiers(&self.pre, &other.pre),
        }
    }
}

impl PartialOrd for TagVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for TagVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for TagVersion {}

/// Semver §11 prerelease comparison over dot-separated identifiers: numeric
/// identifiers compare numerically and sort before alphanumerics; a shorter
/// prefix list sorts first.
fn cmp_dotted_identifiers(a: &str, b: &str) -> Ordering {
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

// ── version-date parsing (vcpkg) ─────────────────────────────────────────────

/// Parse a vcpkg `version-date` string: `YYYY-MM-DD` with an optional `#N`
/// port-version suffix. Rejects anything that is not exactly a calendar date.
fn parse_version_date(raw: &str) -> Option<CppVersion> {
    let (date_part, port_part) = match raw.split_once('#') {
        Some((date, port)) => (date, Some(port)),
        None => (raw, None),
    };
    let mut fields = date_part.split('-');
    let year = fields.next()?;
    let month = fields.next()?;
    let day = fields.next()?;
    if fields.next().is_some() {
        return None; // extra `-` segments: not a date
    }
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let year: u16 = year.parse().ok()?;
    let month: u8 = month.parse().ok()?;
    let day: u8 = day.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let port_version = match port_part {
        Some(port) => port.parse::<u32>().ok()?,
        None => 0,
    };
    Some(CppVersion::Date {
        year,
        month,
        day,
        port_version,
    })
}

// ── pseudo-version parsing (Go grammar) ──────────────────────────────────────

/// Parse a Go-grammar pseudo-version into [`CppVersion::Pseudo`], or `None`.
///
/// Accepts the three Go forms (matching `golang.org/x/mod/module.PseudoVersion`):
///   `vX.0.0-YYYYMMDDHHMMSS-hash12`            (form 1, no ancestor tag; minor
///                                              and patch MUST be `0`)
///   `vX.Y.Z-0.YYYYMMDDHHMMSS-hash12`          (form 2, ancestor is a release)
///   `vX.Y.Z-pre.0.YYYYMMDDHHMMSS-hash12`      (form 3, ancestor is a prerelease
///                                              `vX.Y.Z-pre`; the `pre` label is
///                                              folded into the returned tag)
///
/// Strings that merely *look* like a pseudo-version but don't match one of
/// these three exact shapes (e.g. `v1.2.3-20200828120000-abcdefabcdef`, whose
/// nonzero minor/patch rules out form 1, and which is also missing form 2/3's
/// `0.` infix) are rejected here so the caller falls back to [`TagVersion`] or
/// [`CppVersion::Raw`] instead of silently discarding the numeric core.
fn parse_pseudo(raw: &str) -> Option<CppVersion> {
    let body = raw.strip_prefix('v').or_else(|| raw.strip_prefix('V'))?;
    let (numeric, pre) = body.split_once('-')?;
    let base_tag = TagVersion::parse(numeric)?;
    // The pre portion ends with `-hash12`; before it a `.`-terminated
    // 14-digit timestamp, optionally preceded by a base label.
    let dash = pre.rfind('-')?;
    let hash = &pre[dash + 1..];
    if !is_hash12(hash) {
        return None;
    }
    let before_hash = &pre[..dash];
    // Form 1: the whole `before_hash` is the timestamp, and the numeric core
    // must be exactly `X.0.0` (minor/patch/extra all zero, no prerelease —
    // the latter holds automatically since `numeric` is dash-free by
    // construction). A tag like `1.2.3` here is NOT a valid Go pseudo-version
    // core for form 1; reject so the caller reclassifies the whole string.
    if is_timestamp(before_hash) {
        if base_tag.minor != 0 || base_tag.patch != 0 || base_tag.extra != 0 {
            return None;
        }
        let timestamp = before_hash.parse::<u64>().ok()?;
        return Some(CppVersion::Pseudo {
            base: None,
            timestamp,
            hash12: SmolStr::from(hash),
        });
    }
    // Forms 2/3: `<base_label>.<timestamp>`, where `base_label` is exactly
    // `0` (form 2, release ancestor) or `<pre>.0` (form 3, prerelease
    // ancestor `vX.Y.Z-pre`). Anything else is not a real Go pseudo-version
    // shape and is rejected rather than guessed at.
    let dot = before_hash.rfind('.')?;
    let timestamp_str = &before_hash[dot + 1..];
    if !is_timestamp(timestamp_str) {
        return None;
    }
    let timestamp = timestamp_str.parse::<u64>().ok()?;
    let base_label = &before_hash[..dot];
    let resolved_base = if base_label == "0" {
        // Form 2: release ancestor, no prerelease label to fold in.
        base_tag
    } else {
        // Form 3: prerelease ancestor. Fold the label back into the tag so it
        // participates in ordering and round-trips via `canonical()`.
        let pre_label = base_label.strip_suffix(".0")?;
        if pre_label.is_empty() {
            return None;
        }
        let mut tag = base_tag;
        tag.pre = pre_label.to_ascii_lowercase();
        tag
    };
    Some(CppVersion::Pseudo {
        base: Some(Box::new(resolved_base)),
        timestamp,
        hash12: SmolStr::from(hash),
    })
}

/// Whether `s` is exactly 12 lowercase-or-mixed hex characters.
fn is_hash12(s: &str) -> bool {
    s.len() == 12 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether `s` is exactly a 14-digit `YYYYMMDDHHMMSS` timestamp.
fn is_timestamp(s: &str) -> bool {
    s.len() == 14 && s.chars().all(|c| c.is_ascii_digit())
}

// ── pseudo-version synthesis (P6 helper; pure) ───────────────────────────────

/// Synthesize a Go-grammar pseudo-version string from an optional nearest
/// ancestor tag, a commit timestamp, and a 12-hex commit hash (REGISTRYLESS
/// §3.3). Pure: grit wiring (finding the ancestor tag, the commit time) is a
/// later wave.
///
/// Forms (matching Go's `module.PseudoVersion`):
/// - No ancestor tag → `v0.0.0-<ts>-<hash12>` (form 1).
/// - Ancestor is a release `vX.Y.Z` → `vX.Y.(Z+1)-0.<ts>-<hash12>` (form 2).
/// - Ancestor is a prerelease `vX.Y.Z-pre` → `vX.Y.Z-pre.0.<ts>-<hash12>`
///   (form 3).
///
/// Returns `None` when `hash12` is not 12 hex chars or `timestamp` is not a
/// 14-digit value.
pub fn synthesize_pseudo_version(
    nearest_ancestor_tag: Option<&TagVersion>,
    timestamp: u64,
    hash12: &str,
) -> Option<String> {
    if !is_hash12(hash12) {
        return None;
    }
    let timestamp_str = timestamp.to_string();
    if timestamp_str.len() != 14 {
        return None;
    }
    let rendered = match nearest_ancestor_tag {
        // Form 1: no ancestor.
        None => format!("v0.0.0-{timestamp_str}-{hash12}"),
        Some(tag) if tag.has_prerelease() => {
            // Form 3: append `.0.<ts>-<hash>` to the existing prerelease.
            format!("{}.0.{timestamp_str}-{hash12}", tag.canonical())
        }
        Some(tag) => {
            // Form 2: bump patch, insert `-0.<ts>-<hash>`.
            let mut bumped = tag.clone();
            bumped.patch = tag.patch.saturating_add(1);
            bumped.extra = 0;
            format!(
                "v{}.{}.{}-0.{timestamp_str}-{hash12}",
                bumped.major, bumped.minor, bumped.patch
            )
        }
    };
    Some(rendered)
}

#[cfg(test)]
mod tests;
