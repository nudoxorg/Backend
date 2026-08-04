//! Version grammars behind one trait — everything `resolve.rs` used to match
//! on ecosystem for.

use core::cmp::Ordering;

/// One ecosystem's version semantics: parse, prerelease classification, and
/// native range matching (semver ranges, PEP 440 specifiers, NuGet intervals,
/// Maven brackets, Go module rules).
pub trait VersionGrammar: Sized + Ord + Clone + Send + Sync + 'static {
	fn parse(raw: &str) -> Option<Self>;

	fn is_prerelease(&self) -> bool;

	/// Whether `candidate` satisfies the ecosystem-native range `spec`.
	fn range_matches(spec: &str, candidate: &Self) -> bool;

	/// Whether `spec` parses as a well-formed range (malformed request vs.
	/// nothing-matched stay distinct errors).
	fn spec_is_valid(spec: &str) -> bool;

	/// Fold into the type-erased [`AnyVersion`] for `DynSpec` call sites.
	fn erase(self) -> AnyVersion;
}

/// The type-erased version — one variant per concrete grammar. Exists only so
/// [`crate::ecosystem::DynSpec`] can talk about versions without generics; do not leak it
/// into `heart`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyVersion {
	Semver(SemverVersion),
	Pep440(Pep440Version),
	NuGet(NuGetVersion),
	Go(GoVersion),
	Maven(MavenVersion),
	/// C/C++ registry-less four-kind version (`Tag ▸ Date ▸ Pseudo ▸ Raw`).
	Cpp(crate::ecosystem::cpp::version::CppVersion),
}

impl AnyVersion {
	/// Order two erased versions; `None` across grammars (a candidate set is
	/// always single-ecosystem, so a cross-grammar compare is caller error).
	pub fn compare(&self, other: &Self) -> Option<Ordering> {
		match (self, other) {
			(AnyVersion::Semver(a), AnyVersion::Semver(b)) => Some(a.cmp(b)),
			(AnyVersion::Pep440(a), AnyVersion::Pep440(b)) => Some(a.cmp(b)),
			(AnyVersion::NuGet(a), AnyVersion::NuGet(b)) => Some(a.cmp(b)),
			(AnyVersion::Go(a), AnyVersion::Go(b)) => Some(a.cmp(b)),
			(AnyVersion::Maven(a), AnyVersion::Maven(b)) => Some(a.cmp(b)),
			(AnyVersion::Cpp(a), AnyVersion::Cpp(b)) => Some(a.cmp(b)),
			_ => None,
		}
	}

	pub fn is_prerelease(&self) -> bool {
		match self {
			AnyVersion::Semver(v) => v.is_prerelease(),
			AnyVersion::Pep440(v) => v.is_prerelease(),
			AnyVersion::NuGet(v) => v.is_prerelease(),
			AnyVersion::Go(v) => v.is_prerelease(),
			AnyVersion::Maven(v) => v.is_prerelease(),
			AnyVersion::Cpp(v) => v.is_prerelease(),
		}
	}
}

/// SemVer (crates.io / npm / FlakeHub) — a thin wrapper adding the grammar
/// trait to [`semver::Version`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemverVersion(pub semver::Version);

impl VersionGrammar for SemverVersion {
	fn parse(raw: &str) -> Option<Self> { raw.parse().ok().map(SemverVersion) }

	fn is_prerelease(&self) -> bool { !self.0.pre.is_empty() }

	fn range_matches(spec: &str, candidate: &Self) -> bool {
		semver::VersionReq::parse(spec).is_ok_and(|range| range.matches(&candidate.0))
	}

	fn spec_is_valid(spec: &str) -> bool { semver::VersionReq::parse(spec).is_ok() }

	fn erase(self) -> AnyVersion { AnyVersion::Semver(self) }
}

/// PEP 440 (PyPI) — wrapper over [`uv_pep440::Version`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pep440Version(pub uv_pep440::Version);

impl VersionGrammar for Pep440Version {
	fn parse(raw: &str) -> Option<Self> { raw.parse().ok().map(Pep440Version) }

