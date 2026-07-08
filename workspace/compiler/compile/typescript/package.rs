//! Resolving a TypeScript package's documentation surface: discovering the
//! entry point and declaration roots from `package.json`
//! (types/typings/module/main/exports, triple-slash references), building the
//! deno-graph module graph, and lowering deno-doc's output into the IR.
//!
//! The pipeline hands this module an already-materialized source root (see
//! `crate::generate::PackageInput`); acquisition (registry download, VCS
//! checkout) happens upstream.

use std::{collections::BTreeSet, fs, future::Future, path::{Path, PathBuf}, sync::Arc};

use deno_doc::{DocParser, DocParserOptions, Document};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph, ModuleSpecifier, ast::CapturingModuleAnalyzer, source::{LoadFuture, LoadOptions, LoadResponse, Loader}};
use ir::pipeline::{Collected, Ir};
use rustc_hash::FxHashMap as HashMap;
use tracing::{info, instrument};

use super::{TsDocParser, error::Package as PackageError};

/// A TypeScript package to document from a local source tree.
#[derive(Clone, Debug)]
pub struct TypescriptPackage {
	/// The package name as its `package.json` reports it.
	pub name: String,
}

impl TypescriptPackage {
	/// Lower the materialized package at `code` into the collected IR.
	///
	/// `code` may be the package root directory (the entry point is then
	/// discovered from `package.json` and conventional fallbacks) or a single
	/// TypeScript file to document directly.
	#[instrument(skip_all, fields(package = %self.name))]
	pub fn generate_ir(&self, code: &Path) -> Result<Ir<Collected>, PackageError> {
		let entry_point =
			if code.is_file() { code.to_path_buf() } else { resolve_materialized_entry_point(code)? };
		Self::generate_ir_from_entry_point(&entry_point)
	}

	/// Lower the package whose entry point is `entry_point` into the collected
	/// IR, expanding the entry into its full declaration-root set first.
	pub fn generate_ir_from_entry_point(entry_point: &Path) -> Result<Ir<Collected>, PackageError> {
		let doc_roots = documentation_roots_for_entry_point(entry_point)?;
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

		let documents = build_documents_from_roots(roots, &SourceFileLoader)?;
		documents_to_ir(documents)
	}
}

/// Drive a deno future to completion on a fresh current-thread Tokio runtime.
///
/// The compiler is otherwise synchronous; deno_graph's loader interface is the
/// only async boundary, so it is contained here (mirroring the legacy
/// producers backend) rather than threading a runtime through the pipeline.
fn block_on<F: Future>(future: F) -> Result<F::Output, PackageError> {
	let runtime =
		tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(PackageError::Io)?;
	Ok(runtime.block_on(future))
}

fn build_documents_from_roots(
	roots: Vec<ModuleSpecifier>,
	loader: &impl Loader,
) -> Result<HashMap<String, Document>, PackageError> {
	let analyzer = CapturingModuleAnalyzer::default();
	let mut graph = ModuleGraph::new(GraphKind::TypesOnly);
	block_on(async {
		graph
			.build(roots.clone(), Vec::new(), loader, BuildOptions {
				module_analyzer: &analyzer,
				..Default::default()
			})
			.await;
	})?;

	let parser = DocParser::new(&graph, &analyzer, &roots, DocParserOptions {
		diagnostics: false,
		private:     true,
	})
	.map_err(|source| PackageError::Graph(source.to_string()))?;
	let parse_output = parser.parse().map_err(|source| PackageError::Graph(source.to_string()))?;
	Ok(parse_output
		.into_iter()
		.map(|(specifier, document)| (specifier.to_string(), document))
		.collect())
}

fn documents_to_ir(documents: HashMap<String, Document>) -> Result<Ir<Collected>, PackageError> {
	let mut parser = TsDocParser::from_doc(documents)?;
	let entries = parser.parse()?;
	info!(entries = entries.len(), "IR generation complete");
	Ok(Ir::from_entries(entries))
}

// ─── Entry-point / declaration-root discovery ────────────────────────────────

const SOURCE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts"];
const DECLARATION_EXTENSIONS: &[&str] = &[".d.ts", ".d.tsx", ".d.mts", ".d.cts"];
const JS_EXTENSIONS: &[&str] = &[".js", ".jsx", ".mjs", ".cjs"];

/// Resolve the entry point of a materialized package root: the manifest's
/// `types`/`typings`/`main`, then the conventional `mod.ts`/`index.ts` spots.
pub fn resolve_materialized_entry_point(root: &Path) -> Result<PathBuf, PackageError> {
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

/// Expand an entry point into the full set of declaration roots to document,
/// following `package.json` fields and triple-slash/import references.
pub fn documentation_roots_for_entry_point(
	entry_point: &Path,
) -> Result<Vec<PathBuf>, PackageError> {
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
				&& let Some(start) = rest.find('"').or_else(|| rest.find('\''))
			{
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
	let all_suffixes: Vec<&str> =
		SOURCE_EXTENSIONS.iter().chain(DECLARATION_EXTENSIONS.iter()).copied().collect();
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

/// A `deno_graph` loader that serves `file://` specifiers from disk and
/// declines everything else (the surface is materialized ahead of time).
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
					format!("not a file URL: {specifier}"),
				)))
			})?;
			let content =
				fs::read_to_string(&path).map_err(|e| deno_graph::source::LoadError::Other(Arc::new(e)))?;
			Ok(Some(LoadResponse::Module {
				specifier,
				maybe_headers: None,
				content: content.into_bytes().into(),
				mtime: None,
			}))
		})
	}
}
