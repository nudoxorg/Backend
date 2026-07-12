//! Full-OXC TypeScript producer pipeline (OXC-PLAN.md).
//!
//! Replaces the deno_doc/deno_graph/swc stack with a hand-written extractor on
//! the `oxc` crates. Three stages, facts-first two-pass (OXC-PLAN §3):
//!   1. [`entry`] — resolver-first entry / declaration-root discovery.
//!   2. [`graph`] — module-graph worklist + pass-1 per-module [`extract`]ion to
//!      owned [`extract::ModuleFacts`] (arena dropped per module).
//!   3. [`link`]  — pass-2 cross-module resolution → `ir::entry::Index`.
//!
//! Coexists with the legacy deno files during migration; cutover (OXC-PLAN
//! §Phase 4) deletes those and promotes this module up.

use std::path::Path;

use ir::entry::Index;

pub mod entry;
pub mod error;
pub mod extract;
pub mod graph;
pub mod link;

pub use error::{Package, Parse, TsDeclarationError, TsInterfaceError, TsTypeError};

/// Lower a materialized TypeScript package at `root` into the indexed API
/// surface, using the OXC pipeline.
///
/// `name` is the package name as its manifest reports it. Synchronous end to
/// end (no tokio / deno_graph futures).
pub fn generate_ir(root: &Path, name: &str) -> Result<Index, Package> {
	let roots = entry::discover_entry_points(root)?;
	let modules = graph::build_and_extract(roots)?;
	link::link(modules, name)
}
