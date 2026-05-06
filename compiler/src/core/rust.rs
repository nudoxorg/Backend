use std::{fs, path::PathBuf, process::Command};

use crates_io_api::{AsyncClient, Crate, CratesQuery};
use lang_types::Language;
use semver::Version;
use thiserror::Error;
use tracing::{debug, info, instrument};
use url::Url;

use crate::{core::rust_parser::RustdocParser, error::{PackageError, RegistryError, summarize_command_output}, git::find_commit_for_version, pipeline::{Collected, Ir}, traits::{package::Package, registry::Registry}};

#[derive(Error, Debug)]
#[allow(dead_code)]
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

#[allow(dead_code)]
#[derive(Clone, Debug)]
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
	fn cargo_package_spec(&self, version: &Version) -> String { format!("{}@{}", self.name, version) }

	fn run_cargo_rustdoc(
		&self,
		code: &PathBuf,
		version: &Version,
		lib_only: bool,
	) -> Result<std::process::Output, PackageError> {
		let mut command = Command::new("cargo");
		command.arg("rustdoc").arg("--package").arg(self.cargo_package_spec(version));
		if lib_only {
			command.arg("--lib");
		}
		command
			.arg("--")
			.arg("-Z")
			.arg("unstable-options")
			.arg("--output-format")
			.arg("json")
			.current_dir(code);

		let output = command.output()?;
		if output.status.success() {
			return Ok(output);
		}

		let stderr = summarize_command_output(&output.stderr);
		let stdout = summarize_command_output(&output.stdout);
		let details = if !stderr.is_empty() {
			format!(": {stderr}")
		} else if !stdout.is_empty() {
			format!(": {stdout}")
		} else {
			String::new()
		};

		Err(PackageError::Process {
			command: if lib_only { "cargo rustdoc --lib".into() } else { "cargo rustdoc".into() },
			status: output.status,
			details,
		})
	}

	/// Internal helper to run cargo rustdoc and return the parsed Entry IR.
	/// Takes an input of `code` which is the location of the source code on disk
	#[instrument(skip_all, fields(package = %self.name))]
	pub(crate) fn generate_ir(
		&self,
		code: &PathBuf,
		version: &Version,
	) -> Result<Ir<Collected>, PackageError> {
		let target_dir = code.join("target").join("doc_json");

		if !target_dir.exists() {
			fs::create_dir_all(&target_dir)?;
		}

		match self.run_cargo_rustdoc(code, version, false) {
			Ok(_) => {}
			Err(PackageError::Process { details, .. })
				if details.contains("extra arguments to `rustdoc` can only be passed to one target") =>
			{
				self.run_cargo_rustdoc(code, version, true)?;
			}
			Err(error) => return Err(error),
		}

		let json_path =
			code.join("target").join("doc").join(format!("{}.json", self.name.replace('-', "_")));

		let json_content = fs::read_to_string(&json_path)?;

		let rustdoc_crate: rustdoc_types::Crate = serde_json::from_str(&json_content)?;
		debug!("rustdoc JSON parsed");

		let mut parser = RustdocParser::new(rustdoc_crate)?;

		let parse_result = parser.parse_crate()?;
		info!(entries = parse_result.len(), "IR generation complete");

		Ok(Ir::from_entries(parse_result))
	}

	pub(crate) fn from_registry_crate(c: Crate) -> Self {
		let fallback = format!("https://crates.io/crates/{}", c.name);
		let source = c
			.repository
			.as_deref()
			.and_then(|repository| Url::parse(repository).ok())
			.or_else(|| Url::parse(&fallback).ok())
			.unwrap_or_else(|| Url::parse("https://crates.io").unwrap());

		RustPackage {
			slug: c.name.to_lowercase(),
			name: c.name.clone(),
			language: Language::Rust,
			uuid: c.id.parse::<u64>().unwrap_or(0),
			source,
			description: c.description,
		}
	}
}

impl From<Crate> for RustPackage {
	fn from(c: Crate) -> Self { Self::from_registry_crate(c) }
}

impl Package for RustPackage {
	type Error = PackageError;

	fn get_available_versions(&self) -> Result<Vec<Version>, Self::Error> {
		Err(PackageError::NotImplemented)
	}

	fn flags(&self) -> Result<Option<Vec<String>>, Self::Error> {
		Ok(Some(vec!["default".into(), "std".into(), "alloc".into()]))
	}

	fn description(&self) -> Result<Option<String>, Self::Error> { Ok(self.description.clone()) }

	#[instrument(skip(self, _flags), fields(package = %self.name, version = %version))]
	fn retrieve(
		&self,
		version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, Self::Error> {
		let scratch = tempfile::tempdir()?;
		let repository_dir = scratch.path().join("repository");
		let workspace_dir = scratch.path().join("workspace");
		let repository = crate::git::clone_repository(&repository_dir, &self.source)?;

		let target_oid = find_commit_for_version(&repository, &version, &self.name, None)
			.ok_or_else(|| PackageError::VersionNotFound(version.clone()))?;
		crate::git::materialize_commit(&repository, target_oid, &workspace_dir)?;
		self.generate_ir(&workspace_dir, &version)
	}

	fn dependencies(&self) -> Result<Vec<Self>, Self::Error> { Err(PackageError::NotImplemented) }

	fn dependents(&self) -> Result<Vec<Self>, Self::Error> { Err(PackageError::NotImplemented) }
}

impl Registry for Crates {
	type Error = RegistryError;
	type Pkg = RustPackage;

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

#[cfg(test)]
mod tests {
	use std::{path::PathBuf, time::Duration};

