/// TypeScript/JavaScript package and registry types.
///
/// Mirrors the architecture of `compiler/src/core/rust.rs` for the Rust
/// pipeline. `TsPackage` prefers the in-process Rust `deno_doc` path for local
/// files and falls back to the Deno CLI for remote/npm/jsr specifiers.
use std::{collections::{BTreeSet, HashMap}, fs, io::Cursor, path::{Path, PathBuf}, process::{Command, ExitStatus}, sync::Arc};

use deno_doc::{DocParser, DocParserOptions};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier, ast::CapturingModuleAnalyzer, source::{LoadFuture, LoadOptions, LoadResponse, Loader}};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{core::{parse_common::LanguageParser, ts_parser::{ParseError, TsDocParser}}, error::summarize_command_output, pipeline::{Collected, Ir}, traits::{package::Package, registry::Registry}};

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

	#[error("process `{command}` failed with {status}{details}")]
	Process { command: String, status: ExitStatus, details: String },

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
#[derive(Clone, Debug, Serialize, Deserialize)]
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
		let doc_roots = documentation_roots_for_entry_point(&entry_point)?;
		let roots = doc_roots
			.iter()
			.map(|root| {
				ModuleSpecifier::from_file_path(root).map_err(|_| {
					TsPackageError::InvalidLocalEntryPoint(format!(
						"could not convert `{}` into a file module specifier",
						root.display()
					))
				})
			})
			.collect::<Result<Vec<_>, _>>()?;

		let analyzer = CapturingModuleAnalyzer::default();
		let mut graph = ModuleGraph::new(GraphKind::TypesOnly);
		let loader = SourceFileLoader;
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.map_err(TsPackageError::Io)?;
		runtime.block_on(async {
			graph
				.build(roots.clone(), Vec::new(), &loader, BuildOptions {
					module_analyzer: &analyzer,
					..Default::default()
				})
				.await;
		});

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

		let mut parser = TsDocParser::from_doc(documents)?;
		let entries = parser.parse()?;

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
			let stderr = summarize_command_output(&output.stderr);
			let stdout = summarize_command_output(&output.stdout);
			let details = if !stderr.is_empty() {
				format!(": {stderr}")
			} else if !stdout.is_empty() {
				format!(": {stdout}")
			} else {
				String::new()
			};
			return Err(TsPackageError::Process {
				command: format!("deno run ts_doc_runner {}", self.entry_point),
				status: output.status,
				details,
			});
		}

		let json = String::from_utf8(output.stdout)?;
		let documents: HashMap<String, deno_doc::Document> = serde_json::from_str(&json)?;

		let mut parser = TsDocParser::from_doc(documents)?;
		let entries = parser.parse()?;

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
		return ensure_materialized_entry_point(root, candidate);
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

fn documentation_roots_for_entry_point(entry_point: &Path) -> Result<Vec<PathBuf>, TsPackageError> {
	let Some(package_root) = find_package_root(entry_point) else {
		return Ok(vec![entry_point.to_path_buf()]);
	};

	let roots = canonicalize_documentation_roots(resolve_package_documentation_roots(
		&package_root,
		entry_point,
	)?);
	if roots.is_empty() { Ok(vec![entry_point.to_path_buf()]) } else { Ok(roots) }
}

fn find_package_root(entry_point: &Path) -> Option<PathBuf> {
	let start = if entry_point.is_dir() { entry_point } else { entry_point.parent()? };
	for ancestor in start.ancestors() {
		if ancestor.join("package.json").is_file() {
			return Some(ancestor.to_path_buf());
		}
	}
	None
}

