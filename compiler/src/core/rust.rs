use std::{collections::{BTreeSet, HashMap, VecDeque}, fs, path::PathBuf, process::Command, time::Duration};

use cargo_metadata::{Metadata, MetadataCommand, Package as CargoPackage, PackageId};
use crates_io_api::{AsyncClient, Crate};
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, instrument};
use url::Url;

use crate::{core::rust_parser::RustdocParser, error::{PackageError, RegistryError, summarize_command_output}, git::find_commit_for_version, pipeline::{Collected, Ir}};

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
fn default_rust_language() -> Language { Language::Rust }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RustPackage {
	pub slug:        String,
	pub name:        String,
	#[serde(skip, default = "default_rust_language")]
	pub language:    Language,
	#[serde(default)]
	pub uuid:        u64,
	pub source:      Url,
	pub direct_repo: bool,
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
			direct_repo: false,
			description: None,
		}
	}
}

impl RustPackage {
	fn cargo_package_spec_for(package_name: &str, version: &Version) -> String {
		format!("{package_name}@{version}")
	}

	fn run_cargo_rustdoc(
		&self,
		code: &PathBuf,
		package_name: &str,
		version: &Version,
		lib_only: bool,
	) -> Result<std::process::Output, PackageError> {
		let mut command = Command::new("cargo");
		command
			.arg("rustdoc")
			.arg("--package")
			.arg(Self::cargo_package_spec_for(package_name, version));
		if lib_only {
			command.arg("--lib");
		}
		command
			.arg("--")
			.args(self.direct_repo.then_some("--document-private-items"))
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
	fn generate_ir_for_package(
		&self,
		code: &PathBuf,
		package_name: &str,
		doc_target_name: &str,
		version: &Version,
	) -> Result<(Ir<Collected>, HashMap<String, String>), PackageError> {
		let target_dir = code.join("target").join("doc_json");

		if !target_dir.exists() {
			fs::create_dir_all(&target_dir)?;
		}

		match self.run_cargo_rustdoc(code, package_name, version, false) {
			Ok(_) => {}
			Err(PackageError::Process { details, .. })
				if details.contains("extra arguments to `rustdoc` can only be passed to one target") =>
			{
				self.run_cargo_rustdoc(code, package_name, version, true)?;
			}
			Err(error) => return Err(error),
		}

		let json_path =
			code.join("target").join("doc").join(format!("{}.json", doc_target_name.replace('-', "_")));

		let json_content = fs::read_to_string(&json_path)?;

		let rustdoc_crate: rustdoc_types::Crate = serde_json::from_str(&json_content)?;
		debug!("rustdoc JSON parsed");

		// Build the tree-sitter source map from the *same* parsed crate, before it
		// is moved into the parser — the JSON is parsed exactly once per package.
		let source_map = source_map_from_crate(&rustdoc_crate, code);

		let mut parser = RustdocParser::from_doc(rustdoc_crate)?;

		let parse_result = parser.parse()?;
		info!(entries = parse_result.len(), "IR generation complete");

		Ok((Ir::from_entries(parse_result), source_map))
	}

	/// Generate the IR and the raw-source map in one pass. The source map keys
	/// fully-qualified function names to their raw source so the ingest pipeline
	/// can run tree-sitter without re-parsing the rustdoc JSON.
	pub(crate) fn generate_ir_with_sources(
		&self,
		code: &PathBuf,
		version: &Version,
	) -> Result<(Ir<Collected>, HashMap<String, String>), PackageError> {
		let metadata = cargo_metadata(code)?;
		let packages = documented_local_packages(&metadata, &self.name, self.direct_repo);
		let mut entries = Vec::new();
		let mut source_map = HashMap::new();

		for package_id in packages {
			let package = metadata
				.packages
				.iter()
				.find(|candidate| candidate.id == package_id)
				.expect("documented package id should exist in metadata");
			let doc_target_name =
				package_doc_target_name(package).unwrap_or_else(|| package.name.to_string());
			let (package_ir, package_sources) =
				self.generate_ir_for_package(code, &package.name, &doc_target_name, version)?;
			entries.extend(package_ir.into_entries());
			source_map.extend(package_sources);
		}

		Ok((Ir::from_entries(entries), source_map))
	}

	pub(crate) fn generate_ir(
		&self,
		code: &PathBuf,
		version: &Version,
	) -> Result<Ir<Collected>, PackageError> {
		self.generate_ir_with_sources(code, version).map(|(ir, _)| ir)
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
			direct_repo: false,
			description: c.description,
		}
	}
}

fn cargo_metadata(code: &PathBuf) -> Result<Metadata, PackageError> {
	MetadataCommand::new()
		.current_dir(code)
		.exec()
		.map_err(|source| PackageError::Metadata(source.to_string()))
}

/// Build a map from fully-qualified symbol name → raw Rust source for every
/// `Function` item that carries a span, reading the source files referenced by
/// the already-parsed rustdoc `Crate`. Reusing the parsed crate avoids a second
/// deserialization of the (large) rustdoc JSON in the ingest pipeline.
fn source_map_from_crate(krate: &rustdoc_types::Crate, workspace: &PathBuf) -> HashMap<String, String> {
	let mut map = HashMap::new();
	for (id, item) in &krate.index {
		if !matches!(&item.inner, rustdoc_types::ItemEnum::Function(_)) {
			continue;
		}
		let Some(span) = &item.span else { continue };
		let Some(summary) = krate.paths.get(id) else { continue };
		let fq_name = summary.path.join("::");

		let source_file = workspace.join(&span.filename);
		let Ok(source) = fs::read_to_string(&source_file) else { continue };

		// span.begin / span.end are 1-indexed (line, col) tuples.
		let raw: String = source
			.lines()
			.enumerate()
			.filter(|(i, _)| *i + 1 >= span.begin.0 && *i + 1 <= span.end.0)
			.map(|(_, line)| line)
			.collect::<Vec<_>>()
			.join("\n");

		if !raw.is_empty() {
			map.insert(fq_name, raw);
		}
	}
	map
}

