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
use crate::package::{Coordinates as PackageCoordinates, PackageName};

use crate::error::ResolveError;

use ecosystem::{self, LanguageExt as _, upstream::ListingStatus};

// The shared version vocabulary: the request enum, its constraint predicate,
// and the pick_best selection primitive (folded into `heart::version`).
use heart::version::{Constraint, pick_best};

/// A version request against a package.
///
/// This is the shared [`heart::version::VersionRequest`] specialised to
/// [`PackageVersion`] with [`RangeConstraint`] as its constraint case — so a
/// SemVer range and a raw PEP 440 (or other ecosystem) range both fold into the
/// single `Constraint` arm rather than living as bespoke variants. `Latest` and
/// `Exact` keep their distinct selection semantics.
pub type VersionRequest = heart::version::VersionRequest<PackageVersion, RangeConstraint>;

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
			RangeConstraint::Range { ecosystem, spec } => {
				let raw = candidate.canonical();
				ecosystem.spec().range_matches(spec, &raw)
			}
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
/// The outcome of a successful resolve: the concrete coordinates plus the full
/// per-version listing snapshot observed at resolve time. The caller may
/// persist the snapshot via [`crate::index::Index::set_listing`].
pub struct ResolveOutput {
	pub coordinates: PackageCoordinates,
	/// All versions observed during this resolve, paired with their listing
	/// status. Callers that drive indexing should persist these.
	pub observed_listing: Vec<(PackageVersion, ListingStatus)>,
}

#[tracing::instrument(skip(client, source), fields(origin = %source.origin.token()))]
pub async fn resolve(
	client: &crate::upstream::UpstreamClient,
	source: &ResolveSource,
	name: &PackageName,
	request: &VersionRequest,
) -> Result<ResolveOutput, ResolveError> {
	let published_with_status = published_versions(client, &source.origin, name).await?;
	tracing::debug!(candidates = published_with_status.len(), "published version set fetched");

	// Exact pins may resolve withdrawn versions (allows dependency on yanked/unlisted
	// versions when the caller explicitly requests one). Latest/Constraint silently
	// skip withdrawn versions so they never enter automatic resolution.
	let published: Vec<PackageVersion> = match request {
		VersionRequest::Exact(_) => published_with_status.iter().map(|(pv, _)| pv.clone()).collect(),
		_ => published_with_status
			.iter()
			.filter(|(_, status)| status.is_listed())
			.map(|(pv, _)| pv.clone())
			.collect(),
	};

	let version = select(request, &published).map_err(|error| match error {
		// `select` works over a bare candidate set; re-attach the name here.
		ResolveError::NoMatchLatest { .. } => ResolveError::NoMatchLatest { name: name.original().to_owned() },
		ResolveError::NoMatchExact { pin, .. } => ResolveError::NoMatchExact { name: name.original().to_owned(), pin },
		ResolveError::NoMatchSemver { range, .. } => ResolveError::NoMatchSemver { name: name.original().to_owned(), range },
		ResolveError::NoMatchRange { spec, .. } => ResolveError::NoMatchRange { name: name.original().to_owned(), spec },
		other => other,
	})?;
	Ok(ResolveOutput {
		coordinates: PackageCoordinates { origin: source.origin.clone(), name: name.clone(), version },
		observed_listing: published_with_status,
	})
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
			if let RangeConstraint::Range { ecosystem, spec } = constraint
				&& !ecosystem.spec().spec_is_valid(spec) {
					return Err(ResolveError::MalformedRequest { spec: spec.clone() });
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
		grammar_order(self.0, other.0)
	}
}

impl PartialOrd for OrdKey<'_> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
		Some(self.cmp(other))
	}
}

