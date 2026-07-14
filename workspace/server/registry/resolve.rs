//! Version resolution: turning a package *name* + a version *request* into a
//! concrete, fetchable [`PackageCoordinates`].
//!
//! A caller asks for `requests` at `>=2.0,<3` from PyPI; resolution consults the
//! source's published version set, picks the highest version satisfying the
//! request under the ecosystem's grammar (SemVer for crates/npm, PEP 440 for
//! Python), and yields the exact [`PackageVersion`] + [`RegistryOrigin`] the
//! sync engine will materialize. Identity (the [`PackageId`]) then falls out of
//! [`PackageCoordinates::id`].

use heart::{
	PackageVersion, RegistryOrigin,
	ecosystem::Language,
};
use crate::registry::package::{Coordinates as PackageCoordinates, PackageName};

use crate::registry::error::ResolveError;

// The shared version vocabulary: the request enum, its constraint predicate,
// and the pick_best selection primitive.
use version::{Constraint, pick_best};

/// A version request against a package.
///
/// This is the shared [`version::VersionRequest`] specialised to
/// [`PackageVersion`] with [`RangeConstraint`] as its constraint case — so a
/// SemVer range and a raw PEP 440 (or other ecosystem) range both fold into the
/// single `Constraint` arm rather than living as bespoke variants. `Latest` and
/// `Exact` keep their distinct selection semantics.
pub type VersionRequest = version::VersionRequest<PackageVersion, RangeConstraint>;

/// The range constraint a registry request can carry: either a parsed SemVer
/// range, or a raw ecosystem range string parsed lazily under that ecosystem's
/// grammar (covers PEP 440 specifiers SemVer cannot represent).
///
/// The variant also records *which* [`ResolveError`] a no-match should produce
/// (`NoMatchSemver` vs `NoMatchRange`), so folding the two old request variants
/// into one constraint case loses none of the rich error reporting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RangeConstraint {
	/// A SemVer range (crates.io / npm / nix), e.g. `^1.2`, `>=2,<3`.
	Semver(semver::VersionReq),

	/// A raw ecosystem range string, parsed under `ecosystem`'s grammar.
	Range { ecosystem: Language, spec: String },
}

impl Constraint<PackageVersion> for RangeConstraint {
	fn matches(&self, candidate: &PackageVersion) -> bool {
		match self {
			RangeConstraint::Semver(range) => semver_matches(range, candidate),
			RangeConstraint::Range { ecosystem, spec } => match ecosystem {
				Language::Rust | Language::Typescript | Language::Nix => {
					// A malformed range admits nothing; `select` re-parses to
					// surface `MalformedRequest` distinctly on the error path.
					semver::VersionReq::parse(spec)
						.is_ok_and(|range| semver_matches(&range, candidate))
				}
				Language::Python => match candidate {
					PackageVersion::Python(version) => spec
						.parse::<uv_pep440::VersionSpecifiers>()
						.is_ok_and(|specifiers| specifiers.contains(version)),
					_ => false,
				},
				// NuGet interval notation (`[1.0,2.0)`, bare `1.2.3` = "≥ min").
				Language::CSharp => match candidate {
					PackageVersion::CSharp(version) => nuget::range_matches(spec, version),
					_ => false,
				},
				// Go/Java resolve via git tags, not this registry range path.
				Language::Go | Language::Java => false,
			},
		}
	}
}

/// A source to resolve against — a public registry, or a self-hosted origin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResolveSource {
	/// The registry origin published versions are read from.
	pub origin: RegistryOrigin,
}