fn documented_local_packages(
	metadata: &Metadata,
	root_package_name: &str,
	direct_repo: bool,
) -> Vec<PackageId> {
	let Some(root_package) =
		metadata.packages.iter().find(|package| package.name == root_package_name)
	else {
		return Vec::new();
	};
	let mut documented = vec![root_package.id.clone()];
	if !direct_repo || package_has_library(root_package) {
		return documented;
	}

	let Some(resolve) = &metadata.resolve else {
		return documented;
	};
	let package_map: HashMap<_, _> =
		metadata.packages.iter().map(|package| (&package.id, package)).collect();
	let node_map: HashMap<_, _> = resolve.nodes.iter().map(|node| (&node.id, node)).collect();
	let workspace_root = metadata.workspace_root.as_std_path();
	let mut seen = BTreeSet::from([root_package.id.clone()]);
	let mut queue = VecDeque::from([root_package.id.clone()]);

	while let Some(package_id) = queue.pop_front() {
		let Some(node) = node_map.get(&package_id) else {
			continue;
		};

		for dependency in &node.deps {
			let dependency_id = &dependency.pkg;
			if !seen.insert(dependency_id.clone()) {
				continue;
			}
			queue.push_back(dependency_id.clone());

			let Some(package) = package_map.get(dependency_id) else {
				continue;
			};
			if !package.manifest_path.as_std_path().starts_with(workspace_root) {
				continue;
			}
			if package_has_library(package) {
				documented.push(package.id.clone());
			}
		}
	}

	documented
}

fn package_has_library(package: &CargoPackage) -> bool {
	package.targets.iter().any(|target| target.kind.iter().any(is_library_target_kind))
}

fn package_doc_target_name(package: &CargoPackage) -> Option<String> {
	package
		.targets
		.iter()
		.find(|target| target.kind.iter().any(is_library_target_kind))
		.or_else(|| {
			package
				.targets
				.iter()
				.find(|target| !target.kind.iter().any(|kind| kind.to_string() == "custom-build"))
		})
		.map(|target| target.name.clone())
}

fn is_library_target_kind(kind: &cargo_metadata::TargetKind) -> bool {
	matches!(kind.to_string().as_str(), "lib" | "rlib" | "staticlib" | "cdylib" | "dylib")
}

impl From<Crate> for RustPackage {
	fn from(c: Crate) -> Self { Self::from_registry_crate(c) }
}

impl RustPackage {
	/// Clone the source, materialize the commit for `version`, and generate IR.
	///
	/// Used by the live integration tests; the server's sync path drives
	/// [`generate_ir_with_sources`](Self::generate_ir_with_sources) directly so it
	/// can reuse an already-checked-out workspace.
	#[instrument(skip(self, _flags), fields(package = %self.name, version = %version))]
	pub fn retrieve(
		&self,
		version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, PackageError> {
		let scratch = tempfile::tempdir()?;
		let repository_dir = scratch.path().join("repository");
		let workspace_dir = scratch.path().join("workspace");
		let repository = crate::git::clone_repository(&repository_dir, &self.source)?;

		let target_oid = find_commit_for_version(&repository, &version, &self.name, None)
			.ok_or_else(|| PackageError::VersionNotFound(version.clone()))?;
		crate::git::materialize_commit(&repository, target_oid, &workspace_dir)?;
		self.generate_ir(&workspace_dir, &version)
	}
}

impl Crates {
	/// Construct a crates.io registry client.
	pub fn new() -> Self {
		Self {
			client: AsyncClient::new("my_bot (help@my_bot.com)", Duration::from_secs(1)).unwrap(),
		}
	}

	/// Look up the crate metadata for `name`.
	pub async fn get_packages_by_name(&self, name: &str) -> Result<Vec<RustPackage>, RegistryError> {
		let c = self.client.get_crate(name).await?;
		Ok(vec![RustPackage::from(c.crate_data)])
	}
}

impl Default for Crates {
	fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
	use std::{path::PathBuf, time::Duration};

	use lang_types::Language;
	use tempfile::TempDir;
	use url::Url;

	use super::*;
	use crate::git::{clone_repository, find_commit_for_version, materialize_commit};

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
		assert_eq!(
			RustPackage::cargo_package_spec_for("serde_json", &Version::parse("1.0.82").unwrap()),
			"serde_json@1.0.82"
		);
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

	#[test]
	#[ignore = "manual live IR generation repro for direct repository-backed Rust packages"]
	fn rust_direct_repository_ir_generation_repro() {
		let package = RustPackage {
			slug:        "inko".into(),
			name:        "inko".into(),
			language:    Language::Rust,
			uuid:        0,
			source:      Url::parse("https://github.com/inko-lang/inko").unwrap(),
			direct_repo: true,
			description: None,
		};

		let version = Version::parse("0.20.0").unwrap();
		let result = package.retrieve(version.clone(), None);
		match result {
			Ok(ir) => {
				println!("inko@{version} generated {} entries", ir.entries().len());
			}
			Err(error) => {
				panic!("inko@{version} direct repository IR generation failed: {error:#}");
			}
		}
	}
}