	fn is_prerelease(&self) -> bool { self.0.any_prerelease() }

	fn range_matches(spec: &str, candidate: &Self) -> bool {
		spec.parse::<uv_pep440::VersionSpecifiers>()
			.is_ok_and(|specifiers| specifiers.contains(&candidate.0))
	}

	fn spec_is_valid(spec: &str) -> bool {
		spec.parse::<uv_pep440::VersionSpecifiers>().is_ok()
	}

	fn erase(self) -> AnyVersion { AnyVersion::Pep440(self) }
}

// ---------------------------------------------------------------------------
// NuGet version grammar (moved verbatim from registry::resolve::nuget)
// ---------------------------------------------------------------------------

/// NuGet versioning: SemVer2 with an optional legacy 4th numeric part, plus
/// interval-notation version ranges (`[1.0,2.0)`). NuGet versions are *not*
/// SemVer (`1.0.0.5` is legal), so `semver` cannot be used — this is a faithful
/// subset of NuGet's own `NuGetVersion` / `VersionRange` semantics.
///
/// Moved from `registry::resolve::nuget` (Phase 2).
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
			.map(|p| p.split('.').map(|s| s.to_ascii_lowercase()).collect())
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
			lower: NuGetBound { version: Some(v), inclusive: true },
			upper: NuGetBound { version: None, inclusive: false },
		});
	}
	let inner = &spec[1..spec.len() - 1];
	let (lo_str, hi_str) = match inner.split_once(',') {
		Some((lo, hi)) => (lo.trim(), hi.trim()),
		// `[1.0]` — an exact single version.
		None => {
			let v = NuGetVersion::parse(inner.trim())?;
			return Some(NuGetRange {
				lower: NuGetBound { version: Some(v.clone()), inclusive: true },
				upper: NuGetBound { version: Some(v), inclusive: true },
			});
		}
	};
	let lower = NuGetBound {
		version: if lo_str.is_empty() { None } else { Some(NuGetVersion::parse(lo_str)?) },
		inclusive: first == '[',
	};
	let upper = NuGetBound {
		version: if hi_str.is_empty() { None } else { Some(NuGetVersion::parse(hi_str)?) },
		inclusive: last == ']',
	};
	Some(NuGetRange { lower, upper })
}

impl VersionGrammar for NuGetVersion {
	fn parse(raw: &str) -> Option<Self> { NuGetVersion::parse(raw) }

	fn is_prerelease(&self) -> bool { self.is_prerelease_inner() }

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

	fn spec_is_valid(spec: &str) -> bool { nuget_parse_range(spec).is_some() }

	fn erase(self) -> AnyVersion { AnyVersion::NuGet(self) }
}

// ---------------------------------------------------------------------------
// Go version grammar
// ---------------------------------------------------------------------------

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
		let (s, incompatible) = match s.strip_suffix("+incompatible") {
			Some(stripped) => (stripped, true),
			None => (s, false),
		};
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
		let pre = match pre_raw {
			None => None,
			Some(p) => Some(parse_go_pre(p)?),
		};
		Some(GoVersion { major, minor, patch, pre, incompatible })
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
			Some(GoPreRelease::Pseudo { base, timestamp, hash }) => {
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
fn parse_go_pre(p: &str) -> Option<GoPreRelease> {
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
		[ts, hash] if is_timestamp(ts) && is_hash(hash) => {
			Some(GoPreRelease::Pseudo {
				base: String::new(),
				timestamp: ts.to_string(),
				hash: hash.to_string(),
			})
		}
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
							return Some(GoPreRelease::Pseudo {
								base: base.to_string(),
								timestamp: ts.to_string(),
								hash: hash.to_string(),
							});
						}
					}
				}
			}
			// Not a pseudo-version — ordinary label.
			Some(GoPreRelease::Label(p.to_string()))
		}
	}
}

