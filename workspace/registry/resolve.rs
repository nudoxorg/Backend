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
	ecosystem::Language,
	identity::{
		PackageCoordinates, PackageName, PackageVersion, RegistryOrigin,
	},
};

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
pub async fn resolve(
	source: &ResolveSource,
	name: &PackageName,
	request: &VersionRequest,
) -> Result<PackageCoordinates, ResolveError> {
	let _ = (source, name, request);
	todo!("fetch published versions, filter by request under grammar, pick max, build coordinates")
}

/// Select the greatest version from a candidate set satisfying `request`. Split
/// out so it is unit-testable without a network round-trip.
pub fn select(
	request: &VersionRequest,
	candidates: &[PackageVersion],
) -> Result<PackageVersion, ResolveError> {
	let _ = (request, candidates);
	todo!("apply request under the ecosystem grammar, return the max satisfying candidate")
}
