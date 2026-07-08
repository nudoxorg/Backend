//! Entry-point resolution for TypeScript, including repository-backed packages
//! (the `repo:` hint) and the build-prefix-stripping fallback search.
//!
//! Turns a repository root + optional entry hint into the `.d.ts`/`.ts` entry
//! to document: the hint wins, then `package.json`
//! (types/typings/module/main/exports), then the conventional
//! `mod.ts`/`index.ts` locations. Hints that point at build output
//! (`lib/`, `dist/`, ...) are mapped back to their sources.

use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};

use super::error::Package as PackageError;

/// The entry-point prefix marking a repository-backed package
/// (`repo:<relative-entry>`).
pub const REPOSITORY_ENTRY_PREFIX: &str = "repo:";

/// Whether an entry-point string designates a repository-backed package.
pub fn uses_repository(entry_point: &str) -> bool {
	entry_point.starts_with(REPOSITORY_ENTRY_PREFIX)
}

/// The relative entry hint carried by a `repo:` entry point, if any.
pub fn repository_entry_hint(entry_point: &str) -> Option<&str> {
	entry_point.strip_prefix(REPOSITORY_ENTRY_PREFIX).filter(|hint| !hint.is_empty())
}

/// Resolve the file to document for a repository checkout: the explicit hint
/// first, then the manifest, then the conventional entry locations.
pub fn resolve_repository_entry_point(
	repository_root: &Path,
	entry_hint: Option<&str>,
) -> Result<PathBuf, PackageError> {
	if let Some(entry_hint) = entry_hint {
		let candidate = repository_root.join(entry_hint);
		return ensure_entry_point(repository_root, candidate);
	}

	if let Some(candidate) = entry_point_from_package_json(repository_root)? {
		return ensure_entry_point(repository_root, candidate);
	}

	for candidate in ["mod.ts", "index.ts", "src/mod.ts", "src/index.ts"] {
		let candidate = repository_root.join(candidate);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	Err(PackageError::EntryPointDiscovery {
		path: repository_root.to_string_lossy().into_owned(),
	})
}

fn ensure_entry_point(
	repository_root: &Path,
	candidate: PathBuf,
) -> Result<PathBuf, PackageError> {
	if candidate.is_file() {
		return Ok(candidate);
	}

	for fallback in source_fallback_candidates(repository_root, &candidate) {
		if fallback.is_file() {
			return Ok(fallback);
		}
	}

	Err(PackageError::EntryPointMissing { path: candidate.to_string_lossy().into_owned() })
}

fn source_fallback_candidates(repository_root: &Path, candidate: &Path) -> Vec<PathBuf> {
	let Ok(relative) = candidate.strip_prefix(repository_root) else {
		return Vec::new();
	};

	let variants = relative_variants_without_build_prefixes(relative);
	let mut fallbacks = BTreeSet::new();
	for variant in variants {
		for path in source_candidates_for_path(&repository_root.join(&variant)) {
			fallbacks.insert(path);
		}
	}
	fallbacks.into_iter().collect()
}

fn relative_variants_without_build_prefixes(relative: &Path) -> Vec<PathBuf> {
	let mut variants = vec![relative.to_path_buf()];
	let components = relative.components().collect::<Vec<_>>();
	if let [first, second, ..] = components.as_slice()
		&& matches!(first.as_os_str().to_str().unwrap_or(""), "lib" | "dist" | "build" | "esm" | "cjs")
		&& second.as_os_str() == "src"
	{
		variants.push(components[1..].iter().collect::<PathBuf>());
	}
	variants
}

fn source_candidates_for_path(candidate: &Path) -> Vec<PathBuf> {
	const SOURCE_EXTENSIONS: &[&str] = &[".ts", ".tsx", ".mts", ".cts"];

	let mut paths = BTreeSet::new();
	let candidate_string = candidate.to_string_lossy();

	if let Some(name) = candidate.file_name().and_then(|value| value.to_str())
		&& SOURCE_EXTENSIONS.iter().any(|extension| name.ends_with(extension))
	{
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

fn entry_point_from_package_json(
	repository_root: &Path,
) -> Result<Option<PathBuf>, PackageError> {
	let manifest_path = repository_root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(None);
	}

	let content = fs::read_to_string(&manifest_path)?;
	let manifest: serde_json::Value = serde_json::from_str(&content)?;

	for key in ["types", "typings", "module", "main"] {
		if let Some(value) = manifest.get(key).and_then(serde_json::Value::as_str) {
			return Ok(Some(repository_root.join(value)));
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
		return Ok(Some(repository_root.join(value)));
	}

	Ok(None)
}
