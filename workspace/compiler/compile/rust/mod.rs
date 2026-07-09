//! Lowering Rust into the surface IR.
//!
//! Two producers share this module's public API:
//! - **rust-analyzer** (default): in-process `ra_ap_*` HIR walk → IR.
//! - **rustdoc** (temporary fallback): `cargo rustdoc --output-format json` →
//!   lower JSON. Opt in with `NUDOX_RUST_PRODUCER=rustdoc`; removed at P3.

pub mod context;
pub mod error;
pub mod function;
pub mod generics;
pub mod item;
pub mod package;
pub mod producer;
pub mod ra;
pub mod traversal;
pub mod types;

use std::path::Path;

use ir::entry::Index;
use rustc_hash::FxHashMap as HashMap;
use semver::Version;

pub use self::{
	error::{
		GenericError, ImplError, ItemError, MetadataError, Package, Parse, ProcessFailure,
		ProcessFailureKind, SignatureError, TypeResolutionError,
	},
	package::RustPackage,
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
/// `name` is the root package to document; `document_private` runs the
/// `--document-private-items` pass and pulls the workspace's local library
/// dependencies into the surface (the direct-repo behaviour).
///
/// Producer selection (temporary dual-path flag, removed at P3):
/// - unset / anything else → rust-analyzer (default)
/// - `NUDOX_RUST_PRODUCER=rustdoc` → legacy rustdoc path
pub fn generate_ir(
	root: &Path,
	name: &str,
	version: &Version,
	document_private: bool,
) -> std::result::Result<(Index, HashMap<String, String>), Package> {
	if ra::rustdoc_selected() {
		let package = RustPackage { name: name.to_string(), direct_repo: document_private };
		let (collected, source_map) = package.generate_ir_with_sources(root, version)?;
		return Ok((collected.index().into_index(), source_map));
	}

	ra::generate_ir(root, name, version, document_private)
}