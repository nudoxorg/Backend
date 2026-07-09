//! Lowering Rust into the surface IR.
//!
//! The Rust producer is an in-process rust-analyzer (`ra_ap_*`) HIR walk that
//! lowers directly to [`Index`]. The legacy `cargo rustdoc --output-format
//! json` path was removed in RUST-ANALYZER-PLAN P3.

pub mod error;
pub mod producer;
pub mod ra;
pub mod traversal;

use std::path::Path;

use ir::entry::Index;
use rustc_hash::FxHashMap as HashMap;
use semver::Version;

pub use self::{
	error::{
		GenericError, ImplError, ItemError, MetadataError, Package, Parse, ProcessFailure,
		ProcessFailureKind, SignatureError, TypeResolutionError,
	},
	producer::RustProducer,
};

pub type Result<T> = std::result::Result<T, Parse>;

/// Convert an empty vec to `None`, wrapping a non-empty vec in `Some`.
pub(crate) fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
	if v.is_empty() { None } else { Some(v) }
}

/// Lower a materialized Rust package/workspace at `root` into the indexed API
/// surface plus the fq-name → source-text map for downstream tree-sitter
/// extraction.
///
/// `name` is the root package to document; `document_private` includes private
/// items and pulls the workspace's local library dependencies into the surface
/// (the direct-repo behaviour).
pub fn generate_ir(
	root: &Path,
	name: &str,
	version: &Version,
	document_private: bool,
) -> std::result::Result<(Index, HashMap<String, String>), Package> {
	ra::generate_ir(root, name, version, document_private)
}