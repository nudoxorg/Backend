//! Generating the concrete-syntax-tree resolution (tree-sitter) for a package.
//!
//! The live tree-sitter `Tree` is C-allocated and non-serializable, and one tree
//! per package is the wrong granularity. So the CST resolution carried forward is
//! *per-file* and reduced to the serializable data the rest of the system needs:
//! the [`ResolvedReference`] spans that link identifiers to IR entries. The tree
//! itself is transient — parsed, walked for references, and dropped.

use std::path::PathBuf;

use ir::syntax::ResolvedReference;

use crate::{error::GenerateError, generate::PackageInput};

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
	let _ = input;
	todo!("per file: parse via ir::syntax, walk_references, keep spans, drop the tree")
}
