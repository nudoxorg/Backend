//! Lowering TypeScript into the surface IR via the OXC pipeline.
//!
//! Entry discovery, module-graph construction, per-module extraction, and
//! cross-module linking all live in [`oxc`].

use std::path::Path;

use ir::entry::Index;

pub mod oracle;
pub mod oxc;
pub mod producer;

pub use self::{
	oxc::error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError},
	producer::TypescriptProducer,
};

/// Lower a materialized TypeScript package at `root` into the indexed API
/// surface, using the OXC pipeline. `name` is the package name from its manifest.
pub fn generate_ir(root: &Path, name: &str) -> Result<Index, Package> {
	oxc::generate_ir(root, name)
}
