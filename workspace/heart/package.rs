//! Package identity — the most consequential types in the system.
//!
//! Every downstream store keys on the identity minted here, and identity bugs
//! are *permanent* (they corrupt the global index irreversibly), so this module
//! is deliberately strict:
//!
//! - [`PackageName`] normalizes per ecosystem (PEP 503 for Python, scope-aware
//!   for npm, `-`/`_` folding for crates) so `Requests`, `requests`, and
//!   `requests.` are provably one package.
//! - [`PackageVersion`] carries an ecosystem-appropriate *typed* version —
//!   SemVer for Rust/npm, PEP 440 for Python — because `semver::Version` cannot
//!   represent `1!2.0.post1.dev3`.
//! - [`RegistryOrigin`] records *which* registry a package came from, so a
//!   private mirror and the public index are distinct identities.
//! - [`PackageCoordinates`] is the full addressing tuple; [`PackageId`] is its
//!   deterministic UUIDv5 fingerprint.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;
use uuid::Uuid;

use crate::{ecosystem::Ecosystem, identifier::Id};

/// The DNS-namespace UUIDs deterministic ids are derived under. Distinct
/// namespaces per identity kind so a package id and a symbol id can never
/// collide even on identical input bytes.
pub mod namespace {
	use uuid::Uuid;

	/// Namespace for [`super::PackageId`].
	pub const PACKAGE: Uuid = Uuid::from_u128(0x6e75_646f_785f_706b_675f_6e73_0000_0001);
	/// Namespace for [`super::GlobalSymbolId`].
	pub const SYMBOL: Uuid = Uuid::from_u128(0x6e75_646f_785f_7379_6d5f_6e73_0000_0002);
}

/// Why a raw package name was rejected.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
	/// The name was empty (after trimming/normalization).
	#[error("package name is empty")]
	Empty,

	/// The name contained characters illegal for its ecosystem.
	#[error("package name {raw:?} is invalid for {ecosystem}")]
	Invalid { ecosystem: Ecosystem, raw: String },

	/// The name exceeded the ecosystem's length limit.
	#[error("package name {raw:?} exceeds the {ecosystem} length limit")]
	TooLong { ecosystem: Ecosystem, raw: String },
}

/// A validated, ecosystem-normalized package name.
///
/// Holds both the canonical form (used for identity/equality) and the original
/// as published (for display). Two `PackageName`s are equal iff their canonical
/// forms and ecosystems match — the whole point of normalizing at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageName {
	ecosystem: Ecosystem,
	canonical: SmolStr,
	original: SmolStr,
}

impl PackageName {
	/// Validate and normalize a raw name for its ecosystem.
	///
	/// - **Python**: PEP 503 — lowercase, runs of `-`/`_`/`.` collapse to `-`.
	/// - **Typescript/npm**: preserve `@scope/name`, lowercase, validate charset.
	/// - **Rust/crates**: fold `_`→`-` for comparison, validate charset.
	pub fn new(ecosystem: Ecosystem, raw: impl Into<String>) -> Result<Self, NameError> {
		let _ = (ecosystem, raw);
		todo!("normalize per ecosystem, validate charset + length, retain original")
	}

	/// The canonical, comparison/identity form.
	pub fn canonical(&self) -> &str { &self.canonical }

	/// The name exactly as published, for display.
	pub fn original(&self) -> &str { &self.original }

	/// The ecosystem this name belongs to.
	pub const fn ecosystem(&self) -> Ecosystem { self.ecosystem }
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
///
/// SemVer covers crates.io and npm; PEP 440 (epochs, post/dev releases, local
/// segments) covers Python and is *not* representable as SemVer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageVersion {
	/// crates.io SemVer.
	Cargo(semver::Version),
	/// npm SemVer.
	Npm(semver::Version),
	/// Python PEP 440.
	Python(uv_pep440::Version),
}

impl PackageVersion {
	/// Parse a raw version string under the grammar of the given ecosystem.
	pub fn parse(ecosystem: Ecosystem, raw: &str) -> Result<Self, VersionError> {
		let _ = (ecosystem, raw);
		todo!("dispatch to semver::Version::parse or uv_pep440::Version::from_str")
	}

	/// The ecosystem this version's grammar belongs to.
	pub const fn ecosystem(&self) -> Ecosystem {
		match self {
			PackageVersion::Cargo(_) => Ecosystem::Rust,
			PackageVersion::Npm(_) => Ecosystem::Typescript,
			PackageVersion::Python(_) => Ecosystem::Python,
		}
	}

