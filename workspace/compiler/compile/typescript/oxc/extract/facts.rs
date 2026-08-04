//! The **data contract** between per-module extraction (pass 1) and cross-module
//! linking (pass 2).
//!
//! `ModuleFacts` is fully owned (no `oxc` types, no `'a` lifetimes) so it can
//! outlive the arena of the module it was extracted from. Pass 1 parses one
//! module into its own `Allocator`, extracts everything into a `ModuleFacts`,
//! then drops the `Program`/`Semantic`/`Allocator`. Pass 2 (`super::super::link`)
//! operates purely on `Vec<ModuleFacts>`: it assigns module names + paths,
//! resolves re-exports, runs the visibility BFS, and fills `type_links`.
//!
//! See OXC-PLAN.md §3.1 (facts-first two-pass) and OXC-PORT-SPEC.md §1 for the
//! IR shapes carried by `FactEntry::entry`.

use std::path::PathBuf;

use ir::kind::{Deprecation, Entry};

/// Everything pass 2 needs to know about one parsed module.
#[derive(Debug, Clone)]
pub struct ModuleFacts {
	/// Absolute path of the module file (the former deno `ModuleSpecifier`,
	/// now a plain path — no `file://` round-trip).
	pub specifier: PathBuf,

	/// The lowered IR fragments for every symbol/member in this module. Paths
	/// on the contained `Entry`s are *provisional* (module name not yet
	/// prepended); `link` finalizes them.
	pub entries: Vec<FactEntry>,

	/// `ModuleRecord`-derived export surface (re-exports resolved in pass 2).
	pub exports: ExportTable,

	/// `ModuleRecord`-derived import table (for import-then-export chains and
	/// symbol-accurate cross-module `type_links`).
	pub imports: Vec<ImportFact>,

	/// Module-level documentation (first `/**` block containing `@module`).
	pub module_doc: Option<String>,
}

/// One lowered IR entry plus the side metadata `link` needs to finalize it.
#[derive(Debug, Clone)]
pub struct FactEntry {
	/// The lowered entry. Its `Symbol.path` is a placeholder
	/// (`NudoxPath::Local("")`) until `link` assigns the real path; any
	/// `Function.type_links` are left `None` until `link` resolves `type_refs`.
	pub entry: Entry,

	/// Path segments *within* this module, e.g. `["MyClass"]` or
	/// `["MyClass", "method"]`. `link` prepends the assigned module name and
	/// hashes the result with `path_to_id`.
	pub local_path: Vec<String>,

	/// Unresolved type references appearing in this entry (function params /
	/// returns), used by `link` to build `Function.type_links`.
	pub type_refs: Vec<TypeRefFact>,
}

/// A single unresolved type reference and the `type_links` key it belongs under.
#[derive(Debug, Clone)]
pub struct TypeRefFact {
	/// The `parameter_link_key` (`"in.name"` / `"out.0"` / ...) this reference
	/// occupies in the owning `Function.type_links` map.
	pub link_key: String,

	/// What the reference points at, resolved to an entry id in pass 2.
	pub target: RefTarget,
}

/// The referent of a `TypeRefFact`, resolved against the linked module set.
#[derive(Debug, Clone)]
pub enum RefTarget {
	/// A name declared in the same module.
	Local(String),

	/// A name imported from another module (resolved through the import table).
	Imported { module_request: String, name: String },

	/// Truly global / ambient / unresolved — falls back to name-based matching.
	Unresolved(String),
}

/// The subset of an oxc `ModuleRecord` export surface that pass 2 consumes,
/// with every borrowed `Str<'a>` copied to an owned `String`.
#[derive(Debug, Clone, Default)]
pub struct ExportTable {
	/// `local_export_entries`: `export const x`, `export function f`, ...
	pub local: Vec<LocalExport>,

	/// `indirect_export_entries`: `export { x } from "mod"`, `export * as ns from "mod"`.
	pub indirect: Vec<IndirectExport>,

	/// `star_export_entries`: `export * from "mod"`.
	pub star: Vec<StarExport>,

	/// `export default` local binding name, if any.
	pub default: Option<String>,

	/// `exported_bindings` keys (names visible as exports of this module).
	pub exported_bindings: Vec<String>,
}

/// `export { local as export_name }` (no module request).
#[derive(Debug, Clone)]
pub struct LocalExport {
	pub export_name: String,
	pub local_name: String,
	pub is_type: bool,
}

/// `export { import_name as export_name } from module_request`
/// (or `export * as export_name from module_request` when `import_name` is `*`).
#[derive(Debug, Clone)]
pub struct IndirectExport {
	pub module_request: String,
	/// The name imported from `module_request` (`"*"` for `export * as ns`).
	pub import_name: String,
	pub export_name: String,
	pub is_type: bool,
}

/// `export * from module_request` (no alias — flattened into the re-exporter).
#[derive(Debug, Clone)]
pub struct StarExport {
	pub module_request: String,
	pub is_type: bool,
}

/// One `ImportEntry` from the module record, owned.
#[derive(Debug, Clone)]
pub struct ImportFact {
	pub module_request: String,
	pub import_name: ImportName,
	pub local_name: String,
	pub is_type: bool,
}

/// How an import binds the imported module (`ImportImportName` mirror).
#[derive(Debug, Clone)]
pub enum ImportName {
	/// `import { name }` / `import { name as local }`.
	Named(String),
	/// `import def`.
	Default,
	/// `import * as ns`.
	Namespace,
}

/// Parsed JSDoc for a single node: the doc text plus the tag-derived signals
/// the lowering acts on (`@ignore` suppression, `@deprecated` → IR deprecation).
#[derive(Debug, Clone, Default)]
pub struct DocFacts {
	pub doc: Option<String>,
	pub deprecation: Option<Deprecation>,
	pub ignore: bool,
}
