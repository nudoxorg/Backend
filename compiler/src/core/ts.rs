/// TypeScript/JavaScript package and registry types.
///
/// Mirrors the architecture of `compiler/src/core/rust.rs` for the Rust
/// pipeline. `TsPackage` prefers the in-process Rust `deno_doc` path for local
/// files and falls back to the Deno CLI for remote/npm/jsr specifiers.
use std::{collections::HashMap, io::Cursor, path::{Path, PathBuf}, process::{Command, ExitStatus}, sync::Arc};

use deno_doc::{DocParser, DocParserOptions};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier, ast::CapturingModuleAnalyzer, source::{LoadFuture, LoadOptions, LoadResponse, Loader}};
use semver::Version;
use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::{core::ts_parser::{ParseError, TsDocParser}, pipeline::{Collected, Ir}, traits::{package::Package, registry::Registry}};

// ============================================================================
// Error types
// ============================================================================

/// Parse/conversion errors from the TypeScript pipeline.
/// Mirrors `crate::core::rust::ParseError`.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TsParseError {
	#[error("symbol not found: {0}")]
	SymbolNotFound(String),

	#[error("type resolution failed for `{type_name}`: {reason}")]
	TypeResolution { type_name: String, reason: String },

	#[error("invalid declaration: {0}")]
	InvalidDeclaration(String),

	#[error("unsupported declaration kind: {0}")]
	UnsupportedDeclarationKind(String),

	#[error("circular dependency at path: {path}")]
	CircularDependency { path: String },

	#[error("generic constraint resolution failed: {reason}")]
	GenericConstraintResolution { reason: String },
}

