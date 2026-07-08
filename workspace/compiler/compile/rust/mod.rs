//! Lowering Rust into the surface IR via `cargo rustdoc`'s JSON output.
//!
//! Resolves documented local/workspace packages, reads the produced
//! `target/doc/{crate}.json`, and produces an `ir::Index` plus a fq-name →
//! source-text map for downstream tree-sitter extraction.

pub mod context;
pub mod error;
pub mod function;
pub mod generics;
pub mod item;
pub mod package;
pub mod traversal;
pub mod types;

use std::path::Path;

use ir::entry::Index;
use rustc_hash::FxHashMap as HashMap;
use semver::Version;

pub use self::{error::{Package, Parse}, package::RustPackage};

pub type Result<T> = std::result::Result<T, Parse>;

/// Convert an empty vec to `None`, wrapping a non-empty vec in `Some`.
pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}

/// Lower a materialized Rust package/workspace at `root` into the indexed API
/// surface plus the fq-name → source-text map for downstream tree-sitter
/// extraction.
///
/// `name` is the root package to document; `document_private` runs the
/// `--document-private-items` pass and pulls the workspace's local library
/// dependencies into the surface (the direct-repo behaviour).
pub fn generate_ir(
	root: &Path,
	name: &str,
	version: &Version,
	document_private: bool,
) -> std::result::Result<(Index, HashMap<String, String>), Package> {
	let package = RustPackage { name: name.to_string(), direct_repo: document_private };
	let (collected, source_map) = package.generate_ir_with_sources(root, version)?;
	Ok((collected.index().into_index(), source_map))
}