fn resolve_package_documentation_roots(
	package_root: &Path,
	entry_point: &Path,
) -> Result<Vec<PathBuf>, TsPackageError> {
	let manifest_path = package_root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(vec![entry_point.to_path_buf()]);
	}

	let content = fs::read_to_string(&manifest_path)?;
	let manifest: serde_json::Value = serde_json::from_str(&content)?;
	let mut explicit = BTreeSet::new();

	for key in ["types", "typings", "module", "main"] {
		if let Some(value) = manifest.get(key).and_then(serde_json::Value::as_str) {
			push_doc_root(package_root, value, &mut explicit);
		}
	}

	if let Some(exports) = manifest.get("exports") {
		collect_export_roots(package_root, exports, &mut explicit);
	}

	let mut preferred_declaration_roots = BTreeSet::new();
	let mut preferred_source_roots = BTreeSet::new();
	for root in &explicit {
		if has_supported_extension(root, DECLARATION_EXTENSIONS) {
			preferred_declaration_roots.insert(root.to_path_buf());
		} else if is_source_like(root) {
			preferred_source_roots.insert(root.to_path_buf());
		}
	}

	let roots = if !preferred_declaration_roots.is_empty() && preferred_source_roots.is_empty() {
		let declarations = expand_declaration_roots(package_root, &preferred_declaration_roots)?;
		if declarations.is_empty() { preferred_declaration_roots } else { declarations }
	} else if !preferred_declaration_roots.is_empty() {
		let declarations = expand_declaration_roots(package_root, &preferred_declaration_roots)?;
		if declarations.is_empty() { preferred_declaration_roots } else { declarations }
	} else if !preferred_source_roots.is_empty() {
		preferred_source_roots
	} else {
		let mut declarations = BTreeSet::new();
		collect_matching_files(package_root, &DECLARATION_EXTENSIONS, &mut declarations)?;
		if declarations.is_empty() {
			let mut sources = BTreeSet::new();
			collect_matching_files(package_root, &SOURCE_EXTENSIONS, &mut sources)?;
			if sources.is_empty() { explicit } else { sources }
		} else {
			declarations
		}
	};

	let roots = roots.into_iter().filter(|path| path.is_file()).collect::<Vec<_>>();
	Ok(if roots.is_empty() { vec![entry_point.to_path_buf()] } else { roots })
}

const SOURCE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts"];
const DECLARATION_EXTENSIONS: &[&str] = &[".d.ts", ".d.tsx", ".d.mts", ".d.cts"];

fn canonicalize_documentation_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut by_stem: HashMap<String, PathBuf> = HashMap::new();

	for root in roots {
		let canonical = root.canonicalize().unwrap_or(root);
		let stem = declaration_stem_key(&canonical);
		match by_stem.get(&stem) {
			Some(existing) if declaration_path_rank(existing) <= declaration_path_rank(&canonical) => {}
			_ => {
				by_stem.insert(stem, canonical);
			}
		}
	}

	let mut roots = by_stem.into_values().collect::<Vec<_>>();
	roots.sort();
	roots
}

fn declaration_stem_key(path: &Path) -> String {
	let value = path.to_string_lossy();
	strip_known_suffix(&value).unwrap_or_else(|| value.to_string())
}

fn declaration_path_rank(path: &Path) -> usize {
	let value = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
	match DECLARATION_EXTENSIONS.iter().position(|extension| value.ends_with(extension)) {
		Some(rank) => rank,
		None => DECLARATION_EXTENSIONS.len(),
	}
}

fn push_doc_root(package_root: &Path, value: &str, out: &mut BTreeSet<PathBuf>) {
	if value == "./package.json" || value.ends_with("/package.json") {
		return;
	}

	for candidate in resolve_doc_root_candidates(package_root, value) {
		out.insert(candidate);
	}
}

fn collect_export_roots(
	package_root: &Path,
	value: &serde_json::Value,
	out: &mut BTreeSet<PathBuf>,
) {
	match value {
		serde_json::Value::String(path) => push_doc_root(package_root, path, out),
		serde_json::Value::Array(values) => {
			for value in values {
				collect_export_roots(package_root, value, out);
			}
		}
		serde_json::Value::Object(map) => {
			for (key, value) in map {
				if matches!(key.as_str(), "types" | "@zod/source" | "default" | "import" | "require") {
					collect_export_roots(package_root, value, out);
					continue;
				}
				collect_export_roots(package_root, value, out);
			}
		}
		_ => {}
	}
}