/// Whether a `PackageVersion` represents a prerelease under its grammar.
/// Delegates to the ecosystem's version grammar via `DynSpec`.
fn package_version_is_prerelease(v: &PackageVersion) -> bool {
	let lang = Language::from(v);
	let raw = v.canonical();
	lang.spec().version_is_prerelease(&raw).unwrap_or(false)
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

/// Order two candidates under their own grammar via `DynSpec::compare_versions`.
/// Delegates to the ecosystem grammar; falls back to raw-string comparison for
/// unparseable strays so selection stays total.
fn grammar_order(a: &PackageVersion, b: &PackageVersion) -> std::cmp::Ordering {
	let lang_a = Language::from(a);
	let lang_b = Language::from(b);
	// Mixed-ecosystem pairs have no grammar order; treat as equal so the earlier
	// candidate is kept — a mixed set should not happen in practice.
	if lang_a != lang_b {
		return std::cmp::Ordering::Equal;
	}
	let raw_a = a.canonical();
	let raw_b = b.canonical();
	lang_a
		.spec()
		.compare_versions(&raw_a, &raw_b)
		// Fallback: both failed to parse — sort by raw string to stay total.
		.unwrap_or_else(|| raw_a.cmp(&raw_b))
}


/// Fetch the published version set for `name` from `origin`, parsed under the
/// ecosystem's grammar (unparseable strays are skipped with a debug log, not
/// fatal). `// registry version-list fetch is network I/O`.
async fn published_versions(
	client: &crate::upstream::UpstreamClient,
	origin: &RegistryOrigin,
	name: &PackageName,
) -> Result<Vec<(PackageVersion, ecosystem::upstream::ListingStatus)>, ResolveError> {
	use crate::upstream::UpstreamError;

	let ecosystem_lang = name.ecosystem();
	let spec = ecosystem_lang.spec();
	let url = versions_url(origin, name);
	let body_bytes = match client.get(ecosystem_lang, &url).await {
		Err(UpstreamError::NotFound) => {
			return Err(ResolveError::NotFound { name: name.original().to_owned() });
		}
		other => other?,
	};

	let mut listed = spec.parse_version_listing(&body_bytes);

	// Secondary listing-status fetch for ecosystems that need it (NuGet M3).
	// Best-effort: a failed status fetch leaves everything Listed rather than
	// failing the resolve.
	if let Some(status_template) = spec.endpoints().listing_status {
		let status_url = format!(
			"{}{}",
			versions_base_url(origin),
			status_template.replace("{name_lower}", name.canonical())
		);
		if let Ok(sb) = client.get(ecosystem_lang, &status_url).await {
			listed = spec.merge_listing_status(listed, &sb);
		}
	}

	Ok(listed
		.into_iter()
		.filter_map(|lv| {
			PackageVersion::try_from((ecosystem_lang, lv.raw.as_str()))
				.inspect_err(|error| {
					tracing::debug!(raw = %lv.raw, %error, "skipping unparseable published version");
				})
				.ok()
				.map(|pv| (pv, lv.status))
		})
		.collect())
}

/// Base URL for a registry origin (no trailing slash).
fn versions_base_url(origin: &RegistryOrigin) -> String {
	match origin {
		RegistryOrigin::CratesIo => String::from("https://crates.io"),
		RegistryOrigin::NpmPublic => String::from("https://registry.npmjs.org"),
		RegistryOrigin::PyPi => String::from("https://pypi.org"),
		RegistryOrigin::FlakeHub => String::from("https://api.flakehub.com"),
		RegistryOrigin::NuGet => String::from("https://api.nuget.org"),
		RegistryOrigin::GoProxy => String::from("https://proxy.golang.org"),
		RegistryOrigin::MavenCentral => String::from("https://repo1.maven.org/maven2"),
		RegistryOrigin::Custom { url, .. } => url.as_str().trim_end_matches('/').to_owned(),
	}
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
		RegistryOrigin::GoProxy => String::from("https://proxy.golang.org"),
		RegistryOrigin::MavenCentral => String::from("https://repo1.maven.org/maven2"),
		RegistryOrigin::Custom { url, .. } => url.as_str().trim_end_matches('/').to_owned(),
	};
	match name.ecosystem() {
		Language::Rust => format!("{base}/api/v1/crates/{}", name.canonical()),
		Language::Typescript => format!("{base}/{}", name.canonical()),
		Language::Python => format!("{base}/pypi/{}/json", name.canonical()),
		Language::Go => {
			let escaped = ecosystem::escape_module_path(name.canonical());
			format!("{base}/{escaped}/@v/list")
		}
		Language::Java => {
			// canonical is `group:artifact`; map to group_path/artifact/maven-metadata.xml
			let canon = name.canonical();
			if let Some((group, artifact)) = canon.split_once(':') {
				let group_path = group.replace('.', "/");
				format!("{base}/{group_path}/{artifact}/maven-metadata.xml")
			} else {
				format!("{base}/{canon}/maven-metadata.xml")
			}
		}
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;
	use heart::PackageVersion;

	fn cargo(s: &str) -> PackageVersion {
		PackageVersion::Cargo(semver::Version::parse(s).unwrap())
	}

	fn npm(s: &str) -> PackageVersion {
		PackageVersion::Npm(semver::Version::parse(s).unwrap())
	}

	fn python(s: &str) -> PackageVersion {
		PackageVersion::Python(s.parse().unwrap())
	}

	fn csharp(s: &str) -> PackageVersion {
		PackageVersion::CSharp(s.to_owned())
	}

	fn go_ver(s: &str) -> PackageVersion {
		PackageVersion::Go(s.to_owned())
	}

	fn java(s: &str) -> PackageVersion {
		PackageVersion::Java(s.to_owned())
	}

	// ── select(): exact pin ───────────────────────────────────────────────────

	#[test]
	fn select_exact_pin() {
		let candidates = vec![cargo("1.0.0"), cargo("2.0.0"), cargo("3.0.0")];
		let req = VersionRequest::Exact(cargo("2.0.0"));
		assert_eq!(select(&req, &candidates).unwrap(), cargo("2.0.0"));
	}

	#[test]
	fn select_exact_pin_missing() {
		let candidates = vec![cargo("1.0.0"), cargo("3.0.0")];
		let req = VersionRequest::Exact(cargo("2.0.0"));
		assert!(matches!(
			select(&req, &candidates),
			Err(ResolveError::NoMatchExact { .. })
		));
	}

	// ── select(): SemVer range ────────────────────────────────────────────────

	#[test]
	fn select_semver_range_picks_greatest() {
		let candidates = vec![cargo("1.0.0"), cargo("1.2.0"), cargo("1.5.0"), cargo("2.0.0")];
		let req = VersionRequest::Constraint(RangeConstraint::Semver(
			semver::VersionReq::parse(">=1.0,<2.0").unwrap(),
		));
		assert_eq!(select(&req, &candidates).unwrap(), cargo("1.5.0"));
	}

	#[test]
	fn select_semver_stable_preferred_over_prerelease() {
		let candidates = vec![cargo("1.0.0"), cargo("1.1.0-alpha.1")];
		let req = VersionRequest::Constraint(RangeConstraint::Semver(
			semver::VersionReq::parse(">=1.0").unwrap(),
		));
		// 1.1.0-alpha.1 is higher but pre-release; pick_best should prefer 1.0.0 stable.
		assert_eq!(select(&req, &candidates).unwrap(), cargo("1.0.0"));
	}

	// ── select(): PEP 440 range ───────────────────────────────────────────────

	#[test]
	fn select_pep440_range() {
		let candidates = vec![
			python("1.0.0"),
			python("1.5.0"),
			python("2.0.0"),
			python("2.1.0a1"),
		];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Python,
			spec: String::from(">=1.0,<2.0"),
		});
		assert_eq!(select(&req, &candidates).unwrap(), python("1.5.0"));
	}

	#[test]
	fn select_pep440_stable_preferred() {
		let candidates = vec![python("1.0.0"), python("1.1.0a1")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Python,
			spec: String::from(">=1.0"),
		});
		assert_eq!(select(&req, &candidates).unwrap(), python("1.0.0"));
	}

	// ── select(): NuGet interval notation ────────────────────────────────────

	#[test]
	fn select_nuget_interval() {
		let candidates = vec![csharp("1.0.0"), csharp("1.5.0"), csharp("2.0.0")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::CSharp,
			spec: String::from("[1.0,2.0)"),
		});
		assert_eq!(select(&req, &candidates).unwrap(), csharp("1.5.0"));
	}

	#[test]
	fn select_nuget_malformed_spec() {
		let candidates = vec![csharp("1.0.0")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::CSharp,
			spec: String::from("not-a-nuget-spec!!"),
		});
		assert!(matches!(
			select(&req, &candidates),
			Err(ResolveError::MalformedRequest { .. })
		));
	}

	#[test]
	fn select_nuget_stable_preferred() {
		let candidates = vec![csharp("1.0.0"), csharp("1.1.0-alpha")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::CSharp,
			spec: String::from("1.0.0"), // bare = minimum
		});
		assert_eq!(select(&req, &candidates).unwrap(), csharp("1.0.0"));
	}

	// ── select(): Go exact match ──────────────────────────────────────────────

	#[test]
	fn select_go_exact() {
		let candidates = vec![go_ver("v1.0.0"), go_ver("v1.1.0"), go_ver("v2.0.0")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Go,
			spec: String::from("v1.1.0"),
		});
		// Go range_matches = exact; only v1.1.0 qualifies.
		assert_eq!(select(&req, &candidates).unwrap(), go_ver("v1.1.0"));
	}

	#[test]
	fn select_go_no_match() {
		let candidates = vec![go_ver("v1.0.0"), go_ver("v2.0.0")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Go,
			spec: String::from("v1.5.0"),
		});
		assert!(matches!(
			select(&req, &candidates),
			Err(ResolveError::NoMatchRange { .. })
		));
	}

	// ── select(): Java bracket range ─────────────────────────────────────────

	#[test]
	fn select_java_bracket_range() {
		let candidates = vec![
			java("1.0"),
			java("1.5"),
			java("2.0"),
			java("2.1-SNAPSHOT"),
		];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Java,
			spec: String::from("[1.0,2.0)"),
		});
		assert_eq!(select(&req, &candidates).unwrap(), java("1.5"));
	}

	#[test]
	fn select_java_stable_before_prerelease() {
		let candidates = vec![java("1.0"), java("1.1-SNAPSHOT")];
		let req = VersionRequest::Constraint(RangeConstraint::Range {
			ecosystem: Language::Java,
			spec: String::from("[1.0,2.0)"),
		});
		assert_eq!(select(&req, &candidates).unwrap(), java("1.0"));
	}

	// ── grammar_order ─────────────────────────────────────────────────────────

	#[test]
	fn grammar_order_cargo() {
		assert_eq!(grammar_order(&cargo("2.0.0"), &cargo("1.0.0")), std::cmp::Ordering::Greater);
		assert_eq!(grammar_order(&cargo("1.0.0"), &cargo("1.0.0")), std::cmp::Ordering::Equal);
	}

	#[test]
	fn grammar_order_nuget_fallback_unparseable() {
		// Even if a CSharp version string cannot be parsed as NuGetVersion,
		// grammar_order falls back to raw string cmp (stays total).
		let a = PackageVersion::CSharp(String::from("garbage!!"));
		let b = PackageVersion::CSharp(String::from("zzz"));
		// Should not panic; result is just some ordering.
		let _ = grammar_order(&a, &b);
	}

	#[test]
	fn grammar_order_mixed_ecosystems_equal() {
		// Mixed is not expected in practice; should be Equal (not panic).
		assert_eq!(grammar_order(&cargo("1.0.0"), &npm("1.0.0")), std::cmp::Ordering::Equal);
	}
}
