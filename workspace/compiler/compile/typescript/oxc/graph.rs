//! Module-graph construction + pass-1 extraction driver (OXC-PLAN §Phase 1.2).
//!
//! Worklist from the entry roots. Per module: parse (`preserve_parens: false`,
//! `SourceType::from_path` with `.d_ts()` fallback), build semantic, extract to
//! an owned [`ModuleFacts`] (dropping the arena), then enqueue resolved edges
//! (`requested_modules` ∪ triple-slash refs, resolved via `resolve_dts`).
//! Errors are classified: `NotFound` → external edge, `Builtin` → node builtin,
//! others → diagnostic.
//!
//! LEAF FILE — fill the `todo!()` body. `extract::extract_module` and the whole
//! `ModuleFacts` contract are already frozen; this owns parse + resolve + the
//! arena lifecycle only.

// Leaf implementation pulls these in: oxc_allocator::Allocator,
// oxc_parser::{ParseOptions, Parser}, oxc_semantic::SemanticBuilder,
// oxc_span::SourceType, super::entry, super::extract::extract_module.
use std::path::PathBuf;

use super::{error::Package, extract::ModuleFacts};

/// Build the reachable module graph from `roots` and extract each module into
/// owned [`ModuleFacts`]. One `Allocator` per module (reset/dropped after
/// extraction) keeps `Allocator: !Sync` from leaking across modules.
///
/// Per module: `Parser::new(&alloc, src, SourceType::from_path(p).unwrap_or_else(
/// |_| SourceType::d_ts())).with_options(ParseOptions { preserve_parens: false,
/// ..Default::default() }).parse()`, then `SemanticBuilder::new()
/// .with_check_syntax_error(false).with_build_nodes(true).build(&program)`,
/// then `extract::extract_module(...)`. Edges = `module_record.requested_modules`
/// ∪ triple-slash refs, resolved via `entry::make_resolver().resolve_dts(dir,
/// spec)`; classify `NotFound`/`Builtin`/other.
pub(crate) fn build_and_extract(roots: Vec<PathBuf>) -> Result<Vec<ModuleFacts>, Package> {
	let _ = roots;
	todo!("graph.rs: worklist parse → semantic → extract → resolve edges")
}