fn collect_matching_files(
	root: &Path,
	extensions: &[&str],
	out: &mut BTreeSet<PathBuf>,
) -> Result<(), TsPackageError> {
	if !root.exists() {
		return Ok(());
	}
	if root.is_file() {
		if has_supported_extension(root, extensions) {
			out.insert(root.to_path_buf());
		}
		return Ok(());
	}

	for entry in fs::read_dir(root)? {
		let entry = entry?;
		let path = entry.path();
		if entry.file_type()?.is_dir() {
			let name = entry.file_name();
			if name == "node_modules" || name == ".git" {
				continue;
			}
			collect_matching_files(&path, extensions, out)?;
		} else if has_supported_extension(&path, extensions) {
			out.insert(path);
		}
	}

	Ok(())
}

fn has_supported_extension(path: &Path, extensions: &[&str]) -> bool {
	let name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default();
	extensions.iter().any(|extension| name.ends_with(extension))
}

fn is_source_like(path: &Path) -> bool {
	let name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default();
	SOURCE_EXTENSIONS.iter().any(|extension| name.ends_with(extension))
		&& !DECLARATION_EXTENSIONS.iter().any(|extension| name.ends_with(extension))
}

fn resolve_doc_root_candidates(package_root: &Path, value: &str) -> Vec<PathBuf> {
	let candidate = package_root.join(value);
	let mut roots = BTreeSet::new();
	for path in declaration_candidates_for_path(&candidate) {
		if path.is_file() {
			roots.insert(path);
		}
	}
	if roots.is_empty() && candidate.is_file() {
		roots.insert(candidate);
	}
	roots.into_iter().collect()
}

fn expand_declaration_roots(
	package_root: &Path,
	seeds: &BTreeSet<PathBuf>,
) -> Result<BTreeSet<PathBuf>, TsPackageError> {
	let mut visited = BTreeSet::new();
	let mut stack = seeds.iter().cloned().collect::<Vec<_>>();

	while let Some(path) = stack.pop() {
		if !visited.insert(path.clone()) {
			continue;
		}
		let content = fs::read_to_string(&path)?;
		let base_dir = path.parent().unwrap_or(package_root);
		for specifier in declaration_dependency_specifiers(&content) {
			for candidate in resolve_declaration_specifier(base_dir, &specifier) {
				if candidate.starts_with(package_root)
					&& candidate.is_file()
					&& !visited.contains(&candidate)
				{
					stack.push(candidate);
				}
			}
		}
	}

	Ok(visited)
}

fn declaration_dependency_specifiers(content: &str) -> Vec<String> {
	let mut specifiers = Vec::new();
	for line in content.lines() {
		let trimmed = line.trim();
		if trimmed.starts_with("export ") {
			for marker in [" from \"", " from '"] {
				specifiers.extend(
					extract_quoted_after_marker(trimmed, marker)
						.into_iter()
						.filter(|specifier| specifier.starts_with('.')),
				);
			}
		}
		if trimmed.starts_with("///") {
			for marker in ["path=\"", "path='"] {
				specifiers.extend(
					extract_quoted_after_marker(trimmed, marker)
						.into_iter()
						.filter(|specifier| !specifier.is_empty() && !specifier.contains("://")),
				);
			}
		}
	}
	specifiers.sort();
	specifiers.dedup();
	specifiers
}

fn extract_quoted_after_marker(content: &str, marker: &str) -> Vec<String> {
	let mut results = Vec::new();
	let mut cursor = content;
	let quote = marker.chars().last().unwrap_or('"');

	while let Some(idx) = cursor.find(marker) {
		let start = idx + marker.len();
		let rest = &cursor[start..];
		if let Some(end) = rest.find(quote) {
			results.push(rest[..end].to_string());
			cursor = &rest[end + quote.len_utf8()..];
		} else {
			break;
		}
	}

	results
}

fn resolve_declaration_specifier(base_dir: &Path, specifier: &str) -> Vec<PathBuf> {
	let candidate = base_dir.join(specifier);
	let mut resolved = BTreeSet::new();
	for path in declaration_candidates_for_path(&candidate) {
		if path.is_file() {
			resolved.insert(path);
		}
	}
	resolved.into_iter().collect()
}