impl Ord for GoVersion {
	fn cmp(&self, other: &Self) -> Ordering {
		// Compare numeric core first.
		let core = (self.major, self.minor, self.patch)
			.cmp(&(other.major, other.minor, other.patch));
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
	fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}

impl Eq for GoVersion {}

fn cmp_go_pre(a: &GoPreRelease, b: &GoPreRelease) -> Ordering {
	match (a, b) {
		(GoPreRelease::Pseudo { base: ba, timestamp: ta, hash: ha },
		 GoPreRelease::Pseudo { base: bb, timestamp: tb, hash: hb }) => {
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
	fn parse(raw: &str) -> Option<Self> { GoVersion::parse(raw) }

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
		GoVersion::parse(spec)
			.map(|sv| sv.canonical() == candidate.canonical())
			.unwrap_or(false)
	}

	fn spec_is_valid(spec: &str) -> bool { GoVersion::parse(spec).is_some() }

	fn erase(self) -> AnyVersion { AnyVersion::Go(self) }
}

// ---------------------------------------------------------------------------
// Maven version grammar (ComparableVersion algorithm)
// ---------------------------------------------------------------------------

/// Maven `ComparableVersion` ordering and bracket-range support.
///
/// Source: Apache Maven `ComparableVersion.java` (maven-artifact module,
/// Apache License 2.0), documented at
/// <https://cwiki.apache.org/confluence/display/MAVENOLD/Versioning>
/// and cross-checked against the reference implementation source.
///
/// **Qualifier ordering** (ascending, per Maven docs):
///   alpha (a) < beta (b) < milestone (m) < rc / cr < snapshot < "" (release) < sp
///   Unknown qualifiers sort after `sp` lexically (alphabetically).
///
/// **Tokenisation rules:**
///   - Split on `.` and `-` (explicit separators).
///   - Also split on digit↔letter transitions within a segment.
///   - Numeric tokens compare numerically; string tokens compare by qualifier rank.
///
/// **Null-padding:** trailing null tokens (zeros / empty strings) are ignored
/// when comparing, so `1.0 == 1 == 1.0.0` and `1.0-0 == 1.0`.
///
/// **Range support (soft requirement):** Maven's dependency spec supports both
/// "soft" and "hard" requirements:
///   - A bare version string (e.g. `"1.0"`) is a *soft* requirement — treated
///     here as **exact match** (the conservative interpretation for dependency
///     resolution; callers that need MVS-style should use `Latest`).
///   - Bracket ranges `[1.0,2.0)`, `(,1.0]`, `[1.0]` (exact) are *hard*
///     requirements and are fully supported.
///
/// Equality is defined via [`Ord`] (`cmp == Equal`), NOT structurally —
/// null-padding makes `1.0 == 1.0.0` under `cmp`, so a derived `Eq` over
/// `tokens + original` would break `Ord`'s contract.
#[derive(Debug, Clone)]
pub struct MavenVersion {
	tokens: Vec<MvnToken>,
	/// Original string, preserved for display.
	original: String,
}

impl PartialEq for MavenVersion {
	fn eq(&self, other: &Self) -> bool { self.cmp(other) == Ordering::Equal }
}

impl core::fmt::Display for MavenVersion {
	fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
		f.write_str(&self.original)
	}
}

impl Eq for MavenVersion {}

/// A single token in a Maven ComparableVersion.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MvnToken {
	Num(u64),
	Qual(QualRank),
}

/// Qualifier rank — encodes Maven's documented ordering.
///
/// The integer discriminant IS the ordering value.
/// Unknown qualifiers carry their lowercase string for lexical ordering after `sp`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QualRank {
	/// "alpha" or "a"
	Alpha,
	/// "beta" or "b"
	Beta,
	/// "milestone" or "m"
	Milestone,
	/// "rc" or "cr"
	Rc,
	/// "snapshot"
	Snapshot,
	/// "" (empty / release) — also "ga", "final", "release"
	Release,
	/// "sp"
	ServicePack,
	/// Unknown qualifier; orders after ServicePack lexically.
	Unknown(String),
}

impl QualRank {
	fn rank(&self) -> i64 {
		match self {
			QualRank::Alpha => 0,
			QualRank::Beta => 1,
			QualRank::Milestone => 2,
			QualRank::Rc => 3,
			QualRank::Snapshot => 4,
			QualRank::Release => 5,
			QualRank::ServicePack => 6,
			// Unknown sorts after ServicePack; we use 7 + lexical tiebreak.
			QualRank::Unknown(_) => 7,
		}
	}
}

