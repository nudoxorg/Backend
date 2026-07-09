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

use crate::{error::GenerateError, generate::PackageInput, treesitter::classify_rust};

/// One source file's CST resolution: the resolved reference spans extracted from
/// it. Serializable and self-contained — no live tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cst {
    /// The file, relative to the package root.
    pub path: PathBuf,
    /// The references resolved within it, in source order.
    pub references: Vec<ResolvedReference>,
}

/// The per-file CST resolution for a whole package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CstSet {
    /// One entry per parsed source file, sorted by path for reproducibility.
    pub files: Vec<Cst>,
}

/// Parse each source file, walk it for references, and collect the serializable
/// spans. The transient trees are dropped as we go, so memory stays bounded.
///
/// Runs on a blocking pool (tree-sitter parsing is CPU-bound).
pub fn extract(input: &PackageInput) -> Result<CstSet, GenerateError> {
    // Only Rust has a registered grammar today; other ecosystems produce an
    // empty (but well-formed) resolution rather than an error, mirroring the
    // treesitter module's graceful-fallback contract.
    let (grammar, extension) = match input.coordinates.ecosystem() {
        Language::Rust => match arborium::get_language("rust") {
            Some(grammar) => (grammar, "rs"),
            None => return Ok(CstSet::default()),
        },
        Language::Typescript | Language::Python | Language::Go | Language::Java | Language::Nix => {
            return Ok(CstSet::default());
        }
    };

    let mut files = Vec::new();
    let mut pending = vec![input.root.clone()];

    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let path = entry.path();

            if file_type.is_dir() {
                if entry.file_name() != ".git" {
                    pending.push(path);
                }
                continue;
            }
            if !file_type.is_file() || path.extension().and_then(|e| e.to_str()) != Some(extension)
            {
                continue;
            }

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
                resolve_file(&source, &grammar, &relative.display().to_string())
                    .map_err(|source| GenerateError::Cst {
                        path: relative.display().to_string(),
                        source,
                    })?;
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
) -> Result<Vec<ResolvedReference>, ParseError> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(grammar).map_err(|e| ParseError::LanguageWithSource {
        path:   path.to_owned(),
        source: e,
    })?;
    let tree = parser
        .parse(source.as_bytes(), None)
        .ok_or_else(|| ParseError::ParseWithSource { path: path.to_owned() })?;
    Ok(walk_references(&tree, source, classify_rust))
}
