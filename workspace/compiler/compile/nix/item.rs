//! Lowering the static Nix table into IR entries.
//!
//! This module converts [`syntax::StaticTable`] bindings into
//! `(NudoxPath, Entry)` pairs that form the skeleton of the flake's IR
//! index.  All entries start with [`ir::kind::Visibility::Private`]; the
//! dynamic layer promotes anything reachable from `outputs` to
//! [`ir::kind::Visibility::Public`]. When no dynamic surface is produced,
//! [`super::context`] promotes top-level export roots to Public.
//!
//! Responsibilities of *this* module:
//!
//! * Building the `NudoxPath` scheme for Nix (`a/b/c` attrpath segments).
//! * Emitting one root `Entry::Module` for the flake.
//! * Turning every fully-static binding into either an `Entry::Function`
//!   (when `binding.lambda` is `Some`), an `Entry::Module` (attrset RHS
//!   with nested children), or an `Entry::Constant` with a typed binding.
//! * Emitting intermediate path modules for multi-segment prefixes so
//!   `wire_members` can attach children.
//! * Deduplicating by path (last binding wins, matching evaluation order).
//!
//! Responsibilities of *context*:
//!
//! * Wiring `Module::members` after both the static and dynamic passes.

use std::path::PathBuf;

use ir::entry::NudoxPath;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;
use rustc_hash::FxHashMap as HashMap;

use super::docs;
use super::function;
use super::package::FlakeMeta;
use super::syntax::{self, StaticTable, ValueShape};
use super::types;

// ──────────────────────────────────────────────────────────────────────────
// Path helpers (shared convention — everyone `use super::item::path_of`)
// ──────────────────────────────────────────────────────────────────────────

/// Convert an attrpath segment list into the canonical [`NudoxPath`] used
/// throughout the Nix producer.
///
/// Segments are joined with `/`, matching the POSIX path representation.
/// Dynamic segments (`${}`) are passed through verbatim — callers are
/// expected to filter them out via [`syntax::Binding::is_fully_static`]
/// before calling this function.
///
/// # Examples
///
/// ```text
/// path_of(&["lib", "attrsets", "mapAttrs"])
///     => NudoxPath::Local(PathBuf::from("lib/attrsets/mapAttrs"))
/// ```
pub fn path_of(segments: &[String]) -> NudoxPath {
	NudoxPath::Local(PathBuf::from(segments.join("/")))
}

// ──────────────────────────────────────────────────────────────────────────
// Main lowering pass
// ──────────────────────────────────────────────────────────────────────────