fn declaration_candidates_for_path(candidate: &Path) -> Vec<PathBuf> {
	let mut paths = BTreeSet::new();
	let candidate_string = candidate.to_string_lossy();

	if has_supported_extension(candidate, DECLARATION_EXTENSIONS) && candidate.is_file() {
		paths.insert(candidate.to_path_buf());
	}
	if candidate.is_dir() {
		for extension in DECLARATION_EXTENSIONS {
			paths.insert(candidate.join(format!("index{extension}")));
		}
	}

	let mut stem_variants = Vec::new();
	if let Some(stripped) = strip_known_suffix(&candidate_string) {
		stem_variants.push(PathBuf::from(stripped));
	}
	stem_variants.push(candidate.to_path_buf());

	for stem in stem_variants {
		for extension in DECLARATION_EXTENSIONS {
			paths.insert(PathBuf::from(format!("{}{}", stem.to_string_lossy(), extension)));
		}
		for extension in DECLARATION_EXTENSIONS {
			paths.insert(stem.join(format!("index{extension}")));
		}
	}

	paths.into_iter().collect()
}

fn strip_known_suffix(path: &str) -> Option<String> {
	for suffix in [
		".d.ts", ".d.tsx", ".d.mts", ".d.cts", ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs",
		".cjs",
	] {
		if let Some(stripped) = path.strip_suffix(suffix) {
			return Some(stripped.to_string());
		}
	}
	None
}

fn ensure_materialized_entry_point(
	root: &Path,
	candidate: PathBuf,
) -> Result<PathBuf, TsPackageError> {
	if candidate.is_file() {
		return Ok(candidate);
	}

	for fallback in materialized_source_fallback_candidates(root, &candidate) {
		if fallback.is_file() {
			return Ok(fallback);
		}
	}

	Err(TsPackageError::InvalidLocalEntryPoint(format!(
		"TypeScript entry point `{}` does not exist",
		candidate.display()
	)))
}

fn materialized_source_fallback_candidates(root: &Path, candidate: &Path) -> Vec<PathBuf> {
	let Ok(relative) = candidate.strip_prefix(root) else {
		return Vec::new();
	};

	let variants = relative_variants_without_build_prefixes(relative);
	let mut fallbacks = BTreeSet::new();
	for variant in variants {
		for path in source_candidates_for_path(&root.join(&variant)) {
			fallbacks.insert(path);
		}
	}
	fallbacks.into_iter().collect()
}

fn relative_variants_without_build_prefixes(relative: &Path) -> Vec<PathBuf> {
	let mut variants = vec![relative.to_path_buf()];
	let components = relative.components().collect::<Vec<_>>();
	if components.len() >= 2 {
		let first = components[0].as_os_str().to_string_lossy();
		let second = components[1].as_os_str().to_string_lossy();
		if matches!(first.as_ref(), "lib" | "dist" | "build" | "esm" | "cjs") && second == "src" {
			let stripped = components[1..].iter().collect::<PathBuf>();
			variants.push(stripped);
		}
	}
	variants
}

