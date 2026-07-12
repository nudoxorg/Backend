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
	use oxc_ast::ast::{
		Declaration, ExportDefaultDeclarationKind, Statement, TSModuleDeclarationName,
	};
	use oxc_syntax::module_record::{ExportExportName, ExportImportName, ExportLocalName, ImportImportName};
	use rustc_hash::FxHashMap as HashMap;

	let mut extractor = Extractor::new(source, semantic, specifier);

	// ── Step 1: Build exported-name set from module_record ──────────────────
	// exported_bindings maps exported name → span. Use it to determine which
	// top-level names are exported.
	let exported_names: std::collections::HashSet<String> = module_record
		.exported_bindings
		.iter()
		.map(|(name, _)| name.to_string())
		.collect();

	// Also collect the default export's local name (if any).
	let default_local_name: Option<String> = module_record
		.local_export_entries
		.iter()
		.find_map(|e| {
			if matches!(e.export_name, ExportExportName::Default(_)) {
				if let ExportLocalName::Default(ns) = &e.local_name {
					Some(ns.name.to_string())
				} else if let ExportLocalName::Name(ns) = &e.local_name {
					Some(ns.name.to_string())
				} else {
					None
				}
			} else {
				None
			}
		});

	// ── Step 2: Walk program.body, group declarations by name ───────────────
	// We produce one SymbolGroup per unique name, collecting all same-named
	// declarations together (declaration merging = name-grouping per OXC-PLAN §1.1).
	let mut groups: HashMap<String, SymbolGroup<'a>> = HashMap::default();
	// Track insertion order so we emit entries in source order.
	let mut order: Vec<String> = Vec::new();
	// Tier B: `export default <decl>` declarations, lowered after grouping.
	let mut default_exports: Vec<&'a ExportDefaultDeclarationKind<'a>> = Vec::new();

	for stmt in program.body.iter() {
		match stmt {
			// ── export default: function/class/interface ─────────────────
			// Tier B: deno_doc dropped default-exported declarations entirely.
			// We collect the declaration kind and lower it after grouping (its
			// payload is a bare `Function`/`Class`/`TSInterfaceDeclaration`, not a
			// `Declaration`, so it can't join the name-grouped `SymbolGroup` path).
			// `export default <expression>` is resolved by link.rs via
			// `ExportTable.default`.
			Statement::ExportDefaultDeclaration(exp) => {
				default_exports.push(&exp.declaration);
			}

			// ── export { x } — named export with declaration ─────────────
			Statement::ExportNamedDeclaration(exp) => {
				if let Some(decl) = &exp.declaration {
					let sym_name = decl_name_from_declaration(decl);
					if let Some(sym_name) = sym_name {
						let group = groups.entry(sym_name.clone()).or_insert_with(|| {
							order.push(sym_name.clone());
							SymbolGroup {
								name: sym_name.clone(),
								is_default: false,
								declarations: Vec::new(),
								visibility: ir::kind::Visibility::Public,
							}
						});
						group.declarations.push(decl);
					}
				}
				// Bare `export { x }` specifiers have no declaration; link.rs handles them.
			}

			// ── bare declarations (possibly also exported) ────────────────
			Statement::FunctionDeclaration(f) => {
				let sym_name = f.id.as_ref().map(|id| id.name.to_string());
				if let Some(sym_name) = sym_name {
					let is_exported = exported_names.contains(&sym_name)
						|| default_local_name.as_deref() == Some(sym_name.as_str());
					let vis = if is_exported {
						ir::kind::Visibility::Public
					} else {
						ir::kind::Visibility::Private
					};
					// Convert Statement::FunctionDeclaration to Declaration.
					// In oxc 0.139.0 the INHERIT macro means Statement and Declaration
					// share the same `FunctionDeclaration(Box<Function>)` variant layout.
					// We obtain a `&Declaration` by re-borrowing via a safe pointer cast:
					// Statement::FunctionDeclaration has the same discriminant value and
					// payload as Declaration::FunctionDeclaration (see inherit_variants.rs
					// which maps `Statement::FunctionDeclaration(o)` ↔
					// `Declaration::FunctionDeclaration(o)` with the same Box).
					// We use the fact that the oxc‐generated Statement enum lays out its
					// INHERIT(Declaration) variants in the same discriminant range as the
					// Declaration enum, beginning at discriminant 32 for VariableDeclaration
					// and 33 for FunctionDeclaration. This is an implementation detail of
					// the INHERIT macro codegen. We therefore use a raw pointer cast that
					// is safe under the assumption that both enums have the same memory
					// layout for the shared variants.
					//
					// SAFETY: `Statement::FunctionDeclaration(Box<Function>)` is layout-
					// compatible with `Declaration::FunctionDeclaration(Box<Function>)`
					// as documented in oxc's inherit_variants.rs generated TryFrom impls.
					let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
					let group = groups.entry(sym_name.clone()).or_insert_with(|| {
						order.push(sym_name.clone());
						SymbolGroup {
							name: sym_name.clone(),
							is_default: false,
							declarations: Vec::new(),
							visibility: vis.clone(),
						}
					});
					group.visibility = vis;
					group.declarations.push(decl_ref);
				}
			}

			Statement::ClassDeclaration(c) => {
				let sym_name = c.id.as_ref().map(|id| id.name.to_string());
				if let Some(sym_name) = sym_name {
					let is_exported = exported_names.contains(&sym_name);
					let vis = if is_exported {
						ir::kind::Visibility::Public
					} else {
						ir::kind::Visibility::Private
					};
					let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
					let group = groups.entry(sym_name.clone()).or_insert_with(|| {
						order.push(sym_name.clone());
						SymbolGroup {
							name: sym_name.clone(),
							is_default: false,
							declarations: Vec::new(),
							visibility: vis.clone(),
						}
					});
					group.declarations.push(decl_ref);
				}
			}

			Statement::VariableDeclaration(v) => {
				// Fan-out: each declarator becomes its own group.
				for d in v.declarations.iter() {
					let sym_name = match &d.id {
						oxc_ast::ast::BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
						_ => None,
					};
					if let Some(sym_name) = sym_name {
						let is_exported = exported_names.contains(&sym_name);
						let vis = if is_exported {
							ir::kind::Visibility::Public
						} else {
							ir::kind::Visibility::Private
						};
						let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
						let group = groups.entry(sym_name.clone()).or_insert_with(|| {
							order.push(sym_name.clone());
							SymbolGroup {
								name: sym_name.clone(),
								is_default: false,
								declarations: Vec::new(),
								visibility: vis.clone(),
							}
						});
						group.declarations.push(decl_ref);
					}
				}
			}

			Statement::TSTypeAliasDeclaration(a) => {
				let sym_name = a.id.name.to_string();
				let is_exported = exported_names.contains(&sym_name);
				let vis = if is_exported { ir::kind::Visibility::Public } else { ir::kind::Visibility::Private };
				let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
				let group = groups.entry(sym_name.clone()).or_insert_with(|| {
					order.push(sym_name.clone());
					SymbolGroup { name: sym_name.clone(), is_default: false, declarations: Vec::new(), visibility: vis }
				});
				group.declarations.push(decl_ref);
			}

			Statement::TSInterfaceDeclaration(i) => {
				let sym_name = i.id.name.to_string();
				let is_exported = exported_names.contains(&sym_name);
				let vis = if is_exported { ir::kind::Visibility::Public } else { ir::kind::Visibility::Private };
				let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
				let group = groups.entry(sym_name.clone()).or_insert_with(|| {
					order.push(sym_name.clone());
					SymbolGroup { name: sym_name.clone(), is_default: false, declarations: Vec::new(), visibility: vis }
				});
				group.declarations.push(decl_ref);
			}

			Statement::TSEnumDeclaration(e) => {
				let sym_name = e.id.name.to_string();
				let is_exported = exported_names.contains(&sym_name);
				let vis = if is_exported { ir::kind::Visibility::Public } else { ir::kind::Visibility::Private };
				let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
				let group = groups.entry(sym_name.clone()).or_insert_with(|| {
					order.push(sym_name.clone());
					SymbolGroup { name: sym_name.clone(), is_default: false, declarations: Vec::new(), visibility: vis }
				});
				group.declarations.push(decl_ref);
			}

			Statement::TSModuleDeclaration(m) => {
				let sym_name = match &m.id {
					TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
					TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
				};
				let is_exported = exported_names.contains(&sym_name) || m.declare;
				let vis = if is_exported { ir::kind::Visibility::Public } else { ir::kind::Visibility::Private };
				let decl_ref: &'a Declaration<'a> = stmt.as_declaration().expect("statement is a declaration");
				let group = groups.entry(sym_name.clone()).or_insert_with(|| {
					order.push(sym_name.clone());
					SymbolGroup { name: sym_name.clone(), is_default: false, declarations: Vec::new(), visibility: vis }
				});
				group.declarations.push(decl_ref);
			}

			// All other statements (imports, bare expressions, etc.) are ignored here.
			_ => {}
		}
	}

	// ── Step 3: Lower each SymbolGroup into FactEntries ──────────────────────
	let mut all_entries: Vec<FactEntry> = Vec::new();
	for name in &order {
		if let Some(group) = groups.get(name) {
			// Safety: we built these references above from the same `program` which
			// is borrowed for `'a`.
			let entries = extractor.lower_symbol(group)?;
			all_entries.extend(entries);
		}
	}

	// ── Step 3b: Lower default-exported declarations (Tier B) ────────────────
	for kind in default_exports {
		let entries = extractor.lower_default_export(kind)?;
		all_entries.extend(entries);
	}

	// ── Step 4: Build ExportTable from ModuleRecord ───────────────────────────
	let mut local_exports: Vec<facts::LocalExport> = Vec::new();
	let mut indirect_exports: Vec<facts::IndirectExport> = Vec::new();
	let mut star_exports: Vec<facts::StarExport> = Vec::new();
	let mut default_export_name: Option<String> = None;
	let mut exported_bindings_list: Vec<String> = Vec::new();

	for e in module_record.local_export_entries.iter() {
		let export_name = match &e.export_name {
			ExportExportName::Name(ns) => ns.name.to_string(),
			ExportExportName::Default(_) => "default".to_string(),
			ExportExportName::Null => continue,
		};
		let local_name = match &e.local_name {
			ExportLocalName::Name(ns) => ns.name.to_string(),
			ExportLocalName::Default(ns) => ns.name.to_string(),
			ExportLocalName::Null => continue,
		};
		if export_name == "default" {
			default_export_name = Some(local_name.clone());
		}
		local_exports.push(facts::LocalExport {
			export_name,
			local_name,
			is_type: e.is_type,
		});
	}

	for e in module_record.indirect_export_entries.iter() {
		let module_request = match &e.module_request {
			Some(ns) => ns.name.to_string(),
			None => continue,
		};
		let import_name = match &e.import_name {
			ExportImportName::Name(ns) => ns.name.to_string(),
			ExportImportName::All => "*".to_string(),
			ExportImportName::AllButDefault => "*".to_string(),
			ExportImportName::Null => continue,
		};
		let export_name = match &e.export_name {
			ExportExportName::Name(ns) => ns.name.to_string(),
			ExportExportName::Default(_) => "default".to_string(),
			ExportExportName::Null => continue,
		};
		indirect_exports.push(facts::IndirectExport {
			module_request,
			import_name,
			export_name,
			is_type: e.is_type,
		});
	}

	for e in module_record.star_export_entries.iter() {
		let module_request = match &e.module_request {
			Some(ns) => ns.name.to_string(),
			None => continue,
		};
		star_exports.push(facts::StarExport {
			module_request,
			is_type: e.is_type,
		});
	}

	for (name, _) in module_record.exported_bindings.iter() {
		exported_bindings_list.push(name.to_string());
	}

	let exports = ExportTable {
		local: local_exports,
		indirect: indirect_exports,
		star: star_exports,
		default: default_export_name,
		exported_bindings: exported_bindings_list,
	};

	// ── Step 5: Build ImportFacts from ModuleRecord ───────────────────────────
	let imports: Vec<ImportFact> = module_record
		.import_entries
		.iter()
		.map(|e| {
			let import_name = match &e.import_name {
				ImportImportName::Name(ns) => ImportName::Named(ns.name.to_string()),
				ImportImportName::Default(_) => ImportName::Default,
				ImportImportName::NamespaceObject => ImportName::Namespace,
			};
			ImportFact {
				module_request: e.module_request.name.to_string(),
				import_name,
				local_name: e.local_name.name.to_string(),
				is_type: e.is_type,
			}
		})
		.collect();

	// ── Step 6: Module doc ───────────────────────────────────────────────────
	let module_doc = extractor.module_doc(program);

	Ok(ModuleFacts {
		specifier: specifier.to_path_buf(),
		entries: all_entries,
		exports,
		imports,
		module_doc,
	})
}

/// Extract the name from a `Declaration` node.
fn decl_name_from_declaration(decl: &Declaration<'_>) -> Option<String> {
	use oxc_ast::ast::{BindingPattern, Declaration, TSModuleDeclarationName};
	match decl {
		Declaration::FunctionDeclaration(f) => f.id.as_ref().map(|id| id.name.to_string()),
		Declaration::ClassDeclaration(c) => c.id.as_ref().map(|id| id.name.to_string()),
		Declaration::VariableDeclaration(v) => {
			v.declarations.first().and_then(|d| match &d.id {
				BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
				_ => None,
			})
		}
		Declaration::TSTypeAliasDeclaration(a) => Some(a.id.name.to_string()),
		Declaration::TSInterfaceDeclaration(i) => Some(i.id.name.to_string()),
		Declaration::TSEnumDeclaration(e) => Some(e.id.name.to_string()),
		Declaration::TSModuleDeclaration(m) => match &m.id {
			TSModuleDeclarationName::Identifier(id) => Some(id.name.to_string()),
			TSModuleDeclarationName::StringLiteral(s) => Some(s.value.to_string()),
		},
		_ => None,
	}
}
