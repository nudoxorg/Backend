//! Lowering Rust into the surface IR via the `rustdoc-driver` in-process path.
//!
//! Resolves documented local/workspace packages, runs `cargo rustdoc` with
//! `RUSTDOC=<rustdoc-driver>`, and reads back the IR JSON the driver writes.

pub use rust_lowering::{context, error, function, generics, item, types};
pub use rust_lowering::{empty_to_none, Result};
pub use rust_lowering::error::{
    GenericError, ImplError, ItemError, MetadataError, Package, Parse, ProcessFailure,
    ProcessFailureKind, SignatureError, TypeResolutionError,
};
pub use self::package::RustPackage;

pub mod package;
pub mod traversal;

use std::path::Path;

use ir::entry::Index;
use rustc_hash::FxHashMap as HashMap;
use semver::Version;

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