/// Lower the entire static table into IR entries.
///
/// Returns `(entries, roots)` where `roots` contains the single flake root
/// path.  Module wiring (`Module::members`) is deferred to `context`.
///
/// # Algorithm
///
/// 1. Emit the root `Entry::Module` for the flake at path `""`.
/// 2. For every binding in every file with a fully-static attrpath:
///    * If `binding.lambda` is `Some`, lower via `function::lower_lambda`.
///    * Else if the RHS is an attrset (or any path that has nested children),
///      emit `Entry::Module`.
///    * Otherwise emit `Entry::Constant` with a typed binding from the shape.
/// 3. Emit intermediate modules for every proper path prefix so
///    `wire_members` can attach children under `Entry::Module` parents.
/// 4. Deduplicate: later bindings overwrite earlier ones at the same path
///    (Modules are sticky — a later Constant at a Module path is ignored if
///    children exist; a later Module upgrades a Constant).
pub fn lower_static(
	table: &StaticTable,
	meta:  &FlakeMeta,
) -> (Vec<(NudoxPath, Entry)>, Vec<NudoxPath>) {
	// Accumulate & deduplicate; last binding wins.
	let mut by_path: HashMap<NudoxPath, Entry> = HashMap::default();
	let mut roots: Vec<NudoxPath> = Vec::new();

	// ── 1. Root module ─────────────────────────────────────────────────────
	let root_path = NudoxPath::Local(PathBuf::from(""));
	let root_name = meta
		.name
		.clone()
		.unwrap_or_else(|| "flake".to_string());

	let root_doc: Option<String> = match (&meta.description, &meta.readme) {
		(Some(d), Some(r)) => Some(format!("{d}\n\n---\n\n{r}")),
		(Some(d), None) => Some(d.clone()),
		(None, Some(r)) => Some(r.clone()),
		(None, None) => None,
	};

	by_path.insert(
		root_path.clone(),
		Entry::Module(Symbol {
			name:          root_name,
			path:          root_path.clone(),
			aliases:       None,
			visibility:    Visibility::Public,
			documentation: root_doc,
			deprecation:   None,
			doc_links:     None,
			inner:         Module { members: None },
		}),
	);
	roots.push(root_path);

	// Collect fully-static paths so we know which intermediate prefixes
	// have children (and which attrset bindings should stay Modules).
	let mut all_static_paths: Vec<Vec<String>> = Vec::new();

	// ── 2. Per-binding lowering ─────────────────────────────────────────────
	for file in &table.files {
		for binding in &file.bindings {
			// Skip bindings with dynamic attrpath components.
			if !binding.is_fully_static() {
				continue;
			}

			let segs = binding.path_strings();
			if segs.is_empty() {
				continue;
			}
			all_static_paths.push(segs.clone());

			let name = segs.last().cloned().unwrap_or_default();
			let path = path_of(&segs);

			// Prefer the binding's own doc; for `inherit` aliases fall back to
			// the doc of the owning binding / lambda that shares the same
			// lambda index so the alias surface still carries RFC-145 text.
			let raw_doc = binding.doc.clone().or_else(|| {
				binding.lambda.and_then(|idx| {
					file.bindings
						.iter()
						.find(|b| b.lambda == Some(idx) && b.doc.is_some())
						.and_then(|b| b.doc.clone())
						.or_else(|| file.lambdas.get(idx).and_then(|l| l.doc.clone()))
				})
			});
			let parsed_doc = raw_doc
				.as_deref()
				.map(docs::parse_doc)
				.unwrap_or_default();
			// Fold ParsedDoc examples into the markdown body.
			let documentation = parsed_doc.to_documentation();

			let entry = if let Some(lambda_idx) = binding.lambda {
				// Lambda binding → Entry::Function.
				if let Some(lam) = file.lambdas.get(lambda_idx) {
					let sig = function::signature_from_doc(&parsed_doc);
					let func = function::lower_lambda(lam, &parsed_doc, sig.as_ref());
					Entry::Function(Symbol {
						name,
						path:          path.clone(),
						aliases:       None,
						visibility:    Visibility::Private,
						documentation,
						deprecation:   None,
						doc_links:     None,
						inner:         func,
					})
				} else {
					// Lambda index out of bounds — degrade to typed Constant.
					let source = binding.value_span.slice(&file.source);
					let inner = types::typed_binding_from_shape(&binding.value_shape, source);
					Entry::Constant(Symbol {
						name,
						path:          path.clone(),
						aliases:       None,
						visibility:    Visibility::Private,
						documentation,
						deprecation:   None,
						doc_links:     None,
						inner,
					})
				}
			} else if matches!(binding.value_shape, ValueShape::AttrSet { .. }) {
				// Nested attrset → Module so wire_members can attach children.
				Entry::Module(Symbol {
					name,
					path:          path.clone(),
					aliases:       None,
					visibility:    Visibility::Private,
					documentation,
					deprecation:   None,
					doc_links:     None,
					inner:         Module { members: None },
				})
			} else {
				// Scalar / list / other → typed Constant.
				let source = binding.value_span.slice(&file.source);
				let inner = types::typed_binding_from_shape(&binding.value_shape, source);
				Entry::Constant(Symbol {
					name,
					path:          path.clone(),
					aliases:       None,
					visibility:    Visibility::Private,
					documentation,
					deprecation:   None,
					doc_links:     None,
					inner,
				})
			};

			insert_preferring_module(&mut by_path, path, entry);
		}
	}

	// ── 3. Intermediate path modules for multi-segment prefixes ────────────
	// `a.b.c = …` without an explicit `a = { … }` still needs Modules at
	// `a` and `a/b` so wire_members attaches children.
	ensure_prefix_modules(&mut by_path, &all_static_paths);

	// Collect into a stable-ordered Vec.
	let entries: Vec<(NudoxPath, Entry)> = by_path.into_iter().collect();
	(entries, roots)
}

