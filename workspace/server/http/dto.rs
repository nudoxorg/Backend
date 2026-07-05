//! Request DTOs — the serialized shapes the API *accepts*.

use heart::{Language, PackageVersion, RegistryOrigin};
use registry::package::{Coordinates as PackageCoordinates, PackageName};
use serde::{Deserialize, Serialize};

use crate::error::ServerError;

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

fn resolve_custom_origin(name: &str) -> Result<RegistryOrigin, ServerError> {
	let _ = name;
	todo!("look up the configured custom registry base URL by name")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	pub ready: bool,
	pub degraded: Vec<heart::BackendKind>,
}
