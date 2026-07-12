//! The occurrences pipeline stage: parse every source file, run its
//! [`LanguageSpec`](crate::treesitter::spec::LanguageSpec) extractor, and
//! resolve + attribute the results against the surface index into an
//! [`OccurrenceSet`] (REFERENCES-PLAN §4.3).
//!
//! This is the surface-downstream successor to [`super::cst`]: where `cst`
//! produced unqualified [`ResolvedReference`](ir::syntax::ResolvedReference)
//! spans, this produces fully-qualified [`Occurrence`](ir::syntax::Occurrence)s
//! keyed by `NudoxPath` + span + confidence. The live trees are transient —
//! parsed, walked, resolved, and dropped.

use std::{fs, path::PathBuf};

use arborium_tree_sitter as tree_sitter;
use heart::Language;
use ir::{entry::Index, syntax::OccurrenceSet};

use crate::{
	error::GenerateError,
	generate::{resolve::resolve, PackageInput},
	treesitter::{
		spec::{Extraction, PackageLayout},
		spec_for,
	},
};

/// Map a language + file extension to an arborium grammar name (identical to
/// the [`super::cst`] dispatch; kept here so the two stages can diverge).
fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
	match lang {
		Language::Rust if ext == "rs" => Some("rust"),
		Language::Python if ext == "py" => Some("python"),
		Language::Typescript => match ext {
			"ts" | "mts" | "cts" => Some("typescript"),
			"tsx" => Some("tsx"),
			"js" => Some("javascript"),
			"jsx" => Some("tsx"),
			_ => None,
		},
		Language::Go if ext == "go" => Some("go"),
		Language::Java if ext == "java" => Some("java"),
		Language::Nix if ext == "nix" => Some("nix"),
		_ => None,
	}
}

/// Directory names that never hold first-party source (vendored deps, build
/// output, tool caches). Extends the `cst` skip list per REFERENCES-PLAN §5.
fn is_skipped_dir(name: &std::ffi::OsStr) -> bool {
	matches!(
		name.to_str(),
		Some(
			".git" | "target" | "node_modules" | "__pycache__" | "vendor" | ".venv" | "dist"
				| "build"
		)
	)
}

/// Walk the package source tree, extract each file, and resolve the whole
/// package into an [`OccurrenceSet`].
///
/// CPU-bound and deterministic. Runs on a blocking pool.
pub fn build(input: &PackageInput, surface: &Index) -> Result<OccurrenceSet, GenerateError> {
	let lang = input.coordinates.ecosystem();
	let layout = PackageLayout { package: input.coordinates.name.canonical().to_string() };
	let spec = spec_for(lang);

	let mut extractions: Vec<(PathBuf, Extraction)> = Vec::new();
	let mut pending = vec![input.root.clone()];

	while let Some(dir) = pending.pop() {
		for entry in fs::read_dir(&dir)? {
			let entry = entry?;
			let file_type = entry.file_type()?;
			let path = entry.path();

			if file_type.is_dir() {
				if !is_skipped_dir(&entry.file_name()) {
					pending.push(path);
				}
				continue;
			}
			if !file_type.is_file() {
				continue;
			}

			let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
				continue;
			};
			let Some(grammar_name) = grammar_for_extension(lang, ext) else {
				continue;
			};
			let Some(grammar) = arborium::get_language(grammar_name) else {
				continue;
			};

			let relative = path
				.strip_prefix(&input.root)
				.expect("walk stays under the package root")
				.to_path_buf();

			// Non-UTF-8 "source" files are not parseable code; skip them.
			let Ok(source) = fs::read_to_string(&path) else {
				tracing::warn!(path = %relative.display(), "skipping non-UTF-8 source file");
				continue;
			};

			let mut parser = tree_sitter::Parser::new();
			if parser.set_language(&grammar).is_err() {
				continue;
			}
			let Some(tree) = parser.parse(source.as_bytes(), None) else {
				tracing::warn!(path = %relative.display(), "skipping unparseable source file");
				continue;
			};

			let extraction = spec.extract(&tree, &source, &relative, &layout);
			extractions.push((relative, extraction));
		}
	}

	extractions.sort_by(|a, b| a.0.cmp(&b.0));
	Ok(resolve(lang, &extractions, surface))
}