/// Resolve `name` + `request` against `source` to concrete coordinates.
///
/// Fetches the published version set, filters to those satisfying `request`
/// under the ecosystem's version grammar, and selects the greatest — assembling
/// validated [`PackageCoordinates`] whose [`PackageId`](heart::PackageId)
/// is then deterministic. `// registry version-list fetch is network I/O`.
#[tracing::instrument(skip(source), fields(origin = %source.origin.token()))]
pub async fn resolve(
	source: &ResolveSource,
	name: &PackageName,
	request: &VersionRequest,
) -> Result<PackageCoordinates, ResolveError> {
	let published = published_versions(&source.origin, name).await?;
	tracing::debug!(candidates = published.len(), "published version set fetched");
	let version = select(request, &published).map_err(|error| match error {
		// `select` works over a bare candidate set; re-attach the name here.
		ResolveError::NoMatchLatest { .. } => ResolveError::NoMatchLatest { name: name.original().to_owned() },
		ResolveError::NoMatchExact { pin, .. } => ResolveError::NoMatchExact { name: name.original().to_owned(), pin },
		ResolveError::NoMatchSemver { range, .. } => ResolveError::NoMatchSemver { name: name.original().to_owned(), range },
		ResolveError::NoMatchRange { spec, .. } => ResolveError::NoMatchRange { name: name.original().to_owned(), spec },
		other => other,
	})?;
	Ok(PackageCoordinates { origin: source.origin.clone(), name: name.clone(), version })
}

/// Select the greatest version from a candidate set satisfying `request`. Split
/// out so it is unit-testable without a network round-trip.
///
/// Uses [`version::pick_best`] for the stable-before-prerelease preference
/// step: among all versions satisfying the request, a stable release is
/// preferred over a prerelease of the same or a higher version.
pub fn select(
	request: &VersionRequest,
	candidates: &[PackageVersion],
) -> Result<PackageVersion, ResolveError> {
	let no_match = || ResolveError::NoMatchLatest {
		name: String::from("<candidate set>"),
	};

	let satisfying: Vec<&PackageVersion> = match request {
		VersionRequest::Latest => candidates.iter().collect(),
		VersionRequest::Exact(pin) => {
			let pin_s = pin.canonical();
			return candidates.iter().find(|candidate| *candidate == pin).cloned().ok_or(ResolveError::NoMatchExact { name: String::from("<candidate set>"), pin: pin_s });
		}
		VersionRequest::Constraint(constraint) => {
			// A raw ecosystem range whose spec doesn't parse is a *malformed
			// request*, distinct from "parsed but nothing matched": pre-validate
			// so that error survives the fold into the constraint case.
			if let RangeConstraint::Range { ecosystem, spec } = constraint {
				match ecosystem {
					Language::Rust | Language::Typescript | Language::Nix => {
						semver::VersionReq::parse(spec)
							.map_err(|_| ResolveError::MalformedRequest { spec: spec.clone() })?;
					}
					Language::Python => {
						spec.parse::<uv_pep440::VersionSpecifiers>()
							.map_err(|_| ResolveError::MalformedRequest { spec: spec.clone() })?;
					}
					Language::CSharp => {
						if !nuget::spec_is_valid(spec) {
							return Err(ResolveError::MalformedRequest { spec: spec.clone() });
						}
					}
					Language::Go | Language::Java => {}
				}
			}
			candidates.iter().filter(|candidate| constraint.matches(candidate)).collect()
		}
	};

	// Build (ord_key, &PackageVersion) pairs so pick_best can apply the
	// stable-before-prerelease loop. OrdKey wraps a PackageVersion reference
	// and implements Ord via grammar_order (ecosystem-aware comparison).
	let keyed: Vec<(OrdKey<'_>, &PackageVersion)> = satisfying
		.iter()
		.map(|pv| (OrdKey(pv), *pv))
		.collect();

	let best = pick_best(&keyed, |key| package_version_is_prerelease(key.0)).copied().cloned();
	match request {
		VersionRequest::Constraint(RangeConstraint::Semver(range)) => {
			best.ok_or_else(|| ResolveError::NoMatchSemver { name: String::from("<candidate set>"), range: range.to_string() })
		}
		VersionRequest::Constraint(RangeConstraint::Range { spec, .. }) => {
			best.ok_or_else(|| ResolveError::NoMatchRange { name: String::from("<candidate set>"), spec: spec.clone() })
		}
		_ => best.ok_or_else(no_match),
	}
}

// ---------------------------------------------------------------------------
// Helpers for the pick_best integration
// ---------------------------------------------------------------------------

/// Newtype that adapts `&PackageVersion` to `Ord` via [`grammar_order`].
///
/// A candidate set always contains versions from a single ecosystem (callers
/// resolve one package from one registry), so `grammar_order` never produces
/// a mixed-ecosystem `Equal` in practice.
#[derive(PartialEq, Eq)]
struct OrdKey<'a>(&'a PackageVersion);

