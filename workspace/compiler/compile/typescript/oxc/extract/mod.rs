//! Pass 1: per-module extraction. Lowers one parsed module's `oxc_ast` +
//! `oxc_semantic` directly into owned `ir::*` fragments (`ModuleFacts`), then
//! the caller (`super::graph`) drops the AST/arena.
//!
//! The lowering is split across sibling files, each contributing an
//! `impl<'a> Extractor<'a>` block so leaf work stays file-local:
//!   * [`types`]  — `TSType` → `ir::ty::Type`               (OXC-PLAN §5.3)
//!   * [`infer`]  — `Expression` → inferred `Type`          (OXC-PLAN §1.2)
//!   * [`func`]   — functions / methods / params / overloads
//!   * [`jsdoc`]  — `oxc_jsdoc` → [`DocFacts`] + module doc
//!   * [`decl`]   — declarations → `ir::kind::Entry` (+ members)

use std::path::{Path, PathBuf};

use oxc_ast::ast::{Declaration, Program};
use oxc_semantic::Semantic;
use oxc_syntax::module_record::ModuleRecord;

use super::error::Parse;

pub mod decl;
pub mod facts;
pub mod func;
pub mod infer;
pub mod jsdoc;
pub mod types;

pub use facts::*;

/// Module-local result alias — every extraction step funnels through [`Parse`].
pub type Result<T> = std::result::Result<T, Parse>;

/// A single top-level symbol, i.e. all same-named declarations grouped together
/// (declaration merging is pure name-grouping — OXC-PLAN §1.1). Mirrors the
/// patched deno_doc `Symbol { declarations }` shape the lowering expects.
pub struct SymbolGroup<'a> {
	/// The exported/declared name.
	pub name: String,

	/// `true` when this is the `export default` symbol.
	pub is_default: bool,

	/// Every declaration that shares `name`, in source order. Overload
	/// signatures (`body: None`) precede the implementation (`body: Some`).
	pub declarations: Vec<&'a Declaration<'a>>,

	/// Visibility tag derived from export/ambient status
	/// (`Public` for exported, `Private` for non-exported, `Public` for ambient
	/// `declare` — mirrors `declaration_kind_to_visibility`).
	pub visibility: ir::kind::Visibility,
}

/// Per-module extraction state. Borrows everything from the module's arena
/// (`'a`); produces owned [`ModuleFacts`].
pub struct Extractor<'a> {
	/// The module's source text (for span slicing: decorators, patterns, ...).
	pub source: &'a str,

	/// Semantic model: scoping (symbol-accurate resolution), nodes, jsdoc.
	pub semantic: &'a Semantic<'a>,

	/// The module file path (owned; cheap and avoids threading `'a`).
	pub specifier: PathBuf,

	/// Accumulator for the type references discovered while lowering the
	/// current function's params/return, drained into each `FactEntry`.
	pub type_ref_scratch: Vec<TypeRefFact>,
}

impl<'a> Extractor<'a> {
	/// Construct a per-module extractor.
	pub fn new(source: &'a str, semantic: &'a Semantic<'a>, specifier: &Path) -> Self {
		Self {
			source,
			semantic,
			specifier: specifier.to_path_buf(),
			type_ref_scratch: Vec::new(),
		}
	}
}

/// Pass-1 entry point: lower one module into owned [`ModuleFacts`].
///
/// Groups top-level declarations by exported name into [`SymbolGroup`]s, lowers
/// each via [`Extractor::lower_symbol`], and builds the export/import tables and
/// module doc from `module_record` / `program.comments`.
pub fn extract_module<'a>(
	source: &'a str,
	semantic: &'a Semantic<'a>,
	program: &'a Program<'a>,
	specifier: &Path,
	module_record: &ModuleRecord<'a>,
) -> Result<ModuleFacts> {
	let _ = (source, semantic, program, specifier, module_record);
	todo!("extract/mod.rs: group declarations → SymbolGroup, lower, build tables")
}
