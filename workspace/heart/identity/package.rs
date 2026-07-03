//! Package identity: the coordinates that name a package and the deterministic
//! [`PackageId`] derived from them.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use super::{Id, namespace};
use crate::ecosystem::Language;

/// Identity tag for packages. Zero-variant: it exists only to brand [`Id`], never
/// to be constructed. (`heart` has no `Package` *record*; the rich package types
/// live in the registry crate.)
pub enum Package {}

/// The stable, deterministic identity of a package across the whole system —
/// simply an [`Id`] branded with [`Package`]. Derive one with
/// [`PackageCoordinates::id`].
pub type PackageId = Id<Package>;

/// Why a raw package name was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
	/// The name was empty (after trimming/normalization).
	#[error("package name is empty")]
	Empty,

	/// The name contained characters illegal for its ecosystem.
	#[error("package name {raw:?} is invalid for {ecosystem}")]
	Invalid { ecosystem: Language, raw: String },

	/// The name exceeded the ecosystem's length limit.
	#[error("package name {raw:?} exceeds the {ecosystem} length limit")]
	TooLong { ecosystem: Language, raw: String },
}

/// A validated, ecosystem-normalized package name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageName {
	/// The ecosystem this name belongs to (part of its identity).
	ecosystem: Language,

	/// Our sanitized name.
	canonical: SmolStr,

	/// Their display name.
	original: SmolStr,
}

impl PackageName {
	/// Validate and normalize a raw name for its ecosystem.
	///
	/// - **Python**: PEP 503 — lowercase, runs of `-`/`_`/`.` collapse to `-`.
	/// - **Typescript/npm**: preserve `@scope/name`, lowercase, validate charset.
	/// - **Rust/crates**: fold `_`→`-` for comparison, validate charset.
	pub fn new(ecosystem: Language, raw: impl Into<String>) -> Result<Self, NameError> {
		let _ = (ecosystem, raw);
		todo!("normalize per ecosystem, validate charset + length, retain original")
	}

	/// The canonical, comparison/identity form.
	pub fn canonical(&self) -> &str { &self.canonical }

	/// The name exactly as published, for display.
	pub fn original(&self) -> &str { &self.original }

	/// The ecosystem this name belongs to.
	pub const fn ecosystem(&self) -> Language { self.ecosystem }
}

impl PartialEq for PackageName {
	fn eq(&self, other: &Self) -> bool {
		self.ecosystem == other.ecosystem && self.canonical == other.canonical
	}
}
impl Eq for PackageName {}
impl std::hash::Hash for PackageName {
	fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
		self.ecosystem.hash(state);
		self.canonical.hash(state);
	}
}

/// An ecosystem-appropriate, typed package version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageVersion {
	/// crates.io SemVer.
	Cargo(semver::Version),

	/// npm SemVer.
	Npm(semver::Version),

	/// Python PEP 440.
	Python(uv_pep440::Version),
}

/// Why a raw version failed to parse.
#[derive(Debug, Error)]
#[error("invalid {ecosystem} version {raw:?}: {message}")]
pub struct VersionError {
	pub ecosystem: Language,
	pub raw:       String,
	pub message:   String,
}

impl TryFrom<(Language, &str)> for PackageVersion {
	type Error = VersionError;

	/// Parse a raw version string under the grammar of the given ecosystem.
	fn try_from((ecosystem, raw): (Language, &str)) -> Result<Self, Self::Error> {
		let _ = (ecosystem, raw);
		todo!("dispatch to semver::Version::parse or uv_pep440::Version::from_str")
	}
}

impl PackageVersion {
	/// The canonical string form under this version's grammar — the exact bytes
	/// folded into the package identity and persisted to storage.
	pub fn canonical(&self) -> String {
		match self {
			PackageVersion::Cargo(v) | PackageVersion::Npm(v) => v.to_string(),
			PackageVersion::Python(v) => v.to_string(),
		}
	}
}

impl From<&PackageVersion> for Language {
	/// The ecosystem this version's grammar belongs to.
	fn from(package_version: &PackageVersion) -> Self {
		match package_version {
			PackageVersion::Cargo(_) => Language::Rust,
			PackageVersion::Npm(_) => Language::Typescript,
			PackageVersion::Python(_) => Language::Python,
		}
	}
}

/// Which registry a package was sourced from. A package with the same
/// name+version from two origins is two UNIQUE identities.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RegistryOrigin {
	/// The canonical public crates.io.
	CratesIo,

	/// The canonical public npm registry.
	NpmPublic,

	/// The canonical public PyPI.
	PyPi,

	/// A self-hosted / private registry, identified by a stable name + base URL.
	Custom { name: SmolStr, url: url::Url },
}

impl RegistryOrigin {
	/// The stable token folded into the package identity and persisted alongside
	/// it. Public registries have canonical tokens; a custom registry uses its
	/// configured name.
	pub fn token(&self) -> Cow<'static, str> {
		match self {
			RegistryOrigin::CratesIo => Cow::Borrowed("crates.io"),
			RegistryOrigin::NpmPublic => Cow::Borrowed("npm"),
			RegistryOrigin::PyPi => Cow::Borrowed("pypi"),
			RegistryOrigin::Custom { name, .. } => Cow::Owned(name.to_string()),
		}
	}
}

/// The address of a package.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageCoordinates {
	/// Where the package comes from.
	pub origin: RegistryOrigin,

	/// What the package is named.
	pub name: PackageName,

	/// Which version is it at?
	pub version: PackageVersion,
}

impl PackageCoordinates {
	/// The ecosystem this package belongs to (carried by its validated name).
	pub const fn ecosystem(&self) -> Language { self.name.ecosystem() }

	/// The canonical byte-encoding fed to the id hasher — the single source of
	/// truth for what "the same package" means. Deterministic and versioned.
	pub fn identity_bytes(&self) -> Vec<u8> {
		todo!("origin.token \0 name.canonical \0 version.canonical, length-prefixed")
	}

	/// The deterministic global id for this package — the one construction site.
	pub fn id(&self) -> PackageId { Id::from_name(&namespace::PACKAGE, &self.identity_bytes()) }
}

/// Raised when name and version disagree on ecosystem.
#[derive(Debug, Error)]
#[error("coordinate ecosystem mismatch: name is {name}, version is {version}")]
pub struct CoordinateError {
	pub name:    Language,
	pub version: Language,
}