	use lang_types::Language;
	use tempfile::TempDir;
	use url::Url;

	use super::*;
	use crate::{git::{clone_repository, find_commit_for_version, materialize_commit}, traits::registry::Registry};

	fn live_registry() -> Crates {
		Crates {
			client: AsyncClient::new("nudox-tests (help@nudox.invalid)", Duration::from_secs(1)).unwrap(),
		}
	}

	#[tokio::test]
	async fn rust_registry_results_snapshot() {
		let registry = live_registry();
		let packages = registry.get_packages_by_name("serde").await.unwrap();
		insta::assert_debug_snapshot!(packages);
	}

	#[test]
	fn cargo_package_spec_is_version_qualified() {
		let package = RustPackage {
			slug:        "serde-json".into(),
			name:        "serde_json".into(),
			language:    Language::Rust,
			uuid:        1,
			source:      Url::parse("https://example.com/serde_json").unwrap(),
			description: None,
		};

		assert_eq!(package.cargo_package_spec(&Version::parse("1.0.82").unwrap()), "serde_json@1.0.82");
	}

	#[tokio::test]
	#[ignore = "manual live resolution check for repo-backed Rust crates"]
	async fn rust_registry_specific_crates_resolve() {
		let registry = live_registry();
		let cases = [("turbo-quant", "0.1.0"), ("turbovec", "0.1.3"), ("any-tts", "0.1.1")];

		for (crate_name, version) in cases {
			let package = registry
				.get_packages_by_name(crate_name)
				.await
				.unwrap_or_else(|error| panic!("failed to fetch crate metadata for {crate_name}: {error}"))
				.into_iter()
				.next()
				.unwrap_or_else(|| panic!("crate registry returned no package named {crate_name}"));

			let scratch = TempDir::new().unwrap();
			let repository_dir = scratch.path().join("repository");
			let workspace_dir = scratch.path().join("workspace");
			let repository = clone_repository(&repository_dir, &package.source)
				.unwrap_or_else(|error| panic!("failed to clone {}: {error}", package.source));
			let target = find_commit_for_version(
				&repository,
				&Version::parse(version).unwrap(),
				&package.name,
				None,
			)
			.unwrap_or_else(|| panic!("failed to resolve {crate_name} {version} to a commit"));

			materialize_commit(&repository, target, &workspace_dir)
				.unwrap_or_else(|error| panic!("failed to materialize {crate_name} {version}: {error}"));

			let cargo_manifest = locate_manifest_for_package(&workspace_dir, crate_name)
				.unwrap_or_else(|| panic!("failed to locate materialized manifest for {crate_name}"));
			let manifest = std::fs::read_to_string(&cargo_manifest)
				.unwrap_or_else(|error| panic!("failed to read {}: {error}", cargo_manifest.display()));

			assert!(
				manifest.contains(&format!("name = \"{crate_name}\"")),
				"manifest at {} did not contain package name {crate_name}",
				cargo_manifest.display()
			);
		}
	}

	fn locate_manifest_for_package(root: &PathBuf, package_name: &str) -> Option<PathBuf> {
		let mut stack = vec![root.clone()];
		while let Some(dir) = stack.pop() {
			let entries = std::fs::read_dir(&dir).ok()?;
			for entry in entries {
				let entry = entry.ok()?;
				let path = entry.path();
				if entry.file_type().ok()?.is_dir() {
					stack.push(path);
					continue;
				}

				if path.file_name().and_then(|name| name.to_str()) != Some("Cargo.toml") {
					continue;
				}

				let manifest = std::fs::read_to_string(&path).ok()?;
				if manifest.contains(&format!("name = \"{package_name}\"")) {
					return Some(path);
				}
			}
		}
		None
	}

	#[tokio::test]
	#[ignore = "manual live IR generation repro for Rust crates that failed in production"]
	async fn rust_registry_ir_generation_repros() {
		let registry = live_registry();
		let cases = [("cpal", "0.16.0"), ("tunes", "0.16.0"), ("serde_json", "1.0.82")];

		for (crate_name, version) in cases {
			let package = registry
				.get_packages_by_name(crate_name)
				.await
				.unwrap_or_else(|error| panic!("failed to fetch crate metadata for {crate_name}: {error}"))
				.into_iter()
				.next()
				.unwrap_or_else(|| panic!("registry returned no package for {crate_name}"));

			let version = Version::parse(version).unwrap();
			let result = package.retrieve(version.clone(), None);
			match result {
				Ok(ir) => {
					println!(
						"{crate_name}@{version} generated {} entries from {}",
						ir.entries().len(),
						package.source
					);
				}
				Err(error) => {
					panic!("{crate_name}@{version} IR generation failed from {}: {error:#}", package.source);
				}
			}
		}
	}
}
