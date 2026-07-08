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

/// A version request against a package: an exact pin or a range, in the
/// ecosystem's own grammar.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VersionRequest {
	/// The single latest published version.
	Latest,

	/// An exact version pin.
	Exact(PackageVersion),

	/// A SemVer range (crates.io / npm), e.g. `^1.2`, `>=2,<3`.
	SemverRange(semver::VersionReq),

	/// A raw ecosystem range string to be parsed under the ecosystem's grammar
	/// (covers PEP 440 specifiers SemVer cannot represent).
	Range { ecosystem: Language, spec: String },
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
		ResolveError::NoMatchExact { request: pin, .. } => ResolveError::NoMatchExact { name: name.original().to_owned(), pin },
		ResolveError::NoMatchSemver { range, .. } => ResolveError::NoMatchSemver { name: name.original().to_owned(), range },
		ResolveError::NoMatchRange { spec, .. } => ResolveError::NoMatchRange { name: name.original().to_owned(), spec },
		other => other,
	})?;
	Ok(PackageCoordinates { origin: source.origin.clone(), name: name.clone(), version })
}

/// Select the greatest version from a candidate set satisfying `request`. Split
/// out so it is unit-testable without a network round-trip.
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
		VersionRequest::SemverRange(range) => {
			let r = range.to_string();
			let _ = r; // used only on error path below if empty
			candidates.iter().filter(|candidate| semver_matches(range, candidate)).collect()
		}
		VersionRequest::Range { ecosystem, spec } => match ecosystem {
			Language::Rust | Language::Typescript => {
				let range = semver::VersionReq::parse(spec)
					.map_err(|_| ResolveError::MalformedRequest { spec: spec.clone() })?;
				candidates.iter().filter(|candidate| semver_matches(&range, candidate)).collect()
			}
			Language::Python => {
				let specifiers: uv_pep440::VersionSpecifiers =
					spec.parse().map_err(|_| ResolveError::MalformedRequest { spec: spec.clone() })?;
				candidates
					.iter()
					.filter(|candidate| match candidate {
						PackageVersion::Python(version) => specifiers.contains(version),
		PackageVersion::Go(_) | PackageVersion::Java(_) => false, // version resolution via tags for Go/Java

						_ => false,
					})
					.collect()
			}
		},
	};

	let best = satisfying.into_iter().max_by(|a, b| grammar_order(a, b)).cloned();
	match request {
		VersionRequest::SemverRange(range) => best.ok_or_else(|| ResolveError::NoMatchSemver { name: String::from("<candidate set>"), range: range.to_string() }),
		VersionRequest::Range { spec, .. } => best.ok_or_else(|| ResolveError::NoMatchRange { name: String::from("<candidate set>"), spec: spec.clone() }),
		_ => best.ok_or_else(no_match),
	}
}

/// Whether a SemVer range admits a candidate (only meaningful for the SemVer
/// ecosystems; PEP 440 candidates never satisfy a SemVer range).
fn semver_matches(range: &semver::VersionReq, candidate: &PackageVersion) -> bool {
	match candidate {
		PackageVersion::Cargo(version) | PackageVersion::Npm(version) => range.matches(version),
		PackageVersion::Python(_) => false,
		PackageVersion::Go(_) | PackageVersion::Java(_) => false, // not via semver range here

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
		_ => std::cmp::Ordering::Equal, // mixed not happen

		_ => std::cmp::Ordering::Equal,
	}
}

/// The human form of a request, for error messages.
fn request_display(request: &VersionRequest) -> String {
	match request {
		VersionRequest::Latest => String::from("latest"),
		VersionRequest::Exact(version) => version.canonical(),
		VersionRequest::SemverRange(range) => range.to_string(),
		VersionRequest::Range { spec, .. } => spec.clone(),
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
		RegistryOrigin::Custom { url, .. } => url.as_str().trim_end_matches('/').to_owned(),
	};
	match name.ecosystem() {
		Language::Rust => format!("{base}/api/v1/crates/{}", name.canonical()),
		Language::Typescript => format!("{base}/{}", name.canonical()),
		Language::Python => format!("{base}/pypi/{}/json", name.canonical()),
		Language::Go => format!("{base}/{}", name.canonical()),
		Language::Java => format!("{base}/{}", name.canonical()),
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
		Language::Go | Language::Java => vec![], // not via this registry resolve path yet
	}
}
