use std::{collections::BTreeSet, fs, io::Cursor, path::{Path, PathBuf}, process::{Command, ExitStatus}, sync::Arc};
use rustc_hash::FxHashMap as HashMap;

use deno_doc::{DocParser, DocParserOptions};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier, ast::CapturingModuleAnalyzer, source::{LoadFuture, LoadOptions, LoadResponse, Loader}};
use semver::Version;
use serde::{Deserialize, Serialize};
use url::Url;

use ir::pipeline::{Collected, Ir};

use super::error::{Parse, Package as PackageError};
use super::TsDocParser;

fn summarize_command_output(bytes: &[u8]) -> String {
	let text = String::from_utf8_lossy(bytes);
	let trimmed = text.trim();
	if trimmed.is_empty() { return String::new(); }
	const LIMIT: usize = 2_000;
	if trimmed.len() <= LIMIT { return trimmed.to_owned(); }
	let mut end = LIMIT;
	while !trimmed.is_char_boundary(end) { end -= 1; }
	format!("{}...", &trimmed[..end])
}

fn npm_slug(name: &str) -> String { name.to_ascii_lowercase().replace('/', "__") }

// ============================================================================
// Package — implements the Package trait
// ============================================================================

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
	pub slug:        String,
	pub name:        String,
	pub uuid:        u64,
	pub source:      Url,
	pub description: Option<String>,
	pub entry_point: String,
}

impl Package {
	const RUNNER_SCRIPT: &'static str =
		concat!(env!("CARGO_MANIFEST_DIR"), "/../server/src/core/ts_doc_runner.ts");

	pub fn retrieve(
		&self,
		_version: Version,
		_flags: Option<Vec<String>>,
	) -> Result<Ir<Collected>, PackageError> {
		self.generate_ir()
	}

	pub fn generate_ir(&self) -> Result<Ir<Collected>, PackageError> {
		if let Some(local_entry_point) = self.local_entry_point()? {
			return self.generate_ir_from_local_path(local_entry_point);
		}

		self.generate_ir_with_deno()
	}