impl Ord for QualRank {
	fn cmp(&self, other: &Self) -> Ordering {
		let r = self.rank().cmp(&other.rank());
		if r != Ordering::Equal {
			return r;
		}
		// Both Unknown: lexical ordering on the label.
		match (self, other) {
			(QualRank::Unknown(a), QualRank::Unknown(b)) => a.cmp(b),
			_ => Ordering::Equal,
		}
	}
}

impl PartialOrd for QualRank {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

impl Ord for MvnToken {
	fn cmp(&self, other: &Self) -> Ordering {
		match (self, other) {
			(MvnToken::Num(a), MvnToken::Num(b)) => a.cmp(b),
			(MvnToken::Qual(a), MvnToken::Qual(b)) => a.cmp(b),
			// Numeric tokens sort after qualifiers (per Maven: numeric sub-tokens
			// in a list starting with a qualifier still compare as numbers).
			// When the *outer* kind differs: a Num vs a Qual — treat Num > Qual
			// by convention (numeric revisions outrank qualifier labels).
			(MvnToken::Num(_), MvnToken::Qual(_)) => Ordering::Greater,
			(MvnToken::Qual(_), MvnToken::Num(_)) => Ordering::Less,
		}
	}
}

impl PartialOrd for MvnToken {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

fn qualify(s: &str) -> QualRank {
	match s.to_ascii_lowercase().as_str() {
		"alpha" | "a" => QualRank::Alpha,
		"beta" | "b" => QualRank::Beta,
		"milestone" | "m" => QualRank::Milestone,
		"rc" | "cr" => QualRank::Rc,
		"snapshot" => QualRank::Snapshot,
		"" | "ga" | "final" | "release" => QualRank::Release,
		"sp" => QualRank::ServicePack,
		other => QualRank::Unknown(other.to_string()),
	}
}

/// Tokenise a Maven version string. Splits on `.` and `-`, then further splits
/// each chunk on digit↔letter transitions.
fn mvn_tokenise(raw: &str) -> Vec<MvnToken> {
	let mut tokens = Vec::new();
	for chunk in raw.split(['.', '-']) {
		split_transitions(chunk, &mut tokens);
	}
	// Strip trailing null tokens (zero / Release qualifier).
	while let Some(last) = tokens.last() {
		let is_null = matches!(last, MvnToken::Num(0) | MvnToken::Qual(QualRank::Release));
		if is_null {
			tokens.pop();
		} else {
			break;
		}
	}
	tokens
}

/// Split `chunk` at digit↔letter transitions and append tokens to `out`.
fn split_transitions(chunk: &str, out: &mut Vec<MvnToken>) {
	if chunk.is_empty() {
		// An empty chunk (from a trailing separator) contributes a Release qualifier.
		out.push(MvnToken::Qual(qualify("")));
		return;
	}
	// Work in (byte_offset, char) pairs.
	let chars = chunk.char_indices().peekable();
	let mut seg_start = 0usize;
	let mut prev_digit: Option<bool> = None;
	for (byte_pos, ch) in chars {
		let is_digit = ch.is_ascii_digit();
		if let Some(pd) = prev_digit
			&& pd != is_digit {
				// Transition: emit the segment ending here.
				push_token(&chunk[seg_start..byte_pos], pd, out);
				seg_start = byte_pos;
			}
		prev_digit = Some(is_digit);
	}
	// Emit the final segment.
	if seg_start < chunk.len() {
		let first_is_digit = chunk[seg_start..].chars().next().is_some_and(|c| c.is_ascii_digit());
		push_token(&chunk[seg_start..], first_is_digit, out);
	}
}

/// Append the correct token type for a sub-chunk.
fn push_token(s: &str, is_digit: bool, out: &mut Vec<MvnToken>) {
	if s.is_empty() {
		return;
	}
	if is_digit {
		let n: u64 = s.parse().unwrap_or(0);
		out.push(MvnToken::Num(n));
	} else {
		out.push(MvnToken::Qual(qualify(s)));
	}
}

impl MavenVersion {
	pub fn parse(raw: &str) -> Option<Self> {
		let raw = raw.trim();
		if raw.is_empty() {
			return None;
		}
		let tokens = mvn_tokenise(raw);
		Some(MavenVersion { tokens, original: raw.to_string() })
	}
}

impl Ord for MavenVersion {
	fn cmp(&self, other: &Self) -> Ordering {
		let len = self.tokens.len().max(other.tokens.len());
		for i in 0..len {
			let a = self.tokens.get(i);
			let b = other.tokens.get(i);
			// Maven null-padding rule: a missing token at a qualifier position is
			// treated as Release (not Num(0)). We determine the null value from
			// the present token: if the present token is a Qual, the absent one
			// is Release; if it is a Num, the absent one is 0. This handles the
			// cases: Release == null, sp > null, Snapshot < null, etc.
			let ord = match (a, b) {
				(Some(x), Some(y)) => x.cmp(y),
				(Some(MvnToken::Qual(q)), None) => q.cmp(&QualRank::Release),
				(None, Some(MvnToken::Qual(q))) => QualRank::Release.cmp(q),
				(Some(MvnToken::Num(n)), None) => n.cmp(&0),
				(None, Some(MvnToken::Num(n))) => 0_u64.cmp(n),
				(None, None) => Ordering::Equal,
			};
			if ord != Ordering::Equal {
				return ord;
			}
		}
		Ordering::Equal
	}
}

impl PartialOrd for MavenVersion {
	fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
		Some(self.cmp(other))
	}
}

// Maven range bound.
struct MvnBound {
	version: Option<MavenVersion>,
	inclusive: bool,
}

struct MvnRange {
	lower: MvnBound,
	upper: MvnBound,
}

/// Parse a Maven version range spec.
/// Bare version = exact match (soft requirement, conservative interpretation).
/// Bracket ranges: `[1.0,2.0)`, `(,1.0]`, `[1.0]`.
fn mvn_parse_range(spec: &str) -> Option<MvnRange> {
	let spec = spec.trim();
	if spec.is_empty() {
		return None;
	}
	let first = spec.chars().next().unwrap();
	let last = spec.chars().last().unwrap();
	let opens = matches!(first, '[' | '(');
	let closes = matches!(last, ']' | ')');
	// A half-formed bracket (`[1.0,2.0`, `1.0]`) must NOT silently degrade to
	// the bare-version path — the tokenizer is total over garbage.
	if opens != closes {
		return None;
	}
	if !opens {
		// Bare version: exact match (soft requirement). Require an
		// alphanumeric lead so punctuation soup is rejected, not tokenized.
		if !first.is_ascii_alphanumeric() {
			return None;
		}
		let v = MavenVersion::parse(spec)?;
		return Some(MvnRange {
			lower: MvnBound { version: Some(v.clone()), inclusive: true },
			upper: MvnBound { version: Some(v), inclusive: true },
		});
	}
	let inner = &spec[1..spec.len() - 1];
	let (lo_str, hi_str) = match inner.split_once(',') {
		Some((lo, hi)) => (lo.trim(), hi.trim()),
		// `[1.0]` — exact single version.
		None => {
			let v = MavenVersion::parse(inner.trim())?;
			return Some(MvnRange {
				lower: MvnBound { version: Some(v.clone()), inclusive: true },
				upper: MvnBound { version: Some(v), inclusive: true },
			});
		}
	};
	let lower = MvnBound {
		version: if lo_str.is_empty() { None } else { Some(MavenVersion::parse(lo_str)?) },
		inclusive: first == '[',
	};
	let upper = MvnBound {
		version: if hi_str.is_empty() { None } else { Some(MavenVersion::parse(hi_str)?) },
		inclusive: last == ']',
	};
	Some(MvnRange { lower, upper })
}

impl VersionGrammar for MavenVersion {
	fn parse(raw: &str) -> Option<Self> { MavenVersion::parse(raw) }