impl Ord for OrdKey<'_> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		grammar_order(&self.0, &other.0)
	}
}

impl PartialOrd for OrdKey<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

/// Whether a `PackageVersion` represents a prerelease under its grammar.
fn package_version_is_prerelease(v: &PackageVersion) -> bool {
	match v {
		PackageVersion::Cargo(sv) | PackageVersion::Npm(sv) | PackageVersion::Nix(sv) => {
			!sv.pre.is_empty()
		}
		PackageVersion::Python(pv) => pv.any_prerelease(),
		// Go and Java versions are resolved via git tags (not this registry path),
		// but classify conservatively: any version containing a '-' or alphabetic
		// character after the numeric part is treated as a prerelease.
		PackageVersion::Go(s) | PackageVersion::Java(s) => {
			s.contains('-') || s.chars().any(|c| c.is_ascii_alphabetic())
		}
		// NuGet prerelease: a `-label` suffix on the numeric core.
		PackageVersion::CSharp(s) => nuget::NuGetVersion::parse(s)
			.map(|v| v.is_prerelease())
			.unwrap_or_else(|| s.contains('-')),
	}
}

/// Whether a SemVer range admits a candidate (only meaningful for the SemVer
/// ecosystems; PEP 440 candidates never satisfy a SemVer range).
fn semver_matches(range: &semver::VersionReq, candidate: &PackageVersion) -> bool {
	match candidate {
		PackageVersion::Cargo(version)
		| PackageVersion::Npm(version)
		| PackageVersion::Nix(version) => range.matches(version),
		PackageVersion::Python(_) => false,
		// Go/Java via git tags; NuGet via interval notation, not semver range.
		PackageVersion::Go(_) | PackageVersion::Java(_) | PackageVersion::CSharp(_) => false,

	}
}

/// Order two candidates under their own grammar. Mixed grammars have no order
/// and compare equal, so `max_by` keeps the earlier of an (invalid) mixed set.
fn grammar_order(a: &&PackageVersion, b: &&PackageVersion) -> std::cmp::Ordering {
	match (a, b) {
		(PackageVersion::Cargo(x), PackageVersion::Cargo(y))
		| (PackageVersion::Npm(x), PackageVersion::Npm(y)) => x.cmp(y),
		(PackageVersion::Python(x), PackageVersion::Python(y)) => x.cmp(y),
		(PackageVersion::Go(x), PackageVersion::Go(y))
		| (PackageVersion::Java(x), PackageVersion::Java(y)) => x.cmp(y),
		(PackageVersion::CSharp(x), PackageVersion::CSharp(y)) => {
			match (nuget::NuGetVersion::parse(x), nuget::NuGetVersion::parse(y)) {
				(Some(a), Some(b)) => a.cmp(&b),
				_ => x.cmp(y),
			}
		}
		(PackageVersion::Nix(x), PackageVersion::Nix(y)) => x.cmp(y),
		_ => std::cmp::Ordering::Equal, // mixed not happen
	}
}


/// Fetch the published version set for `name` from `origin`, parsed under the
/// ecosystem's grammar (unparseable strays are skipped with a debug log, not
/// fatal). `// registry version-list fetch is network I/O`.
async fn published_versions(
	origin: &RegistryOrigin,
	name: &PackageName,
) -> Result<Vec<PackageVersion>, ResolveError> {
	let ecosystem = name.ecosystem();
	let url = versions_url(origin, name);
	let response = reqwest::get(&url).await.map_err(ResolveError::Lookup)?;
	if response.status() == reqwest::StatusCode::NOT_FOUND {
		return Err(ResolveError::NotFound { name: name.original().to_owned() });
	}
	let body: serde_json::Value = response
		.error_for_status()
		.map_err(ResolveError::Lookup)?
		.json()
		.await
		.map_err(ResolveError::Lookup)?;

	Ok(raw_versions(ecosystem, &body)
		.into_iter()
		.filter_map(|raw| {
			PackageVersion::try_from((ecosystem, raw.as_str()))
				.inspect_err(|error| {
					tracing::debug!(%raw, %error, "skipping unparseable published version");
				})
				.ok()
		})
		.collect())
}

