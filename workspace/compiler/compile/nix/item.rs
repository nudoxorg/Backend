//! Lowering the static Nix table into IR entries.
//!
//! This module converts [`syntax::StaticTable`] bindings into
//! `(NudoxPath, Entry)` pairs that form the skeleton of the flake's IR
//! index.  All entries start with [`ir::kind::Visibility::Private`]; the
//! dynamic layer promotes anything reachable from `outputs` to
//! [`ir::kind::Visibility::Public`].
//!
//! Responsibilities of *this* module:
//!
//! * Building the `NudoxPath` scheme for Nix (`a/b/c` attrpath segments).
//! * Emitting one root `Entry::Module` for the flake.
//! * Turning every fully-static binding into either an `Entry::Function`
//!   (when `binding.lambda` is `Some`) or an `Entry::Constant`.
//! * Deduplicating by path (last binding wins, matching evaluation order).
//!
//! Responsibilities of *context*:
//!
//! * Wiring `Module::members` after both the static and dynamic passes.

use std::collections::HashMap;
use std::path::PathBuf;

use ir::entry::NudoxPath;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;

use super::docs;
use super::function;
use super::package::FlakeMeta;
use super::syntax::{self, StaticTable};

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
///    * Otherwise emit `Entry::Constant`.
/// 3. Deduplicate: later bindings overwrite earlier ones at the same path.
pub fn lower_static(
	table: &StaticTable,
	meta:  &FlakeMeta,
) -> (Vec<(NudoxPath, Entry)>, Vec<NudoxPath>) {
	// Use an ordered map to accumulate & deduplicate; last binding wins.
	let mut by_path: HashMap<NudoxPath, Entry> = HashMap::new();
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
			let documentation = if parsed_doc.markdown.is_empty() {
				None
			} else {
				Some(parsed_doc.markdown.clone())
			};

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
					// Lambda index out of bounds — degrade to Constant.
					Entry::Constant(Symbol {
						name,
						path:          path.clone(),
						aliases:       None,
						visibility:    Visibility::Private,
						documentation,
						deprecation:   None,
						doc_links:     None,
						inner:         (),
					})
				}
			} else {
				// Non-lambda binding → Entry::Constant.
				Entry::Constant(Symbol {
					name,
					path:          path.clone(),
					aliases:       None,
					visibility:    Visibility::Private,
					documentation,
					deprecation:   None,
					doc_links:     None,
					inner:         (),
				})
			};

			// Deduplication: last binding at a given path wins.
			by_path.insert(path, entry);
		}
	}

	// Collect into a stable-ordered Vec.
	let entries: Vec<(NudoxPath, Entry)> = by_path.into_iter().collect();
	(entries, roots)
}
