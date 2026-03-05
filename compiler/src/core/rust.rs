use std::{fs, path::PathBuf, process::Command};

use crates_io_api::{AsyncClient, Crate, CratesQuery};
use ir::entry::Entry;
use lang_types::Language;
use semver::Version;
use thiserror::Error;
use url::Url;

use crate::{
	core::rust_parser::RustdocParser,
	error::{PackageError, RegistryError},
	pipeline::{Collected, Ir},
	traits::{package::Package, registry::Registry},
};

#[derive(Error, Debug)]
pub enum ParseError {
	#[error("Item not found: {0}")]
	ItemNotFound(u32),

	#[error("Invalid item kind for ID {id}: expected {expected}, got {actual}")]
	InvalidItemKind { id: String, expected: String, actual: String },

	#[error("Missing required field: {field} in {context}")]
	MissingField { field: String, context: String },

	#[error("Re-export resolution failed for {path}: {reason}")]
	ReExportResolution { path: String, reason: String },

	#[error("Generic argument mismatch: expected {expected}, got {actual}")]
	GenericArgMismatch { expected: usize, actual: usize },

	#[error("Type resolution failed for {type_name}: {reason}")]
	TypeResolution { type_name: String, reason: String },

	#[error("Invalid visibility format: {0}")]
	InvalidVisibility(String),

	#[error("Path parsing error: {0}")]
	PathParsing(String),

	#[error("Unsupported item type: {0}")]
	UnsupportedItemType(String),

	#[error("Circular dependency detected: {path}")]
	CircularDependency { path: String },

	#[error("Generic constraint resolution failed: {reason}")]
	GenericConstraintResolution { reason: String },

	#[error("Trait bound resolution failed: {reason}")]
	TraitBoundResolution { reason: String },

	#[error("Invalid function signature: {reason}")]
	InvalidFunctionSignature { reason: String },

	#[error("Associated type resolution failed: {reason}")]
	AssociatedTypeResolution { reason: String },

	#[error("Impl block parsing failed: {reason}")]
	ImplBlockParsing { reason: String },

	#[error("Invalid primitive type: {0}")]
	InvalidPrimitive(String),
}

pub struct Crates {
	pub client: AsyncClient,
}

pub struct RustPackage {
	pub slug:        String,
	pub name:        String,
	pub language:    Language,
	pub uuid:        u64,
	pub source:      Url,
	pub description: Option<String>,
}

impl Default for RustPackage {
	fn default() -> Self {
		Self {
			slug:        String::default(),
			name:        String::default(),
			language:    Language::Rust,
			uuid:        0,
			source:      Url::parse("https://example.com").unwrap(),
			description: None,
		}
	}
}

impl RustPackage {
	/// Internal helper to run cargo rustdoc and return the parsed Entry IR.
	fn generate_ir(&self, version: &Version) -> Result<Vec<Entry>, PackageError> {
		let target_dir = std::env::current_dir()?.join("target").join("doc_json");

		if !target_dir.exists() {
			fs::create_dir_all(&target_dir)?;
		}

		let status = Command::new("cargo")
			.arg("rustdoc")
			.arg("--package")
			.arg(&self.name)
			.arg("--")
			.arg("-Z")
			.arg("unstable-options")
			.arg("--output-format")
			.arg("json")
			.status()
			.map_err(|e| PackageError::Process(format!("Failed to run cargo: {e}")))?;

		if !status.success() {
			return Err(PackageError::Process("Cargo rustdoc failed".to_string()));
		}

		let json_path =
			PathBuf::from("target/doc").join(format!("{}.json", self.name.replace('-', "_")));

		let json_content = fs::read_to_string(&json_path)?;

		let rustdoc_crate: rustdoc_types::Crate = serde_json::from_str(&json_content)?;

		let mut parser =
			RustdocParser::new(rustdoc_crate).map_err(|e| PackageError::Parse(e.to_string()))?;

		parser.parse_crate().map_err(|e| PackageError::Parse(e.to_string()))
	}
}

impl From<Crate> for RustPackage {
	fn from(c: Crate) -> Self {
		RustPackage {
			slug:        c.name.to_lowercase(),
			name:        c.name.clone(),
			language:    Language::Rust,
			uuid:        c.id.parse::<u64>().unwrap_or(0),
			source:      Url::parse(
				&c.repository.unwrap_or_else(|| format!("https://crates.io/crates/{}", c.name)),
			)
			.unwrap_or_else(|_| Url::parse("https://crates.io").unwrap()),
			description: c.description,
		}
	}
}

impl Package for RustPackage {
	type Error = PackageError;

	fn get_available_versions(&self) -> Result<Vec<Version>, Self::Error> {
		Err(PackageError::NotImplemented)
	}

	fn flags(&self) -> Result<Option<Vec<String>>, Self::Error> {
		Ok(Some(vec!["default".into(), "std".into(), "alloc".into()]))
	}

	fn description(&self) -> Result<Option<String>, Self::Error> {
		Ok(self.description.clone())
	}

	fn retrieve(
		&self,
		version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, Self::Error> {
		let entries = self.generate_ir(&version)?;
		Ok(Ir::from_entries(entries))
	}

	fn dependencies(&self) -> Result<Vec<Self>, Self::Error> {
		todo!("Implement dependency resolution")
	}

	fn dependents(&self) -> Result<Vec<Self>, Self::Error> {
		todo!("Implement reverse dependency resolution")
	}
}

impl Registry for Crates {
	type Pkg = RustPackage;
	type Error = RegistryError;

	async fn search_packages(&self, query: &str) -> Result<Vec<RustPackage>, RegistryError> {
		let q = CratesQuery::builder().search(query).build();
		let result = self.client.crates(q).await?;

		Ok(result.crates.into_iter().map(RustPackage::from).collect())
	}

	async fn get_package_by_id(&self, id: u64) -> Result<RustPackage, RegistryError> {
		let c = self.client.get_crate(&id.to_string()).await?;
		Ok(RustPackage::from(c.crate_data))
	}

	async fn get_packages_by_name(&self, name: &str) -> Result<Vec<RustPackage>, RegistryError> {
		let c = self.client.get_crate(name).await?;
		Ok(vec![RustPackage::from(c.crate_data)])
	}

	async fn get_reference(&self) -> RustPackage {
		RustPackage {
			slug:        "rust-reference".into(),
			name:        "The Rust Reference".into(),
			language:    Language::Rust,
			uuid:        0,
			source:      Url::parse("https://doc.rust-lang.org/reference/").unwrap(),
			description: Some("The official reference manual for the Rust language".into()),
		}
	}
}