/// The version-list endpoint for an origin, following each public registry's
/// documented API shape; a custom origin is assumed to mirror its ecosystem's
/// public shape under its own base URL.
fn versions_url(origin: &RegistryOrigin, name: &PackageName) -> String {
	let base = match origin {
		RegistryOrigin::CratesIo => String::from("https://crates.io"),
		RegistryOrigin::NpmPublic => String::from("https://registry.npmjs.org"),
		RegistryOrigin::PyPi => String::from("https://pypi.org"),
		RegistryOrigin::FlakeHub => String::from("https://api.flakehub.com"),
		RegistryOrigin::NuGet => String::from("https://api.nuget.org"),
		RegistryOrigin::Custom { url, .. } => url.as_str().trim_end_matches('/').to_owned(),
	};
	match name.ecosystem() {
		Language::Rust => format!("{base}/api/v1/crates/{}", name.canonical()),
		Language::Typescript => format!("{base}/{}", name.canonical()),
		Language::Python => format!("{base}/pypi/{}/json", name.canonical()),
		Language::Go => format!("{base}/{}", name.canonical()),
		Language::Java => format!("{base}/{}", name.canonical()),
		// NuGet flat-container version index (ids lowercased — `canonical`
		// already folds case).
		Language::CSharp => {
			format!("{base}/v3-flatcontainer/{}/index.json", name.canonical())
		}
		// FlakeHub resolution has bespoke semantics handled by the Nix
		// producer's `traversal` module, not this generic registry path.
		Language::Nix => format!("{base}/f/{}/releases", name.canonical()),
	}
}

/// Project the raw version strings out of each registry's response shape,
/// dropping yanked crates.io versions (npm/PyPI listings are already live-only
/// at these endpoints).
fn raw_versions(ecosystem: Language, body: &serde_json::Value) -> Vec<String> {
	match ecosystem {
		Language::Rust => body["versions"]
			.as_array()
			.into_iter()
			.flatten()
			.filter(|version| !version["yanked"].as_bool().unwrap_or(false))
			.filter_map(|version| version["num"].as_str().map(str::to_owned))
			.collect(),
		Language::Typescript => body["versions"]
			.as_object()
			.into_iter()
			.flat_map(|versions| versions.keys().cloned())
			.collect(),
		Language::Python => body["releases"]
			.as_object()
			.into_iter()
			.flat_map(|releases| releases.keys().cloned())
			.collect(),
		// NuGet flat-container `index.json`: `{"versions":[...]}`. Includes
		// unlisted versions (registration `listed` flag cross-checks elsewhere).
		Language::CSharp => body["versions"]
			.as_array()
			.into_iter()
			.flatten()
			.filter_map(|version| version.as_str().map(str::to_owned))
			.collect(),
		Language::Go | Language::Java => vec![], // not via this registry resolve path yet
		Language::Nix => vec![],                 // resolved via FlakeHub in the Nix producer
	}
}

// ---------------------------------------------------------------------------
// NuGet version grammar + interval-notation range matching
// ---------------------------------------------------------------------------

/// NuGet versioning: SemVer2 with an optional legacy 4th numeric part, plus
/// interval-notation version ranges (`[1.0,2.0)`). NuGet versions are *not*
/// SemVer (`1.0.0.5` is legal), so `semver` cannot be used — this is a faithful
/// subset of NuGet's own `NuGetVersion` / `VersionRange` semantics.
pub mod nuget {
	use std::cmp::Ordering;

	/// A parsed NuGet version: up to four numeric parts, an optional
	/// dot-separated prerelease, and (dropped) build metadata.
	#[derive(Debug, Clone, PartialEq, Eq)]
	pub struct NuGetVersion {
		parts: [u64; 4],
		pre: Vec<String>,
	}