/// Errors arising from TypeScript package operations.
/// Mirrors `crate::error::PackageError`.
#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum TsPackageError {
	#[error("IO error: {0}")]
	Io(#[from] std::io::Error),

	#[error("process `{command}` failed with {status}")]
	Process { command: String, status: ExitStatus },

	#[error("parse error: {0}")]
	Parse(#[from] ParseError),

	#[error("serialization error: {0}")]
	Serialization(#[from] serde_json::Error),

	#[error("UTF-8 decode error: {0}")]
	Utf8(#[from] std::string::FromUtf8Error),

	#[error("network error: {0}")]
	Network(#[from] reqwest::Error),

	#[error("feature not implemented")]
	NotImplemented,

	#[error("package not found: {0}")]
	NotFound(String),

	#[error("invalid URL: {0}")]
	InvalidUrl(String),

	#[error("invalid local entry point: {0}")]
	InvalidLocalEntryPoint(String),

	#[error("TypeScript document graph generation failed: {0}")]
	Graph(String),
}

// ============================================================================
// TsPackage — implements the Package trait
// ============================================================================

/// A TypeScript/JavaScript package that can be documented via `jsr:@deno/doc`.
///
/// Mirrors `RustPackage` in `rust.rs`.
#[derive(Clone, Debug)]
pub struct TsPackage {
	pub slug:        String,
	pub name:        String,
	pub uuid:        u64,
	pub source:      Url,
	pub description: Option<String>,
	/// The primary entry-point specifier passed to the documentation step.
	/// Can be a URL, local path, or `npm:`/`jsr:` specifier.
	pub entry_point: String,
}

impl TsPackage {
	/// Absolute path to the embedded Deno runner script.
	///
	/// The script lives in the same directory as this source file and is
	/// referenced at compile time so the path is always valid regardless of
	/// the working directory at runtime.
	const RUNNER_SCRIPT: &'static str =
		concat!(env!("CARGO_MANIFEST_DIR"), "/src/core/ts_doc_runner.ts");

	/// Generate the collected IR for this package.
	pub(crate) fn generate_ir(&self) -> Result<Ir<Collected>, TsPackageError> {
		if let Some(local_entry_point) = self.local_entry_point()? {
			return self.generate_ir_from_local_path(local_entry_point);
		}

		self.generate_ir_with_deno()
	}

	fn npm_slug(name: &str) -> String { name.to_ascii_lowercase().replace('/', "__") }

	fn generate_ir_from_local_path(
		&self,
		entry_point: PathBuf,
	) -> Result<Ir<Collected>, TsPackageError> {
		let root = ModuleSpecifier::from_file_path(&entry_point).map_err(|_| {
			TsPackageError::InvalidLocalEntryPoint(format!(
				"could not convert `{}` into a file module specifier",
				entry_point.display()
			))
		})?;

		let analyzer = CapturingModuleAnalyzer::default();
		let mut graph = ModuleGraph::new(GraphKind::TypesOnly);
		let loader = SourceFileLoader;
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.map_err(TsPackageError::Io)?;
		runtime.block_on(async {
			graph
				.build(vec![root.clone()], Vec::new(), &loader, BuildOptions {
					module_analyzer: &analyzer,
					..Default::default()
				})
				.await;
		});

		let roots = [root.clone()];
		let parser = DocParser::new(&graph, &analyzer, &roots, DocParserOptions {
			diagnostics: false,
			private:     true,
		})
		.map_err(|source| TsPackageError::Graph(source.to_string()))?;
		let parse_output =
			parser.parse().map_err(|source| TsPackageError::Graph(source.to_string()))?;
		let documents: HashMap<String, deno_doc::Document> = parse_output
			.into_iter()
			.map(|(specifier, document)| (specifier.to_string(), document))
			.collect();

		let mut parser = TsDocParser::new(documents)?;
		let entries = parser.parse_documents()?;

		Ok(Ir::from_entries(entries))
	}

	fn generate_ir_with_deno(&self) -> Result<Ir<Collected>, TsPackageError> {
		let output = Command::new("deno")
			.arg("run")
			.arg("--allow-read")
			.arg("--allow-net")
			.arg("--allow-env")
			.arg(Self::RUNNER_SCRIPT)
			.arg(&self.entry_point)
			.output()?;

		if !output.status.success() {
			return Err(TsPackageError::Process {
				command: format!("deno run ts_doc_runner {}", self.entry_point),
				status:  output.status,
			});
		}

		let json = String::from_utf8(output.stdout)?;
		let documents: HashMap<String, deno_doc::Document> = serde_json::from_str(&json)?;

		let mut parser = TsDocParser::new(documents)?;
		let entries = parser.parse_documents()?;

		Ok(Ir::from_entries(entries))
	}

	fn local_entry_point(&self) -> Result<Option<PathBuf>, TsPackageError> {
		if self.entry_point.starts_with("npm:")
			|| self.entry_point.starts_with("jsr:")
			|| self.entry_point.starts_with("http://")
			|| self.entry_point.starts_with("https://")
		{
			return Ok(None);
		}

		if self.entry_point.starts_with("file://") {
			let url = Url::parse(&self.entry_point)
				.map_err(|source| TsPackageError::InvalidUrl(source.to_string()))?;
			let path = url.to_file_path().map_err(|_| {
				TsPackageError::InvalidLocalEntryPoint(format!(
					"`{}` is not a valid file URL",
					self.entry_point
				))
			})?;
			return Ok(Some(path));
		}

		let path = PathBuf::from(&self.entry_point);
		if path.exists() {
			return Ok(Some(path.canonicalize()?));
		}

		Ok(None)
	}
}

impl Package for TsPackage {
	type Error = TsPackageError;

	fn get_available_versions(&self) -> Result<Vec<Version>, Self::Error> {
		Err(TsPackageError::NotImplemented)
	}

	fn flags(&self) -> Result<Option<Vec<String>>, Self::Error> { Ok(None) }

	fn description(&self) -> Result<Option<String>, Self::Error> { Ok(self.description.clone()) }

	fn retrieve(
		&self,
		_version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, Self::Error> {
		self.generate_ir()
	}

	fn dependencies(&self) -> Result<Vec<Self>, Self::Error> { Err(TsPackageError::NotImplemented) }

	fn dependents(&self) -> Result<Vec<Self>, Self::Error> { Err(TsPackageError::NotImplemented) }
}

/// npm registry client for discovering and fetching TypeScript packages.
///
/// Mirrors `Crates` in `rust.rs`.
pub struct Npm {
	/// Base URL for the npm registry API.
	pub registry_url: Url,
}

impl Default for Npm {
	fn default() -> Self { Self { registry_url: Url::parse("https://registry.npmjs.org").unwrap() } }
}

impl Npm {
	async fn fetch_package_metadata(&self, name: &str) -> Result<NpmPackageMetadata, TsPackageError> {
		let response = reqwest::Client::new()
			.get(self.package_metadata_url(name)?)
			.send()
			.await?
			.error_for_status()?;
		Ok(response.json().await?)
	}

	fn package_metadata_url(&self, name: &str) -> Result<Url, TsPackageError> {
		let mut url = self.registry_url.clone();
		url
			.path_segments_mut()
			.map_err(|_| TsPackageError::InvalidUrl(self.registry_url.to_string()))?
			.pop_if_empty()
			.push(name);
		Ok(url)
	}

	pub(crate) async fn resolve_package_version(
		&self,
		name: &str,
		version: &Version,
	) -> Result<TsPackage, TsPackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		let version_key = version.to_string();
		let package_version = metadata
			.versions
			.get(&version_key)
			.ok_or_else(|| TsPackageError::NotFound(format!("{name}@{version}")))?;
		Ok(metadata.to_versioned_package(name, version, package_version))
	}

	pub(crate) async fn materialize_package_version(
		&self,
		name: &str,
		version: &Version,
		destination: &Path,
	) -> Result<PathBuf, TsPackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		let version_key = version.to_string();
		let package_version = metadata
			.versions
			.get(&version_key)
			.ok_or_else(|| TsPackageError::NotFound(format!("{name}@{version}")))?;
		let tarball = package_version
			.dist
			.as_ref()
			.map(|distribution| distribution.tarball.clone())
			.ok_or_else(|| TsPackageError::NotFound(format!("missing tarball for {name}@{version}")))?;

		let bytes =
			reqwest::Client::new().get(tarball).send().await?.error_for_status()?.bytes().await?;
		unpack_npm_tarball(bytes.as_ref(), destination)?;
		resolve_materialized_entry_point(destination)
	}
}

impl Registry for Npm {
	type Error = TsPackageError;
	type Pkg = TsPackage;

	async fn search_packages(&self, query: &str) -> Result<Vec<TsPackage>, TsPackageError> {
		let response = reqwest::Client::new()
			.get(
				self
					.registry_url
					.join("-/v1/search")
					.map_err(|source| TsPackageError::InvalidUrl(source.to_string()))?,
			)
			.query(&[("text", query), ("size", "5")])
			.send()
			.await?
			.error_for_status()?;
		let payload: NpmSearchResponse = response.json().await?;
		Ok(payload.objects.into_iter().map(|object| object.package.into_package()).collect())
	}

	async fn get_package_by_id(&self, id: u64) -> Result<TsPackage, TsPackageError> {
		let _ = id;
		Err(TsPackageError::NotImplemented)
	}

	async fn get_packages_by_name(&self, name: &str) -> Result<Vec<TsPackage>, TsPackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		Ok(vec![metadata.to_package(name)])
	}

	async fn get_reference(&self) -> TsPackage {
		TsPackage {
			slug:        "typescript-reference".into(),
			name:        "TypeScript Reference".into(),
			uuid:        0,
			source:      Url::parse("https://www.typescriptlang.org/docs/").unwrap(),
			description: Some("The official TypeScript language reference documentation".into()),
			entry_point: "npm:typescript".into(),
		}
	}
}

#[derive(Debug, Deserialize)]
struct NpmPackageMetadata {
	name:        String,
	description: Option<String>,
	repository:  Option<NpmRepository>,
	homepage:    Option<String>,
	#[serde(default)]
	versions:    HashMap<String, NpmPackageVersion>,
}

impl NpmPackageMetadata {
	fn to_package(&self, requested_name: &str) -> TsPackage {
		TsPackage {
			slug:        TsPackage::npm_slug(&self.name),
			name:        self.name.clone(),
			uuid:        0,
			source:      package_source_url(
				requested_name,
				self.repository.as_ref(),
				self.homepage.as_deref(),
			),
			description: self.description.clone(),
			entry_point: format!("npm:{}", self.name),
		}
	}

	fn to_versioned_package(
		&self,
		requested_name: &str,
		version: &Version,
		package_version: &NpmPackageVersion,
	) -> TsPackage {
		let version_name = package_version.name.as_deref().unwrap_or(&self.name);
		TsPackage {
			slug:        TsPackage::npm_slug(version_name),
			name:        version_name.to_owned(),
			uuid:        0,
			source:      package_source_url(
				requested_name,
				package_version.repository.as_ref().or(self.repository.as_ref()),
				package_version.homepage.as_deref().or(self.homepage.as_deref()),
			),
			description: package_version.description.clone().or_else(|| self.description.clone()),
			entry_point: format!("npm:{version_name}@{version}"),
		}
	}
}

#[derive(Debug, Deserialize)]
struct NpmPackageVersion {
	name:        Option<String>,
	description: Option<String>,
	repository:  Option<NpmRepository>,
	homepage:    Option<String>,
	dist:        Option<NpmDistribution>,
}

#[derive(Debug, Deserialize)]
struct NpmDistribution {
	tarball: Url,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NpmRepository {
	String(String),
	Object { url: String },
}

#[derive(Debug, Deserialize)]
struct NpmSearchResponse {
	objects: Vec<NpmSearchObject>,
}

#[derive(Debug, Deserialize)]
struct NpmSearchObject {
	package: NpmSearchPackage,
}

#[derive(Debug, Deserialize)]
struct NpmSearchPackage {
	name:        String,
	description: Option<String>,
	links:       NpmSearchLinks,
}

impl NpmSearchPackage {
	fn into_package(self) -> TsPackage {
		let source = self
			.links
			.repository
			.as_deref()
			.and_then(normalize_package_url)
			.or_else(|| self.links.homepage.as_deref().and_then(normalize_package_url))
			.or_else(|| self.links.npm.as_deref().and_then(normalize_package_url))
			.unwrap_or_else(|| npm_package_page_url(&self.name));

		TsPackage {
			slug: TsPackage::npm_slug(&self.name),
			name: self.name.clone(),
			uuid: 0,
			source,
			description: self.description,
			entry_point: format!("npm:{}", self.name),
		}
	}
}

#[derive(Debug, Deserialize)]
struct NpmSearchLinks {
	npm:        Option<String>,
	homepage:   Option<String>,
	repository: Option<String>,
}

fn package_source_url(
	package_name: &str,
	repository: Option<&NpmRepository>,
	homepage: Option<&str>,
) -> Url {
	repository
		.and_then(repository_url)
		.or_else(|| homepage.and_then(normalize_package_url))
		.unwrap_or_else(|| npm_package_page_url(package_name))
}

fn repository_url(repository: &NpmRepository) -> Option<Url> {
	match repository {
		NpmRepository::String(url) => normalize_package_url(url),
		NpmRepository::Object { url } => normalize_package_url(url),
	}
}

fn normalize_package_url(url: &str) -> Option<Url> {
	Url::parse(url.trim_start_matches("git+")).ok()
}

fn npm_package_page_url(name: &str) -> Url {
	let registry_url = Url::parse("https://www.npmjs.com").unwrap();
	registry_url.join(&format!("package/{name}")).unwrap_or(registry_url)
}

fn unpack_npm_tarball(bytes: &[u8], destination: &Path) -> Result<(), TsPackageError> {
	let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
	let mut archive = tar::Archive::new(decoder);

	for entry in archive.entries()? {
		let mut entry = entry?;
		let path = entry.path()?;
		let mut components = path.components();
		let Some(_) = components.next() else {
			continue;
		};
		let relative_path: PathBuf = components.collect();

		if relative_path.as_os_str().is_empty() {
			continue;
		}

		let output_path = destination.join(relative_path);
		if let Some(parent) = output_path.parent() {
			std::fs::create_dir_all(parent)?;
		}
		entry.unpack(output_path)?;
	}

	Ok(())
}

fn resolve_materialized_entry_point(root: &Path) -> Result<PathBuf, TsPackageError> {
	if let Some(candidate) = read_entry_point_from_package_json(root)? {
		return ensure_materialized_entry_point(candidate);
	}

	for candidate in ["mod.ts", "index.ts", "src/mod.ts", "src/index.ts", "index.d.ts"] {
		let candidate = root.join(candidate);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	Err(TsPackageError::InvalidLocalEntryPoint(format!(
		"could not determine a TypeScript entry point in `{}`",
		root.display()
	)))
}

fn ensure_materialized_entry_point(candidate: PathBuf) -> Result<PathBuf, TsPackageError> {
	if candidate.is_file() {
		return Ok(candidate);
	}

	Err(TsPackageError::InvalidLocalEntryPoint(format!(
		"TypeScript entry point `{}` does not exist",
		candidate.display()
	)))
}

fn read_entry_point_from_package_json(root: &Path) -> Result<Option<PathBuf>, TsPackageError> {
	let manifest_path = root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(None);
	}

	let content = std::fs::read_to_string(&manifest_path)?;
	let manifest: serde_json::Value = serde_json::from_str(&content)?;

	for key in ["types", "typings", "module", "main"] {
		if let Some(value) = manifest.get(key).and_then(serde_json::Value::as_str) {
			return Ok(Some(root.join(value)));
		}
	}

	if let Some(exports) = manifest.get("exports") {
		for value in [
			exports.get(".").and_then(serde_json::Value::as_str),
			exports
				.get(".")
				.and_then(serde_json::Value::as_object)
				.and_then(|entry| entry.get("types"))
				.and_then(serde_json::Value::as_str),
			exports
				.get(".")
				.and_then(serde_json::Value::as_object)
				.and_then(|entry| entry.get("default"))
				.and_then(serde_json::Value::as_str),
			exports.get("types").and_then(serde_json::Value::as_str),
		] {
			if let Some(value) = value {
				return Ok(Some(root.join(value)));
			}
		}
	}

	Ok(None)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::traits::registry::Registry;

	#[tokio::test]
	async fn npm_registry_results_snapshot() {
		let registry = Npm::default();
		let packages = registry.get_packages_by_name("@types/node").await.unwrap();
		insta::assert_debug_snapshot!(packages);
	}

	#[tokio::test]
	async fn npm_version_resolution_snapshot() {
		let registry = Npm::default();
		let package = registry
			.resolve_package_version("@types/node", &Version::parse("24.0.0").unwrap())
			.await
			.unwrap();
		insta::assert_debug_snapshot!(package);
	}
}

struct SourceFileLoader;

impl Loader for SourceFileLoader {
	fn load(&self, specifier: &ModuleSpecifier, _options: LoadOptions) -> LoadFuture {
		let specifier = specifier.clone();
		Box::pin(async move {
			if specifier.scheme() != "file" {
				return Ok(None);
			}

			let path = specifier.to_file_path().map_err(|_| {
				deno_graph::source::LoadError::Other(Arc::new(std::io::Error::new(
					std::io::ErrorKind::InvalidInput,
					format!("invalid file specifier: {specifier}"),
				)))
			})?;

			let content = std::fs::read(path)
				.map_err(|source| deno_graph::source::LoadError::Other(Arc::new(source)))?;

			Ok(Some(LoadResponse::Module {
				specifier,
				maybe_headers: None,
				content: content.into(),
				mtime: None,
			}))
		})
	}
}