/// Insert `entry` at `path`, keeping an existing Module when the new entry
/// is a bare Constant (so nested-member parents stay Modules).
fn insert_preferring_module(
	by_path: &mut HashMap<NudoxPath, Entry>,
	path: NudoxPath,
	entry: Entry,
) {
	match by_path.get(&path) {
		Some(Entry::Module(_)) if matches!(entry, Entry::Constant(_)) => {
			// Keep the Module shell; children will still wire under it.
		}
		Some(Entry::Constant(_)) if matches!(entry, Entry::Module(_)) => {
			// Upgrade Constant → Module (attrset wins over a prior placeholder).
			by_path.insert(path, entry);
		}
		_ => {
			by_path.insert(path, entry);
		}
	}
}

/// Ensure every proper prefix of a static path is present as a Module so
/// `wire_members` has a Module parent to attach children to.
fn ensure_prefix_modules(
	by_path: &mut HashMap<NudoxPath, Entry>,
	paths: &[Vec<String>],
) {
	for segs in paths {
		if segs.len() < 2 {
			continue;
		}
		for len in 1..segs.len() {
			let prefix = &segs[..len];
			let path = path_of(prefix);
			// Only fill missing slots or upgrade Constants — never clobber
			// an existing Function / typed entry.
			match by_path.get(&path) {
				None => {
					let name = prefix.last().cloned().unwrap_or_default();
					by_path.insert(
						path.clone(),
						Entry::Module(Symbol {
							name,
							path,
							aliases:       None,
							visibility:    Visibility::Private,
							documentation: None,
							deprecation:   None,
							doc_links:     None,
							inner:         Module { members: None },
						}),
					);
				}
				Some(Entry::Constant(_)) => {
					let name = prefix.last().cloned().unwrap_or_default();
					by_path.insert(
						path.clone(),
						Entry::Module(Symbol {
							name,
							path,
							aliases:       None,
							visibility:    Visibility::Private,
							documentation: None,
							deprecation:   None,
							doc_links:     None,
							inner:         Module { members: None },
						}),
					);
				}
				_ => {}
			}
		}
	}
}

/// Promote every top-level (single-segment) static binding to Public.
///
/// Used by the orchestrator when the dynamic layer produces no surface —
/// export roots of a plain `.nix` tree / static-only flake should still be
/// publicly visible in the index.
pub fn promote_export_roots(by_path: &mut HashMap<NudoxPath, Entry>) {
	for (path, entry) in by_path.iter_mut() {
		let is_top = match path {
			NudoxPath::Local(p) => {
				let s = p.to_string_lossy();
				// Single-segment non-empty path (not root "", not nested).
				!s.is_empty() && !s.contains('/')
			}
			NudoxPath::External { .. } => false,
		};
		if !is_top {
			continue;
		}
		// Don't touch the synthetic builtins module (already Public).
		set_visibility(entry, Visibility::Public);
	}
}

fn set_visibility(entry: &mut Entry, vis: Visibility) {
	match entry {
		Entry::Module(s) => s.visibility = vis.clone(),
		Entry::Function(s) => s.visibility = vis.clone(),
		Entry::RecordType(s) => s.visibility = vis.clone(),
		Entry::Constant(s) => s.visibility = vis.clone(),
		Entry::TypeAlias(s) => s.visibility = vis.clone(),
		Entry::Variable(s) => s.visibility = vis.clone(),
		Entry::Info(s) => s.visibility = vis.clone(),
		Entry::UnionType(s) => s.visibility = vis.clone(),
		Entry::TraitDef(s) => s.visibility = vis.clone(),
		Entry::TraitImpl(s) => s.visibility = vis.clone(),
		Entry::SumType(s) => s.visibility = vis.clone(),
		Entry::Macro(s) => s.visibility = vis.clone(),
		Entry::PrimitiveType(s) => s.visibility = vis.clone(),
		Entry::Field(s) => s.visibility = vis.clone(),
		Entry::Event(s) => s.visibility = vis,
	}
}

