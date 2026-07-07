//! Request DTOs — the serialized shapes the API *accepts* — and their lowering
//! into the typed domain vocabulary.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwap;
use heart::{Language, PackageVersion, RegistryOrigin};
use registry::package::{Coordinates as PackageCoordinates, PackageName};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strum::IntoEnumIterator;
use url::Url;

use crate::config::CustomRegistry;
use crate::error::ServerError;
use crate::search::query::{
	AbstractQuery, Filter, LiteralQuery, PackageSelector, Pagination, Query, Search,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequestDto {
	pub query: String,
	#[serde(default)]
	pub semantic: bool,
	#[serde(default)]
	pub ecosystems: Vec<Language>,
	#[serde(default)]
	pub packages: Vec<String>,
	pub limit: std::num::NonZeroU32,
	#[serde(default)]
	pub cursor: Option<String>,
	/// The caller's exploration session, when the results should accumulate
	/// into a session graph (the `/expand` surface).
	#[serde(default)]
	pub session: Option<uuid::Uuid>,
}

impl SearchRequestDto {
	/// Lower the wire request into the typed [`Search`]. A semantic opt-in
	/// becomes an [`AbstractQuery`] (which only the planner may escalate);
	/// everything else is a validated, operator-escaped literal.
	pub fn into_search(self) -> Result<Search<'static>, ServerError> {
		let query = if self.semantic {
			Query::Abstract(AbstractQuery::NaturalLanguage(self.query))
		} else {
			Query::Literal(LiteralQuery::parse(&self.query).map_err(bad_request)?)
		};

		// Package names are ecosystem-scoped; a bare name is tried against every
		// requested (or, unstated, every known) ecosystem's grammar.
		let candidates: Vec<Language> = match self.ecosystems.as_slice() {
			[] => Language::iter().collect(),
			requested => requested.to_vec(),
		};
		let mut selectors = Vec::new();
		for raw in &self.packages {
			for &ecosystem in &candidates {
				if let Ok(name) = PackageName::new(ecosystem, raw.as_str()) {
					selectors.push(PackageSelector { name, version: None });
				}
			}
		}
		if !self.packages.is_empty() && selectors.is_empty() {
			return Err(ServerError::BadRequest(
				"no requested package name is valid for the requested ecosystems".into(),
			));
		}

		Ok(Search {
			query,
			filter: Filter {
				ecosystems: nonempty::NonEmpty::from_vec(self.ecosystems),
				packages: nonempty::NonEmpty::from_vec(selectors),
			},
			page: Pagination { limit: self.limit, after: self.cursor },
			_lifetime: std::marker::PhantomData,
		})
	}
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddPackageDto {
	pub ecosystem: Language,
	pub name: String,
	pub version: String,
	#[serde(default)]
	pub origin: Option<String>,
}

impl AddPackageDto {
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

fn default_origin(ecosystem: Language) -> RegistryOrigin {
	match ecosystem {
		Language::Rust => RegistryOrigin::CratesIo,
		Language::Typescript => RegistryOrigin::NpmPublic,
		Language::Python => RegistryOrigin::PyPi,
	}
}

/// The process-wide lookup table behind the `origin` field of an add-package
/// request. A free function ([`resolve_custom_origin`]) resolves against it, so
/// the table is installed once at assembly rather than threaded through every
/// DTO conversion.
static CUSTOM_REGISTRIES: LazyLock<ArcSwap<HashMap<SmolStr, Url>>> =
	LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Install (replacing wholesale) the operator-configured custom registries as
/// the origin lookup table. Called once per assembly from [`crate::Server`].
pub fn register_custom_registries(registries: &[CustomRegistry]) {
	let table: HashMap<SmolStr, Url> = registries
		.iter()
		.map(|registry| (registry.name.clone(), registry.url.clone()))
		.collect();
	tracing::debug!(registries = table.len(), "custom registries registered");
	CUSTOM_REGISTRIES.store(Arc::new(table));
}

fn resolve_custom_origin(name: &str) -> Result<RegistryOrigin, ServerError> {
	CUSTOM_REGISTRIES
		.load()
		.get(name)
		.map(|url| RegistryOrigin::Custom { name: SmolStr::new(name), url: url.clone() })
		.ok_or_else(|| ServerError::BadRequest(format!("unknown custom registry {name:?}")))
}

/// The wire error a DTO field rejection projects to.
fn bad_request(error: impl std::fmt::Display) -> ServerError {
	ServerError::BadRequest(error.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	pub ready: bool,
	pub degraded: Vec<heart::BackendKind>,
}
