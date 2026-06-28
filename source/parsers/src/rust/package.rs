use std::{collections::{BTreeSet, HashMap, VecDeque}, fs, io, path::{Path, PathBuf}, process::Command, time::Duration};

use cargo_metadata::{Metadata, MetadataCommand, Package as CargoPackage, PackageId};
use crates_io_api::{AsyncClient, Crate};
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, instrument};
use url::Url;

use ir::pipeline::{Collected, Ir};

use super::error::{PackageError, RegistryError};
use super::parse::RustdocParser;
use super::Result;

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
	) -> std::result::Result<std::process::Output, PackageError> {
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

	#[instrument(skip_all, fields(package = %self.name))]
	fn generate_ir_for_package(
		&self,
		code: &PathBuf,
		package_name: &str,
		doc_target_name: &str,
		version: &Version,
	) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), PackageError> {
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

		let source_map = source_map_from_crate(&rustdoc_crate, code);

		let mut parser = RustdocParser::from_doc(rustdoc_crate)?;

		let parse_result = parser.parse()?;
		info!(entries = parse_result.len(), "IR generation complete");

		Ok((Ir::from_entries(parse_result), source_map))
	}

	pub fn generate_ir_with_sources(
		&self,
		code: &PathBuf,
		version: &Version,
	) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), PackageError> {
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

	pub fn generate_ir(
		&self,
		code: &PathBuf,
		version: &Version,
	) -> std::result::Result<Ir<Collected>, PackageError> {
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

fn cargo_metadata(code: &PathBuf) -> std::result::Result<Metadata, PackageError> {
	MetadataCommand::new()
		.current_dir(code)
		.exec()
		.map_err(|source| PackageError::Metadata(source.to_string()))
}

fn source_map_from_crate(krate: &rustdoc_types::Crate, workspace: &Path) -> HashMap<String, String> {
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

		let raw: String = source
			.lines()
			.enumerate()
			.filter(|(i, _)| *i + 1 >= span.begin.0 && *i < span.end.0)
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

impl Crates {
	pub fn new() -> Self {
		Self {
			client: AsyncClient::new("my_bot (help@my_bot.com)", Duration::from_secs(1)).unwrap(),
		}
	}

	pub async fn get_packages_by_name(&self, name: &str) -> std::result::Result<Vec<RustPackage>, RegistryError> {
		let c = self.client.get_crate(name).await?;
		Ok(vec![RustPackage::from(c.crate_data)])
	}
}

impl Default for Crates {
	fn default() -> Self { Self::new() }
}

fn summarize_command_output(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let trimmed = text.trim();
	if trimmed.is_empty() {
		return String::new();
	}

	const LIMIT: usize = 2_000;
	if trimmed.len() <= LIMIT {
		return trimmed.to_owned();
	}

	let mut end = LIMIT;
	while !trimmed.is_char_boundary(end) {
		end -= 1;
	}

	format!("{}...", &trimmed[..end])
}