fn source_candidates_for_path(candidate: &Path) -> Vec<PathBuf> {
	let mut paths = BTreeSet::new();
	let candidate_string = candidate.to_string_lossy();

	if is_source_like(candidate) && candidate.is_file() {
		paths.insert(candidate.to_path_buf());
	}
	if candidate.is_dir() {
		for extension in SOURCE_EXTENSIONS {
			paths.insert(candidate.join(format!("index{extension}")));
		}
	}

	let mut stem_variants = Vec::new();
	if let Some(stripped) = strip_known_suffix(&candidate_string) {
		stem_variants.push(PathBuf::from(stripped));
	}
	stem_variants.push(candidate.to_path_buf());

	for stem in stem_variants {
		for extension in SOURCE_EXTENSIONS {
			paths.insert(PathBuf::from(format!("{}{}", stem.to_string_lossy(), extension)));
		}
		for extension in SOURCE_EXTENSIONS {
			paths.insert(stem.join(format!("index{extension}")));
		}
	}

	paths.into_iter().collect()
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

	if let Some(exports) = manifest.get("exports")
		&& let Some(value) = [
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
		]
		.into_iter()
		.flatten()
		.next()
	{
		return Ok(Some(root.join(value)));
	}

	Ok(None)
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

#[cfg(test)]
mod tests {
	use std::{collections::HashMap, fs};

	use deno_doc::{DocParser, DocParserOptions};
	use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier, ast::CapturingModuleAnalyzer};

	use super::*;
	use crate::{core::ts_parser::TsDocParser, traits::registry::Registry};

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

	#[test]
	fn documentation_roots_follow_triple_slash_references_for_declaration_only_package() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(workspace.path().join("package.json"), r#"{"types":"./index.d.ts"}"#).unwrap();
		fs::write(
			workspace.path().join("index.d.ts"),
			"/// <reference path=\"./nested/extra.d.ts\" />\nexport interface Root {}\n",
		)
		.unwrap();
		fs::create_dir_all(workspace.path().join("nested")).unwrap();
		fs::write(workspace.path().join("nested").join("extra.d.ts"), "export interface Extra {}\n")
			.unwrap();

		let roots = documentation_roots_for_entry_point(&workspace.path().join("index.d.ts")).unwrap();
		assert_eq!(roots.len(), 2);
		assert!(roots.iter().any(|path| path.ends_with("index.d.ts")));
		assert!(roots.iter().any(|path| path.ends_with("nested/extra.d.ts")));
	}

	#[test]
	fn documentation_roots_follow_triple_slash_paths_without_dot_prefix() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(workspace.path().join("package.json"), r#"{"types":"index.d.ts"}"#).unwrap();
		fs::create_dir_all(workspace.path().join("compatibility")).unwrap();
		fs::write(
			workspace.path().join("index.d.ts"),
			"/// <reference path=\"compatibility/iterators.d.ts\" />\nexport interface Root {}\n",
		)
		.unwrap();
		fs::write(
			workspace.path().join("compatibility").join("iterators.d.ts"),
			"export interface IteratorShim {}\n",
		)
		.unwrap();

		let roots = documentation_roots_for_entry_point(&workspace.path().join("index.d.ts")).unwrap();
		assert_eq!(roots.len(), 2);
		assert!(roots.iter().any(|path| path.ends_with("index.d.ts")));
		assert!(roots.iter().any(|path| path.ends_with("compatibility/iterators.d.ts")));
	}

	#[test]
	fn documentation_roots_prefer_explicit_declarations_for_mixed_export_packages() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(
			workspace.path().join("package.json"),
			r#"{
				"exports": {
					".": {
						"types": "./index.d.cts",
						"@zod/source": "./src/index.ts"
					},
					"./v3": {
						"types": "./v3/index.d.cts",
						"@zod/source": "./src/v3/index.ts"
					}
				}
			}"#,
		)
		.unwrap();
		fs::create_dir_all(workspace.path().join("src").join("v3").join("benchmarks")).unwrap();
		fs::create_dir_all(workspace.path().join("v3")).unwrap();
		fs::write(workspace.path().join("index.d.cts"), "export {}\n").unwrap();
		fs::write(workspace.path().join("v3").join("index.d.cts"), "export {}\n").unwrap();
		fs::write(workspace.path().join("src").join("index.ts"), "export {}\n").unwrap();
		fs::write(workspace.path().join("src").join("v3").join("index.ts"), "export {}\n").unwrap();
		fs::write(
			workspace.path().join("src").join("v3").join("benchmarks").join("realworld.ts"),
			"export const noisy = true;\n",
		)
		.unwrap();

		let roots = documentation_roots_for_entry_point(&workspace.path().join("index.d.cts")).unwrap();
		assert_eq!(roots.len(), 2);
		assert!(
			roots.iter().all(|path| path.extension().and_then(|value| value.to_str()) == Some("cts"))
		);
		assert!(roots.iter().any(|path| path.ends_with("index.d.cts")));
		assert!(roots.iter().any(|path| path.ends_with("v3/index.d.cts")));
		assert!(!roots.iter().any(|path| path.ends_with("src/index.ts")));
		assert!(!roots.iter().any(|path| path.ends_with("benchmarks/realworld.ts")));
	}

	#[test]
	fn documentation_roots_follow_declaration_export_chains() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(workspace.path().join("package.json"), r#"{"types":"./index.d.ts"}"#).unwrap();
		fs::create_dir_all(workspace.path().join("v3")).unwrap();
		fs::write(workspace.path().join("index.d.ts"), "export * from \"./v3/external.js\";\n")
			.unwrap();
		fs::write(workspace.path().join("v3").join("external.d.ts"), "export interface External {}\n")
			.unwrap();

		let roots = documentation_roots_for_entry_point(&workspace.path().join("index.d.ts")).unwrap();
		assert_eq!(roots.len(), 2);
		assert!(roots.iter().any(|path| path.ends_with("index.d.ts")));
		assert!(roots.iter().any(|path| path.ends_with("v3/external.d.ts")));
	}

	#[test]
	fn documentation_roots_deduplicate_commonjs_and_esm_declaration_twins() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(
			workspace.path().join("package.json"),
			r#"{
				"types":"./index.d.ts",
				"exports": {
					".": {
						"types":"./index.d.cts"
					}
				}
			}"#,
		)
		.unwrap();
		fs::write(workspace.path().join("index.d.ts"), "export interface Root {}\n").unwrap();
		fs::write(workspace.path().join("index.d.cts"), "export interface Root {}\n").unwrap();

		let roots = documentation_roots_for_entry_point(&workspace.path().join("index.d.ts")).unwrap();
		assert_eq!(roots, vec![workspace.path().join("index.d.ts").canonicalize().unwrap()]);
	}

	#[test]
	fn push_doc_root_maps_javascript_exports_to_declarations() {
		let workspace = tempfile::tempdir().unwrap();
		fs::create_dir_all(workspace.path().join("non-secure")).unwrap();
		fs::write(
			workspace.path().join("non-secure").join("index.d.ts"),
			"export declare function nanoid(): string;\n",
		)
		.unwrap();

		let mut roots = BTreeSet::new();
		push_doc_root(workspace.path(), "./non-secure/index.js", &mut roots);
		assert_eq!(roots.len(), 1);
		assert!(roots.iter().any(|path| path.ends_with("non-secure/index.d.ts")));
	}

	#[test]
	fn resolve_materialized_entry_point_falls_back_to_source_for_old_repo_layouts() {
		let workspace = tempfile::tempdir().unwrap();
		fs::write(
			workspace.path().join("package.json"),
			r#"{
				"main":"./lib/src/index.js",
				"types":"./lib/src/index.d.ts"
			}"#,
		)
		.unwrap();
		fs::create_dir_all(workspace.path().join("src")).unwrap();
		fs::write(workspace.path().join("src").join("index.ts"), "export const z = 1;\n").unwrap();

		let resolved = resolve_materialized_entry_point(workspace.path()).unwrap();
		assert_eq!(resolved, workspace.path().join("src").join("index.ts"));
	}

	#[tokio::test]
	#[ignore = "live TypeScript package diagnostics"]
	async fn live_typescript_package_diagnostics() {
		for (name, version) in [("@types/node", "24.0.0"), ("zod", "3.25.76"), ("nanoid", "5.1.6")] {
			let registry = Npm::default();
			let version = Version::parse(version).unwrap();
			let package = registry.resolve_package_version(name, &version).await.unwrap();
			let workspace = tempfile::tempdir().unwrap();
			let entry_point =
				registry.materialize_package_version(name, &version, workspace.path()).await.unwrap();
			eprintln!("=== {name}@{version} ===");
			eprintln!("entry point: {}", entry_point.display());
			let doc_roots = documentation_roots_for_entry_point(&entry_point).unwrap();
			eprintln!("doc roots: {}", doc_roots.len());
			for root in doc_roots.iter().take(20) {
				eprintln!("  root {}", root.display());
			}

			let roots = doc_roots
				.iter()
				.map(|root| ModuleSpecifier::from_file_path(root).unwrap())
				.collect::<Vec<_>>();
			let analyzer = CapturingModuleAnalyzer::default();
			let mut graph = ModuleGraph::new(GraphKind::TypesOnly);
			let loader = SourceFileLoader;
			graph
				.build(roots.clone(), Vec::new(), &loader, BuildOptions {
					module_analyzer: &analyzer,
					..Default::default()
				})
				.await;

			let parser = DocParser::new(&graph, &analyzer, &roots, DocParserOptions {
				diagnostics: false,
				private:     true,
			})
			.unwrap();
			let parse_output = parser.parse().unwrap();
			eprintln!("doc modules: {}", parse_output.len());
			for (specifier, document) in &parse_output {
				eprintln!("  {} symbols={}", specifier, document.symbols.len());
			}

			let documents: HashMap<String, deno_doc::Document> = parse_output
				.into_iter()
				.map(|(specifier, document)| (specifier.to_string(), document))
				.collect();
			let mut ts_parser = TsDocParser::new(documents).unwrap();
			match ts_parser.parse_documents() {
				Ok(entries) => {
					eprintln!("entries={}", entries.len());
					for entry in entries.iter().take(10) {
						eprintln!("  {} {}", entry.kind_tag(), entry.name());
					}
				}
				Err(error) => panic!("failed to parse {name}@{version}: {error:?}"),
			}

			let local_package =
				TsPackage { entry_point: entry_point.display().to_string(), ..package.clone() };
			match tokio::task::spawn_blocking(move || local_package.generate_ir()).await.unwrap() {
				Ok(ir) => eprintln!("generate_ir index entries={}", ir.index().len()),
				Err(error) => eprintln!("generate_ir failed: {error:?}"),
			}
		}
	}

	#[tokio::test]
	#[ignore = "live zod usability regression"]
	async fn live_zod_index_contains_stable_representative_symbols() {
		let registry = Npm::default();
		let version = Version::parse("3.25.76").unwrap();
		let package = registry.resolve_package_version("zod", &version).await.unwrap();
		let workspace = tempfile::tempdir().unwrap();
		let entry_point =
			registry.materialize_package_version("zod", &version, workspace.path()).await.unwrap();
		let local_package = TsPackage { entry_point: entry_point.display().to_string(), ..package };

		let index = tokio::task::spawn_blocking(move || local_package.generate_ir().unwrap().index())
			.await
			.unwrap();
		let path_strings = index
			.iter()
			.map(|entry| match entry.path() {
				ir::entry::NudoxPath::Local(path) => path.to_string_lossy().into_owned(),
				ir::entry::NudoxPath::External { path, dependency } => {
					format!("{dependency}::{}", path.to_string_lossy())
				}
			})
			.collect::<Vec<String>>();
		let names = index.iter().map(|entry| (entry.kind_tag(), entry.name())).collect::<Vec<_>>();

		assert!(index.len() > 10_000, "expected deep zod graph, got {} entries", index.len());
		assert!(
			path_strings.iter().all(|path| !path.contains(".tmp")),
			"expected stable logical paths, found tempdir leak in {:?}",
			path_strings.iter().find(|path| path.contains(".tmp"))
		);

		assert!(names.iter().any(|(kind, name)| *kind == "record" && *name == "ZodString"));
		assert!(names.iter().any(|(kind, name)| *kind == "record" && *name == "ZodType"));
		assert!(names.iter().any(|(kind, name)| *kind == "record" && *name == "ZodError"));
		assert!(names.iter().any(|(kind, name)| *kind == "function" && *name == "string"));
		assert!(names.iter().any(|(kind, name)| *kind == "function" && *name == "object"));
		assert!(names.iter().any(|(kind, name)| *kind == "function" && *name == "union"));
		assert!(names.iter().any(|(kind, name)| *kind == "function" && *name == "parse"));
		assert!(names.iter().any(|(kind, name)| *kind == "function" && *name == "safeParse"));

		assert!(path_strings.iter().any(|path| path.ends_with("index::z")));
		assert!(path_strings.iter().any(|path| path.ends_with("::ZodType::parse")));
		assert!(path_strings.iter().any(|path| path.ends_with("::ZodError::format")));
		assert!(path_strings.iter().any(|path| path.ends_with("::ZodString")));
	}
}