	impl NuGetVersion {
		/// Parse a NuGet version string, or `None` if it isn't numeric-led.
		pub fn parse(text: &str) -> Option<Self> {
			let text = text.trim();
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
		pub fn is_prerelease(&self) -> bool {
			!self.pre.is_empty()
		}
	}

	impl Ord for NuGetVersion {
		fn cmp(&self, other: &Self) -> Ordering {
			match self.parts.cmp(&other.parts) {
				Ordering::Equal => {}
				ord => return ord,
			}
			match (self.pre.is_empty(), other.pre.is_empty()) {
				(true, true) => Ordering::Equal,
				(true, false) => Ordering::Greater,
				(false, true) => Ordering::Less,
				(false, false) => cmp_pre(&self.pre, &other.pre),
			}
		}
	}

	impl PartialOrd for NuGetVersion {
		fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
			Some(self.cmp(other))
		}
	}

	fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
		for i in 0..a.len().max(b.len()) {
			let ord = match (a.get(i), b.get(i)) {
				(Some(x), Some(y)) => cmp_pre_seg(x, y),
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

	fn cmp_pre_seg(x: &str, y: &str) -> Ordering {
		match (x.parse::<u64>(), y.parse::<u64>()) {
			(Ok(nx), Ok(ny)) => nx.cmp(&ny),
			(Ok(_), Err(_)) => Ordering::Less,
			(Err(_), Ok(_)) => Ordering::Greater,
			(Err(_), Err(_)) => x.cmp(y),
		}
	}

	struct Bound {
		version: Option<NuGetVersion>,
		inclusive: bool,
	}

	struct Range {
		lower: Bound,
		upper: Bound,
	}

	/// Parse a NuGet version-range spec (interval notation or a bare version =
	/// "≥ minimum", NOT exact — the documented rule).
	fn parse_range(spec: &str) -> Option<Range> {
		let spec = spec.trim();
		if spec.is_empty() {
			return None;
		}
		let first = spec.chars().next().unwrap();
		let last = spec.chars().last().unwrap();
		let is_interval = matches!(first, '[' | '(') && matches!(last, ']' | ')');
		if !is_interval {
			let v = NuGetVersion::parse(spec)?;
			return Some(Range {
				lower: Bound { version: Some(v), inclusive: true },
				upper: Bound { version: None, inclusive: false },
			});
		}
		let inner = &spec[1..spec.len() - 1];
		let (lo_str, hi_str) = match inner.split_once(',') {
			Some((lo, hi)) => (lo.trim(), hi.trim()),
			None => {
				let v = NuGetVersion::parse(inner.trim())?;
				return Some(Range {
					lower: Bound { version: Some(v.clone()), inclusive: true },
					upper: Bound { version: Some(v), inclusive: true },
				});
			}
		};
		let lower = Bound {
			version: if lo_str.is_empty() { None } else { Some(NuGetVersion::parse(lo_str)?) },
			inclusive: first == '[',
		};
		let upper = Bound {
			version: if hi_str.is_empty() { None } else { Some(NuGetVersion::parse(hi_str)?) },
			inclusive: last == ']',
		};
		Some(Range { lower, upper })
	}

	/// Whether `spec` is a well-formed NuGet range.
	pub fn spec_is_valid(spec: &str) -> bool {
		parse_range(spec).is_some()
	}

	/// Whether `candidate` (a raw NuGet version string) satisfies `spec`.
	pub fn range_matches(spec: &str, candidate: &str) -> bool {
		let Some(range) = parse_range(spec) else {
			return false;
		};
		let Some(v) = NuGetVersion::parse(candidate) else {
			return false;
		};
		if let Some(lo) = &range.lower.version {
			match v.cmp(lo) {
				Ordering::Less => return false,
				Ordering::Equal if !range.lower.inclusive => return false,
				_ => {}
			}
		}
		if let Some(hi) = &range.upper.version {
			match v.cmp(hi) {
				Ordering::Greater => return false,
				Ordering::Equal if !range.upper.inclusive => return false,
				_ => {}
			}
		}
		true
	}
}
