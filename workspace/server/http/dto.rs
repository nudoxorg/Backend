//! Request DTOs — the serialized shapes the API *accepts*, kept separate from the
//! internal domain types only where the wire genuinely diverges. Responses, by
//! contrast, serialize the domain types directly: a search page is a
//! [`heart::Page<Symbol>`] (a hit is `heart::Scored<Symbol>`, not a bespoke
//! `MatchDto`), and an add/ensure response is the domain
//! [`crate::coordination::initialization::Initialized`].
//!
//! Pagination is always by **opaque cursor token** (never offset): a page
//! response carries `next` only if more results exist, and the client echoes it
//! back verbatim (see [`heart::Cursor`]).

use heart::{Language, PackageCoordinates, PackageName, PackageVersion, RegistryOrigin};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

/// A search request as it arrives on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequestDto {
	/// The raw query text.
	pub query: String,
	/// Whether to treat the query as a natural-language / semantic query. The
	/// semantic path is still gated server-side; this is only a request.
	#[serde(default)]
	pub semantic: bool,
	/// Optional ecosystem scope.
	#[serde(default)]
	pub ecosystems: Vec<Language>,
	/// Optional package-name scope (any version), by canonical name.
	#[serde(default)]
	pub packages: Vec<String>,
	/// Page size.
	pub limit: std::num::NonZeroU32,
	/// Opaque cursor from a previous page, if continuing.
	#[serde(default)]
	pub cursor: Option<String>,
}

/// A request to add/index a package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddPackageDto {
	/// The ecosystem the package lives in.
	pub ecosystem: Language,
	/// The package name as published.
	pub name: String,
	/// The version requested (concrete or a range to resolve).
	pub version: String,
	/// The origin registry, if not the ecosystem default.
	#[serde(default)]
	pub origin: Option<String>,
}

impl AddPackageDto {
	/// Parse the wire request into typed [`PackageCoordinates`], validating the
	/// name and — crucially — the **version** into a typed [`PackageVersion`]
	/// rather than leaving it a bare string. Since the deterministic `PackageId`
	/// is derived from the full coordinates (version included), dropping the
	/// version here would mint the wrong id; this is the one place it is consumed.
	pub fn into_coordinates(&self) -> Result<PackageCoordinates, ServerError> {
		let name = PackageName::new(self.ecosystem, self.name.as_str())
			.map_err(|e| ServerError::BadRequest(e.to_string()))?;
		let version = PackageVersion::try_from((self.ecosystem, self.version.as_str()))
			.map_err(|e| ServerError::BadRequest(e.to_string()))?;
		let origin = match self.origin.as_deref() {
			None => default_origin(self.ecosystem),
			Some(custom) => resolve_custom_origin(custom)?,
		};
		Ok(PackageCoordinates { origin, name, version })
	}
}

/// The canonical public registry for an ecosystem.
fn default_origin(ecosystem: Language) -> RegistryOrigin {
	match ecosystem {
		Language::Rust => RegistryOrigin::CratesIo,
		Language::Typescript => RegistryOrigin::NpmPublic,
		Language::Python => RegistryOrigin::PyPi,
	}
}

/// Resolve a named custom registry into a [`RegistryOrigin`]. A custom origin
/// needs a base URL, which is looked up from server config by name.
fn resolve_custom_origin(name: &str) -> Result<RegistryOrigin, ServerError> {
	let _ = name;
	todo!("look up the configured custom registry base URL by name")
}

/// The health/readiness projection returned by the health endpoint — a flatter,
/// friendlier wire shape than the internal [`crate::coordination::health::Health`]
/// enum (hence a real projection, not a 1:1 mirror).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	/// Overall readiness.
	pub ready: bool,
	/// Backends currently degraded/unreachable, if any.
	pub degraded: Vec<heart::BackendKind>,
}