	/// The canonical, normalized string form — stable enough to feed into an id.
	pub fn canonical(&self) -> String { todo!("render each grammar's normalized form") }
}

/// Why a raw version failed to parse.
#[derive(Debug, Error)]
#[error("invalid {ecosystem} version {raw:?}: {message}")]
pub struct VersionError {
	pub ecosystem: Ecosystem,
	pub raw: String,
	pub message: String,
}

/// Which registry a package was sourced from. A package with the same
/// name+version from two origins is two distinct identities — this is the
/// federation/multi-source dimension the access layer promises.
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
	/// A stable token identifying this origin, folded into ids and paths.
	pub fn token(&self) -> SmolStr { todo!("stable per-origin token, incl. custom name+host") }
}

/// The full, unambiguous address of a package: origin × name × version.
///
/// This tuple is what [`PackageId`] fingerprints and what the object store
/// derives its key from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageCoordinates {
	pub origin: RegistryOrigin,
	pub name: PackageName,
	pub version: PackageVersion,
}

impl PackageCoordinates {
	/// Assemble coordinates, checking name and version agree on ecosystem.
	pub fn new(
		origin: RegistryOrigin,
		name: PackageName,
		version: PackageVersion,
	) -> Result<Self, CoordinateError> {
		let _ = (origin, name, version);
		todo!("assert name.ecosystem() == version.ecosystem(); reject otherwise")
	}

	/// The ecosystem these coordinates live in.
	pub const fn ecosystem(&self) -> Ecosystem { self.name.ecosystem() }

	/// The canonical byte-encoding fed to the id hasher — the single source of
	/// truth for what "the same package" means. Deterministic and versioned.
	pub fn identity_bytes(&self) -> Vec<u8> {
		todo!("origin.token \0 name.canonical \0 version.canonical, length-prefixed")
	}

	/// The deterministic global id for this package.
	pub fn id(&self) -> PackageId {
		PackageId(Id::from_name(&namespace::PACKAGE, &self.identity_bytes()))
	}
}

/// Raised when name and version disagree on ecosystem.
#[derive(Debug, Error)]
#[error("coordinate ecosystem mismatch: name is {name}, version is {version}")]
pub struct CoordinateError {
	pub name: Ecosystem,
	pub version: Ecosystem,
}

/// The stable, deterministic identity of a package across the whole system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PackageId(Id<PackageCoordinates>);

impl PackageId {
	/// The underlying tagged id.
	pub const fn as_id(&self) -> Id<PackageCoordinates> { self.0 }

	/// The raw UUID, for storage keys.
	pub const fn as_uuid(&self) -> &Uuid { self.0.as_uuid() }

	/// Reconstruct from a raw UUID read back from storage. (The forward
	/// direction is [`PackageCoordinates::id`]; this is the inverse used when
	/// hydrating rows.)
	pub const fn from_uuid(uuid: Uuid) -> Self { Self(Id::from_uuid(uuid)) }
}

/// A symbol's path *within* a package — the ecosystem-relative locator that,
/// combined with the graph instance, yields a [`GlobalSymbolId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EntryUri {
	/// The package the symbol lives in.
	pub package: PackageId,
	/// The `::`/`.`-agnostic path segments to the symbol within the package.
	pub path: Box<[SmolStr]>,
}

impl EntryUri {
	/// The canonical string form of this URI.
	pub fn canonical(&self) -> String { todo!("join package id + path segments canonically") }
}

/// A globally-unique symbol identity, deterministic per graph instance so it is
/// offline-recomputable. Salted with the TerminusDB instance so two instances
/// of the same corpus don't share ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GlobalSymbolId(Id<EntryUri>);

impl GlobalSymbolId {
	/// Derive the deterministic id from the instance token and the entry URI.
	pub fn derive(instance_token: &str, uri: &EntryUri) -> Self {
		let _ = (instance_token, uri);
		todo!("v5 over instance-token NUL uri-canonical under namespace::SYMBOL")
	}

	/// The raw UUID, for storage.
	pub fn as_uuid(&self) -> &Uuid { self.0.as_uuid() }

	/// Reconstruct from a raw UUID read back from storage. (The forward
	/// direction is [`GlobalSymbolId::derive`]; this is the inverse used when
	/// hydrating rows.)
	pub const fn from_uuid(uuid: Uuid) -> Self { Self(Id::from_uuid(uuid)) }
}
