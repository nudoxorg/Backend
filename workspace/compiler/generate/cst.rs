//! Generating the concrete-syntax-tree resolution (tree-sitter) for a package.
//!
//! The live tree-sitter `Tree` is C-allocated and non-serializable, and one tree
//! per package is the wrong granularity. So the CST resolution carried forward is
//! *per-file* and reduced to the serializable data the rest of the system needs:
//! the [`ResolvedReference`] spans that link identifiers to IR entries. The tree
//! itself is transient — parsed, walked for references, and dropped.

use std::{fs, path::PathBuf};

use arborium_tree_sitter as tree_sitter;
use heart::Language;
use ir::syntax::{walk_references, ParseError, ResolvedReference};

use crate::{error::GenerateError, generate::PackageInput, treesitter::classify_for};

/// One source file's CST resolution: the resolved reference spans extracted from
/// it. Serializable and self-contained — no live tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Cst {
	/// The file, relative to the package root.
	pub path: PathBuf,
	/// The references resolved within it, in source order.
	pub references: Vec<ResolvedReference>,
}

/// The per-file CST resolution for a whole package.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CstSet {
	/// One entry per parsed source file, sorted by path for reproducibility.
	pub files: Vec<Cst>,
}

/// Map a language + file extension to an arborium grammar name.
///
/// Returns `None` when the extension is not a source file for that language.
fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
	match lang {
		Language::Rust if ext == "rs" => Some("rust"),
		Language::Python if ext == "py" => Some("python"),
		Language::Typescript => match ext {
			"ts" | "mts" | "cts" => Some("typescript"),
			"tsx" => Some("tsx"),
			// Optional JS companions of a TypeScript package.
			"js" => Some("javascript"),
			"jsx" => Some("tsx"),
			_ => None,
		},
		Language::Go if ext == "go" => Some("go"),
		Language::Java if ext == "java" => Some("java"),
		Language::Nix if ext == "nix" => Some("nix"),
		Language::CSharp if ext == "cs" => Some("c-sharp"),
		_ => None,
	}
}

/// Parse each source file, walk it for references, and collect the serializable
/// spans. The transient trees are dropped as we go, so memory stays bounded.
///
/// Runs on a blocking pool (tree-sitter parsing is CPU-bound).
///
/// Missing grammars yield an empty (but well-formed) resolution rather than an
/// error, mirroring the treesitter module's graceful-fallback contract.
pub fn extract(input: &PackageInput) -> Result<CstSet, GenerateError> {
	let lang = input.coordinates.ecosystem();

	let mut files = Vec::new();
	let mut pending = vec![input.root.clone()];

	while let Some(dir) = pending.pop() {
		for entry in fs::read_dir(&dir)? {
			let entry = entry?;
			let file_type = entry.file_type()?;
			let path = entry.path();

			if file_type.is_dir() {
				let name = entry.file_name();
				// Skip VCS metadata and toolchain/build output dirs.
				if name != ".git" && name != "target" && name != "node_modules" && name != "__pycache__" {
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
			// Skip files whose grammar isn't registered rather than failing the package.
			let Some(grammar) = arborium::get_language(grammar_name) else {
				continue;
			};

			let relative = path
				.strip_prefix(&input.root)
				.expect("walk stays under the package root")
				.to_path_buf();

			// Non-UTF-8 "source" files are not parseable code; skip rather
			// than fail the package.
			let Ok(source) = fs::read_to_string(&path) else {
				tracing::warn!(path = %relative.display(), "skipping non-UTF-8 source file");
				continue;
			};

			let references =
				resolve_file(&source, &grammar, &relative.display().to_string(), lang).map_err(
					|source| GenerateError::Cst {
						path: relative.display().to_string(),
						source,
					},
				)?;
			files.push(Cst {
				path: relative,
				references,
			});
		}
	}

	files.sort_by(|a, b| a.path.cmp(&b.path));
	Ok(CstSet { files })
}

/// Parse one file with a transient tree, walk it for references, and keep only
/// the serializable spans — the C-allocated tree is dropped on return.
/// The `path` is carried into the error variants so LanguageError / parse
/// failures preserve the originating source file in the error chain.
fn resolve_file(
	source: &str,
	grammar: &tree_sitter::Language,
	path: &str,
	lang: Language,
) -> Result<Vec<ResolvedReference>, ParseError> {
	let mut parser = tree_sitter::Parser::new();
	parser.set_language(grammar).map_err(|e| ParseError::LanguageWithSource {
		path:   path.to_owned(),
		source: e,
	})?;
	let tree = parser
		.parse(source.as_bytes(), None)
		.ok_or_else(|| ParseError::ParseWithSource { path: path.to_owned() })?;

	// Dispatch by language so TSX/JS files still use the TypeScript classifier
	// (shared identifier / call / import node kinds).
	Ok(walk_references(&tree, source, classify_for(lang)))
}
