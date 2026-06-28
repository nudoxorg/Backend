use std::{collections::BTreeSet, fs, path::{Path, PathBuf}};

use crate::parse::typescript::Package;
use crate::backends::error::Error;

pub const TYPESCRIPT_REPOSITORY_ENTRY_PREFIX: &str = "repo:";

pub fn typescript_package_uses_repository(package: &Package) -> bool {
	package.entry_point.starts_with(TYPESCRIPT_REPOSITORY_ENTRY_PREFIX)
}

pub(crate) fn typescript_repository_entry_hint(package: &Package) -> Option<&str> {
	package
		.entry_point
		.strip_prefix(TYPESCRIPT_REPOSITORY_ENTRY_PREFIX)
		.filter(|entry_point| !entry_point.is_empty())
}

pub fn typescript_slug(name: &str) -> String { name.to_ascii_lowercase().replace('/', "__") }

pub(crate) fn resolve_typescript_repository_entry_point(
	repository_root: &Path,
	entry_hint: Option<&str>,
) -> Result<PathBuf, Error> {
	if let Some(entry_hint) = entry_hint {
		let candidate = repository_root.join(entry_hint);
		return ensure_typescript_entry_point(repository_root, candidate);
	}

	if let Some(candidate) = read_typescript_entry_point_from_package_json(repository_root)? {
		return ensure_typescript_entry_point(repository_root, candidate);
	}

	for candidate in ["mod.ts", "index.ts", "src/mod.ts", "src/index.ts"] {
		let candidate = repository_root.join(candidate);
		if candidate.is_file() {
			return Ok(candidate);
		}
	}

	Err(Error::TypescriptEntryPointDiscovery {
		path: repository_root.to_string_lossy().into_owned(),
	})
}

fn ensure_typescript_entry_point(
	repository_root: &Path,
	candidate: PathBuf,
) -> Result<PathBuf, Error> {
	if candidate.is_file() {
		return Ok(candidate);
	}

	for fallback in typescript_source_fallback_candidates(repository_root, &candidate) {
		if fallback.is_file() {
			return Ok(fallback);
		}
	}

	Err(Error::TypescriptEntryPointMissing { path: candidate.to_string_lossy().into_owned() })
}

fn typescript_source_fallback_candidates(repository_root: &Path, candidate: &Path) -> Vec<PathBuf> {
	let Ok(relative) = candidate.strip_prefix(repository_root) else {
		return Vec::new();
	};

	let variants = typescript_relative_variants_without_build_prefixes(relative);
	let mut fallbacks = BTreeSet::new();
	for variant in variants {
		for path in typescript_source_candidates_for_path(&repository_root.join(&variant)) {
			fallbacks.insert(path);
		}
	}
	fallbacks.into_iter().collect()
}

fn typescript_relative_variants_without_build_prefixes(relative: &Path) -> Vec<PathBuf> {
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

fn typescript_source_candidates_for_path(candidate: &Path) -> Vec<PathBuf> {
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
	if let Some(stripped) = strip_typescript_known_suffix(&candidate_string) {
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

fn strip_typescript_known_suffix(path: &str) -> Option<String> {
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

fn read_typescript_entry_point_from_package_json(
	repository_root: &Path,
) -> Result<Option<PathBuf>, Error> {
	let manifest_path = repository_root.join("package.json");
	if !manifest_path.is_file() {
		return Ok(None);
	}

	let content = fs::read_to_string(&manifest_path)
		.map_err(|source| Error::Storage { path: manifest_path.clone(), source })?;
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