	fn generate_ir_from_local_path(
		&self,
		entry_point: PathBuf,
	) -> Result<Ir<Collected>, PackageError> {
		let doc_roots = documentation_roots_for_entry_point(&entry_point)?;
		let roots = doc_roots
			.iter()
			.map(|root| {
				ModuleSpecifier::from_file_path(root).map_err(|_| {
					PackageError::InvalidLocalEntryPoint(format!(
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
			.map_err(PackageError::Io)?;
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
		.map_err(|source| PackageError::Graph(source.to_string()))?;
		let parse_output =
			parser.parse().map_err(|source| PackageError::Graph(source.to_string()))?;
		let documents: HashMap<String, deno_doc::Document> = parse_output
			.into_iter()
			.map(|(specifier, document)| (specifier.to_string(), document))
			.collect();

		let mut parser = TsDocParser::from_doc(documents)?;
		let entries = parser.parse()?;

		Ok(Ir::from_entries(entries))
	}

	fn generate_ir_with_deno(&self) -> Result<Ir<Collected>, PackageError> {
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
			return Err(PackageError::Process {
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

	fn local_entry_point(&self) -> Result<Option<PathBuf>, PackageError> {
		if self.entry_point.starts_with("npm:")
			|| self.entry_point.starts_with("jsr:")
			|| self.entry_point.starts_with("http://")
			|| self.entry_point.starts_with("https://")
		{
			return Ok(None);
		}

		if self.entry_point.starts_with("file://") {
			let url = Url::parse(&self.entry_point)
				.map_err(|source| PackageError::InvalidUrl(source.to_string()))?;
			let path = url.to_file_path().map_err(|_| {
				PackageError::InvalidLocalEntryPoint(format!(
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

pub struct Npm {
	pub registry_url: Url,
}

impl Default for Npm {
	fn default() -> Self { Self { registry_url: Url::parse("https://registry.npmjs.org").unwrap() } }
}

impl Npm {
	async fn fetch_package_metadata(&self, name: &str) -> Result<NpmPackageMetadata, PackageError> {
		let response = reqwest::Client::new()
			.get(self.package_metadata_url(name)?)
			.send()
			.await?
			.error_for_status()?;
		Ok(response.json().await?)
	}

	fn package_metadata_url(&self, name: &str) -> Result<Url, PackageError> {
		let mut url = self.registry_url.clone();
		url
			.path_segments_mut()
			.map_err(|_| PackageError::InvalidUrl(self.registry_url.to_string()))?
			.pop_if_empty()
			.push(name);
		Ok(url)
	}

	pub async fn resolve_package_version(
		&self,
		name: &str,
		version: &Version,
	) -> Result<Package, PackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		let version_key = version.to_string();
		let package_version = metadata
			.versions
			.get(&version_key)
			.ok_or_else(|| PackageError::NotFound(format!("{name}@{version}")))?;
		Ok(metadata.to_versioned_package(name, version, package_version))
	}

	pub async fn materialize_package_version(
		&self,
		name: &str,
		version: &Version,
		destination: &Path,
	) -> Result<PathBuf, PackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		let version_key = version.to_string();
		let package_version = metadata
			.versions
			.get(&version_key)
			.ok_or_else(|| PackageError::NotFound(format!("{name}@{version}")))?;
		let tarball = package_version
			.dist
			.as_ref()
			.map(|distribution| distribution.tarball.clone())
			.ok_or_else(|| PackageError::NotFound(format!("missing tarball for {name}@{version}")))?;

		let bytes =
			reqwest::Client::new().get(tarball).send().await?.error_for_status()?.bytes().await?;
		unpack_npm_tarball(bytes.as_ref(), destination)?;
		resolve_materialized_entry_point(destination)
	}

	#[cfg(test)]
	pub(crate) async fn get_packages_by_name(
		&self,
		name: &str,
	) -> Result<Vec<Package>, PackageError> {
		let metadata = self.fetch_package_metadata(name).await?;
		Ok(vec![metadata.to_package(name)])
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
	#[cfg(test)]
	fn to_package(&self, requested_name: &str) -> Package {
		Package {
			slug:        npm_slug(&self.name),
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
	) -> Package {
		let version_name = package_version.name.as_deref().unwrap_or(&self.name);
		Package {
			slug:        npm_slug(version_name),
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

fn unpack_npm_tarball(bytes: &[u8], destination: &Path) -> Result<(), PackageError> {
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

fn resolve_materialized_entry_point(root: &Path) -> Result<PathBuf, PackageError> {
	if let Some(candidate) = read_entry_point_from_package_json(root)? {
		if let Ok(path) = ensure_materialized_entry_point(root, candidate) {
			return Ok(path);
		}
	}

	for candidate in ["mod.ts", "index.ts", "src/mod.ts", "src/index.ts", "index.d.ts"] {
		let candidate = root.join(candidate);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	Err(PackageError::InvalidLocalEntryPoint(format!(
		"could not determine a TypeScript entry point in `{}`",
		root.display()
	)))
}

fn documentation_roots_for_entry_point(entry_point: &Path) -> Result<Vec<PathBuf>, PackageError> {
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
) -> Result<Vec<PathBuf>, PackageError> {
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

	let roots = if !preferred_declaration_roots.is_empty() {
		let declarations = expand_declaration_roots(package_root, &preferred_declaration_roots)?;
		if declarations.is_empty() { preferred_declaration_roots } else { declarations }
	} else if !preferred_source_roots.is_empty() {
		preferred_source_roots
	} else {
		let mut declarations = BTreeSet::new();
		collect_matching_files(package_root, DECLARATION_EXTENSIONS, &mut declarations)?;
		if declarations.is_empty() {
			let mut sources = BTreeSet::new();
			collect_matching_files(package_root, SOURCE_EXTENSIONS, &mut sources)?;
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
const JS_EXTENSIONS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs"];

fn canonicalize_documentation_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
	let mut by_stem: HashMap<String, PathBuf> = HashMap::default();

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
) -> Result<(), PackageError> {
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
) -> Result<BTreeSet<PathBuf>, PackageError> {
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
	let mut out = Vec::new();
	for line in content.lines() {
		let line = line.trim();
		for prefix in [
			"/// <reference path=",
			"/// <reference types=",
			"import ",
			"export ",
			"import type ",
			"export type ",
		] {
			if let Some(rest) = line.strip_prefix(prefix)
				&& let Some(start) = rest.find('"').or_else(|| rest.find('\'')) {
					let rest = &rest[start + 1..];
					if let Some(end) = rest.find('"').or_else(|| rest.find('\'')) {
						out.push(rest[..end].to_owned());
					}
				}
		}
	}
	out
}

fn resolve_declaration_specifier(base_dir: &Path, specifier: &str) -> Vec<PathBuf> {
	if specifier.starts_with('/') {
		return Vec::new();
	}
	let base = base_dir.join(specifier);
	declaration_candidates_for_path(&base)
}

fn declaration_candidates_for_path(base: &Path) -> Vec<PathBuf> {
	let mut candidates = Vec::new();
	candidates.push(base.to_path_buf());
	let base_str = base.to_string_lossy();
	for suffix in DECLARATION_EXTENSIONS {
		candidates.push(PathBuf::from(format!("{base_str}{suffix}")));
	}
	if let Some(stem) = strip_known_suffix(&base_str) {
		for suffix in DECLARATION_EXTENSIONS {
			candidates.push(PathBuf::from(format!("{stem}{suffix}")));
		}
	}
	for js_ext in JS_EXTENSIONS {
		if let Some(stem) = base_str.strip_suffix(js_ext) {
			for decl_ext in DECLARATION_EXTENSIONS {
				candidates.push(PathBuf::from(format!("{stem}{decl_ext}")));
			}
		}
	}
	candidates
}

fn strip_known_suffix(value: &str) -> Option<String> {
	let all_suffixes: Vec<&str> = SOURCE_EXTENSIONS
		.iter()
		.chain(DECLARATION_EXTENSIONS.iter())
		.copied()
		.collect();
	for suffix in &all_suffixes {
		if let Some(stem) = value.strip_suffix(suffix) {
			return Some(stem.to_owned());
		}
	}
	None
}

fn read_entry_point_from_package_json(root: &Path) -> Result<Option<PathBuf>, PackageError> {
	let manifest_path = root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(None);
	}
	let content = fs::read_to_string(&manifest_path)?;
	let manifest: serde_json::Value = serde_json::from_str(&content)?;
	for key in ["types", "typings", "main"] {
		if let Some(value) = manifest.get(key).and_then(serde_json::Value::as_str) {
			return Ok(Some(root.join(value)));
		}
	}
	Ok(None)
}

fn ensure_materialized_entry_point(
	root: &Path,
	candidate: PathBuf,
) -> Result<PathBuf, PackageError> {
	if candidate.is_file() {
		return Ok(candidate);
	}
	for suffix in DECLARATION_EXTENSIONS.iter().chain(SOURCE_EXTENSIONS.iter()) {
		let with_suffix = PathBuf::from(format!("{}{suffix}", candidate.display()));
		if with_suffix.is_file() {
			return Ok(with_suffix);
		}
	}
	Err(PackageError::InvalidLocalEntryPoint(format!(
		"entry point `{}` does not exist in `{}`",
		candidate.display(),
		root.display()
	)))
}

struct SourceFileLoader;

impl Loader for SourceFileLoader {
	fn load(
		&self,
		specifier: &ModuleSpecifier,
		_options: LoadOptions,
	) -> LoadFuture {
		let specifier = specifier.clone();
		Box::pin(async move {
			if specifier.scheme() != "file" {
				return Ok(None);
			}
			let path = specifier.to_file_path().map_err(|_| {
				deno_graph::source::LoadError::Other(Arc::new(std::io::Error::new(
					std::io::ErrorKind::InvalidInput,
					format!("not a file URL: {specifier}"),
				)))
			})?;
			let content = fs::read_to_string(&path).map_err(|e| {
				deno_graph::source::LoadError::Other(Arc::new(e))
			})?;
			Ok(Some(LoadResponse::Module {
				specifier,
				maybe_headers: None,
				content:       content.into_bytes().into(),
				mtime:         None,
			}))
		})
	}
}

#[cfg(test)]
mod tests {
	use std::{collections::BTreeSet, fs};

	use super::{documentation_roots_for_entry_point, push_doc_root, resolve_materialized_entry_point};

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
}