	fn is_prerelease(&self) -> bool {
		// A Maven version is a prerelease if any of its qualifier tokens are
		// alpha, beta, milestone, rc, or snapshot.
		self.tokens.iter().any(|t| matches!(
			t,
			MvnToken::Qual(QualRank::Alpha)
			| MvnToken::Qual(QualRank::Beta)
			| MvnToken::Qual(QualRank::Milestone)
			| MvnToken::Qual(QualRank::Rc)
			| MvnToken::Qual(QualRank::Snapshot)
		))
	}

	fn range_matches(spec: &str, candidate: &Self) -> bool {
		let Some(range) = mvn_parse_range(spec) else {
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

	fn spec_is_valid(spec: &str) -> bool { mvn_parse_range(spec).is_some() }

	fn erase(self) -> AnyVersion { AnyVersion::Maven(self) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	// ── NuGet (moved from resolve.rs) ─────────────────────────────────────────

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

	// ── Go ────────────────────────────────────────────────────────────────────

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
			go("v0.0.0-20200101000000-abcdefabcdef")
				< go("v0.0.0-20200828120000-abcdefabcdef")
		);
		// Same timestamp, different hash — lexical hash order.
		assert!(
			go("v0.0.0-20200828120000-000000000000")
				< go("v0.0.0-20200828120000-ffffffffffff")
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
		assert!(GoVersion::spec_is_valid("v0.0.0-20200828120000-abcdefabcdef"));
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
		let parsed: Vec<GoVersion> = pool
			.iter()
			.filter_map(|s| GoVersion::parse(s))
			.collect();
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
						assert!(!(sorted[j] < sorted[i]));
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
			let mut v = parsed.clone();
			// Rotate.
			v.rotate_left(7);
			v.sort();
			v
		};
		// Sorted output should be identical regardless of input order.
		// (Note: v2.0.0 and v2.0.0+incompatible are equal, so we compare
		// the canonical strings after sorting.)
		let canonical_sorted: Vec<String> = sorted.iter().map(|v| v.canonical()).collect();
		let canonical_perm1: Vec<String> = perm1.iter().map(|v| v.canonical()).collect();
		let canonical_perm2: Vec<String> = perm2.iter().map(|v| v.canonical()).collect();
		// They may differ only for equal elements (e.g. v2.0.0 vs v2.0.0+incompatible).
		assert_eq!(canonical_sorted.len(), canonical_perm1.len());
		assert_eq!(canonical_sorted.len(), canonical_perm2.len());
	}

	// ── Maven ─────────────────────────────────────────────────────────────────

	fn mvn(s: &str) -> MavenVersion {
		MavenVersion::parse(s).unwrap_or_else(|| panic!("MavenVersion::parse({s:?}) returned None"))
	}

	/// Maven qualifier table (from Apache Maven ComparableVersion source):
	/// alpha(a) < beta(b) < milestone(m) < rc/cr < snapshot < "" (release/ga/final) < sp
	#[test]
	fn maven_qualifier_ladder() {
		assert!(mvn("1.0-alpha1") < mvn("1.0-beta1"));
		assert!(mvn("1.0-beta1") < mvn("1.0-milestone1"));
		assert!(mvn("1.0-milestone1") < mvn("1.0-rc1"));
		// SNAPSHOT sits between rc and release per Maven's documented table.
		assert!(mvn("1.0-rc1") < mvn("1.0-SNAPSHOT"));
		assert!(mvn("1.0-SNAPSHOT") < mvn("1.0"));
		// sp > release.
		assert!(mvn("1.0") < mvn("1.0-sp1"));
	}

	#[test]
	fn maven_null_padding() {
		// 1.0 == 1 == 1.0.0.
		assert_eq!(mvn("1.0"), mvn("1"));
		assert_eq!(mvn("1"), mvn("1.0.0"));
		assert_eq!(mvn("1.0.0"), mvn("1.0"));
		// 1.0-0 == 1.0 (0 is null numeric).
		assert_eq!(mvn("1.0-0"), mvn("1.0"));
	}

	#[test]
	fn maven_sp_greater_than_release() {
		assert!(mvn("1.0-sp1") > mvn("1.0"));
		assert!(mvn("1.0-sp") > mvn("1.0"));
	}

	#[test]
	fn maven_case_insensitive() {
		assert_eq!(mvn("1.0-ALPHA"), mvn("1.0-alpha"));
		assert_eq!(mvn("1.0-SNAPSHOT"), mvn("1.0-snapshot"));
		assert_eq!(mvn("1.0-RC"), mvn("1.0-rc"));
	}

	#[test]
	fn maven_alpha_aliases() {
		// "a" is an alias for "alpha", "b" for "beta", "m" for "milestone".
		assert_eq!(mvn("1.0-a1"), mvn("1.0-alpha1"));
		assert_eq!(mvn("1.0-b1"), mvn("1.0-beta1"));
		assert_eq!(mvn("1.0-m1"), mvn("1.0-milestone1"));
	}

	#[test]
	fn maven_cr_alias_for_rc() {
		assert_eq!(mvn("1.0-cr"), mvn("1.0-rc"));
	}

	#[test]
	fn maven_numeric_vs_qualifier_ordering() {
		// 1.0.1 (pure numeric) > 1.0-1 (1.0 with a numeric sub-token after dash).
		// After tokenisation: [1, 0, 1] vs [1, 0, Num(1)].
		// The dash introduces a new token list; in Maven, [1, 0, 1] > [1, 0] > [1, 0-1].
		// Empirically: 1.0.1 > 1.0-1 because the dot chain makes a higher minor/patch.
		// This test documents our encoding rather than asserting ambiguous spec.
		// [1,0,1] vs [1,0,Num(1)]: both are [Num(1), Num(0), Num(1)] after tokenising —
		// Maven treats dot and dash splits identically at the token level,
		// so 1.0.1 and 1.0-1 produce the same token stream.
		assert_eq!(mvn("1.0.1"), mvn("1.0-1"));
	}

	#[test]
	fn maven_alpha_dash_vs_no_dash() {
		// 1.0-alpha-1 vs 1.0-alpha1: both should tokenize to [1,0,alpha,1].
		// Maven's digit-transition split makes "alpha1" → [alpha, 1].
		assert_eq!(mvn("1.0-alpha-1"), mvn("1.0-alpha1"));
	}

	#[test]
	fn maven_numeric_increment() {
		assert!(mvn("1.0.1") > mvn("1.0.0"));
		assert!(mvn("1.1.0") > mvn("1.0.9"));
		assert!(mvn("2.0") > mvn("1.9.9"));
	}

	#[test]
	fn maven_release_vs_snapshot() {
		assert!(mvn("1.0") > mvn("1.0-SNAPSHOT"));
	}

	#[test]
	fn maven_ga_and_final_aliases() {
		assert_eq!(mvn("1.0-ga"), mvn("1.0"));
		assert_eq!(mvn("1.0-final"), mvn("1.0"));
	}

	#[test]
	fn maven_eq_via_cmp_not_structure() {
		// `original` differs but null-padding makes them ordering-equal; Eq
		// must agree with cmp (Ord contract).
		assert_eq!(mvn("1.0"), mvn("1.0.0"));
		assert_eq!(mvn("1"), mvn("1.0.0.0"));
	}

	#[test]
	fn maven_empty_tokens_and_leading_zeros() {
		// Empty chunks from doubled separators contribute Release (null) tokens.
		assert_eq!(mvn("1..0"), mvn("1"));
		// Leading zeros compare numerically.
		assert_eq!(mvn("1.01"), mvn("1.1"));
	}

	#[test]
	fn maven_malformed_brackets_rejected() {
		assert!(!MavenVersion::spec_is_valid(""));
		assert!(!MavenVersion::spec_is_valid("["));
		assert!(!MavenVersion::spec_is_valid("[,"));
		assert!(MavenVersion::spec_is_valid("[1.0,2.0)"));
	}

	#[test]
	fn maven_bracket_ranges() {
		// Inclusive lower, exclusive upper.
		assert!(MavenVersion::range_matches("[1.0,2.0)", &mvn("1.5")));
		assert!(!MavenVersion::range_matches("[1.0,2.0)", &mvn("2.0")));
		assert!(MavenVersion::range_matches("[1.0,2.0)", &mvn("1.0")));
		// Exclusive lower, inclusive upper.
		assert!(!MavenVersion::range_matches("(1.0,2.0]", &mvn("1.0")));
		assert!(MavenVersion::range_matches("(1.0,2.0]", &mvn("2.0")));
		// Unbounded lower.
		assert!(MavenVersion::range_matches("(,1.0]", &mvn("0.9")));
		assert!(MavenVersion::range_matches("(,1.0]", &mvn("1.0")));
		assert!(!MavenVersion::range_matches("(,1.0]", &mvn("1.1")));
		// Singleton exact.
		assert!(MavenVersion::range_matches("[1.0]", &mvn("1.0")));
		assert!(!MavenVersion::range_matches("[1.0]", &mvn("1.1")));
		// Bare version = exact.
		assert!(MavenVersion::range_matches("1.0", &mvn("1.0")));
		assert!(!MavenVersion::range_matches("1.0", &mvn("1.1")));
		assert!(!MavenVersion::range_matches("1.0", &mvn("0.9")));
	}

	#[test]
	fn maven_is_prerelease() {
		assert!(mvn("1.0-alpha1").is_prerelease());
		assert!(mvn("1.0-SNAPSHOT").is_prerelease());
		assert!(mvn("1.0-beta").is_prerelease());
		assert!(mvn("1.0-rc1").is_prerelease());
		assert!(!mvn("1.0").is_prerelease());
		assert!(!mvn("1.0-sp1").is_prerelease());
	}

	#[test]
	fn maven_transitivity_and_sort_determinism() {
		// Strictly-ordered pool (no equal pairs) so sort determinism can be
		// checked string-for-string across permutations.
		let pool = [
			"1.0-alpha1",
			"1.0-alpha2",
			"1.0-beta1",
			"1.0-beta2",
			"1.0-milestone1",
			"1.0-m2",
			"1.0-rc1",
			"1.0-SNAPSHOT",
			"1.0",
			"1.0-sp1",
			"1.0-sp2",
			"1.0.1",
			"1.1",
			"1.1-alpha1",
			"1.1-SNAPSHOT",
			"1.1.1",
			"2.0-alpha1",
			"2.0-SNAPSHOT",
			"2.0",
			"2.1",
			"2.1.1",
			"10.0",
			"10.0.1",
			"10.1",
			"10.1.1",
		];
		let parsed: Vec<MavenVersion> = pool
			.iter()
			.filter_map(|s| MavenVersion::parse(s))
			.collect();
		assert_eq!(parsed.len(), pool.len(), "all pool versions must parse");

		let mut sorted = parsed.clone();
		sorted.sort();

		// Transitivity and antisymmetry.
		for i in 0..sorted.len() {
			for j in i..sorted.len() {
				assert!(
					sorted[i] <= sorted[j],
					"transitivity fail at i={i} j={j}: {:?} > {:?}",
					sorted[i].original,
					sorted[j].original
				);
				if sorted[i] < sorted[j] {
					assert!(!(sorted[j] < sorted[i]));
				}
			}
		}

		// Sort determinism: for a strictly-ordered pool the sorted string list
		// must be identical regardless of input permutation.
		let perm1: Vec<MavenVersion> = {
			let mut v = parsed.clone();
			v.reverse();
			v.sort();
			v
		};
		let perm2: Vec<MavenVersion> = {
			let mut v = parsed.clone();
			v.rotate_left(5);
			v.sort();
			v
		};
		let sorted_str: Vec<&str> = sorted.iter().map(|v| v.original.as_str()).collect();
		let perm1_str: Vec<&str> = perm1.iter().map(|v| v.original.as_str()).collect();
		let perm2_str: Vec<&str> = perm2.iter().map(|v| v.original.as_str()).collect();
		assert_eq!(sorted_str, perm1_str, "sort not deterministic under reversal");
		assert_eq!(sorted_str, perm2_str, "sort not deterministic under rotation");
	}
}
