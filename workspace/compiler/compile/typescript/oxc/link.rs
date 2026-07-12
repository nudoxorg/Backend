//! Pass 2: cross-module linking (OXC-PLAN §Phase 3). Operates purely on
//! `Vec<ModuleFacts>` (no ASTs): assigns module names + paths, resolves
//! re-exports (`indirect`/`star`), runs the visibility BFS, fills symbol-accurate
//! `type_links`, and materializes the final `ir::entry::Index`.
//!
//! The pure path/module-naming helpers below are ported verbatim from the old
//! deno pipeline (they only ever touched paths, never deno types) — the only
//! change is dropping the `ModuleSpecifier` URL branch now that specifiers are
//! plain paths.
//!
//! LEAF FILE — fill the `todo!()` body of [`link`]; the helpers are done.

use std::{
	collections::HashMap,
	hash::{Hash, Hasher},
	path::PathBuf,
};

use ir::entry::Index;

use super::extract::ModuleFacts;

/// Cross-module link: `ModuleFacts` set → `ir::entry::Index`.
pub(crate) fn link(modules: Vec<ModuleFacts>, package: &str) -> Result<Index, super::error::Package> {
	let _ = (modules, package);
	todo!("link.rs: module names → paths → re-exports → visibility BFS → type_links → Index")
}

// ============================================================================
// Pure path/module-naming helpers (deno-free ports of the old mod.rs)
// ============================================================================

/// A stable 64-bit id for an entry path, used for `type_links`.
pub(crate) fn path_to_id(path: &[String]) -> i64 {
	let mut hasher = std::collections::hash_map::DefaultHasher::new();
	path.hash(&mut hasher);
	hasher.finish() as i64
}

/// Convert a local file path to a short module name (basename, extension
/// stripped).
pub(crate) fn specifier_to_module_name(specifier: &str) -> String {
	let clean = specifier
		.split('?')
		.next()
		.unwrap_or(specifier)
		.split('#')
		.next()
		.unwrap_or(specifier);
	let last = clean.split('/').next_back().unwrap_or(clean);
	let name = strip_typescript_like_suffix(last);
	if name.is_empty() { clean.to_string() } else { name.to_string() }
}

fn strip_typescript_like_suffix(value: &str) -> &str {
	value
		.trim_end_matches(".d.ts")
		.trim_end_matches(".d.tsx")
		.trim_end_matches(".d.mts")
		.trim_end_matches(".d.cts")
		.trim_end_matches(".ts")
		.trim_end_matches(".tsx")
		.trim_end_matches(".mts")
		.trim_end_matches(".cts")
		.trim_end_matches(".js")
		.trim_end_matches(".mjs")
		.trim_end_matches(".cjs")
		.trim_end_matches(".jsx")
}

fn specifier_to_module_segments(specifier: &str) -> Vec<String> {
	let clean = specifier
		.split('?')
		.next()
		.unwrap_or(specifier)
		.split('#')
		.next()
		.unwrap_or(specifier);
	let raw_segments: Vec<String> = clean
		.split('/')
		.filter(|segment| !segment.is_empty())
		.map(str::to_string)
		.collect();

	if raw_segments.is_empty() {
		return vec![clean.to_string()];
	}

	let last_index = raw_segments.len() - 1;
	raw_segments
		.into_iter()
		.enumerate()
		.map(|(idx, segment)| {
			if idx == last_index {
				strip_typescript_like_suffix(&segment).to_string()
			} else {
				segment
			}
		})
		.filter(|segment| !segment.is_empty() && segment != ".")
		.collect()
}

/// Assign a unique, short dotted module name to every specifier.
pub(crate) fn assign_unique_module_names(specifiers: &[String]) -> HashMap<String, String> {
	let mut segment_map: Vec<(String, Vec<String>)> = specifiers
		.iter()
		.cloned()
		.map(|specifier| {
			let segments = specifier_to_module_segments(&specifier);
			(specifier, if segments.is_empty() { vec!["module".to_string()] } else { segments })
		})
		.collect();

	let shared_prefix_len = shared_segment_prefix_len(
		&segment_map.iter().map(|(_, segments)| segments.as_slice()).collect::<Vec<_>>(),
	);
	if shared_prefix_len > 0 {
		for (_, segments) in &mut segment_map {
			if segments.len() > shared_prefix_len {
				segments.drain(..shared_prefix_len);
			}
		}
	}

	let mut assignments = HashMap::with_capacity(segment_map.len());

	for (specifier, segments) in &segment_map {
		let mut chosen = segments.join(".");
		for suffix_len in 1..=segments.len() {
			let candidate = segments[segments.len() - suffix_len..].join(".");
			let duplicate = segment_map.iter().any(|(other_specifier, other_segments)| {
				if other_specifier == specifier {
					return false;
				}
				other_segments.len() >= suffix_len
					&& other_segments[other_segments.len() - suffix_len..].join(".") == candidate
			});
			if !duplicate {
				chosen = candidate;
				break;
			}
		}
		assignments.insert(specifier.clone(), chosen);
	}

	assignments
}

fn shared_segment_prefix_len(segment_sets: &[&[String]]) -> usize {
	let Some(first) = segment_sets.first() else {
		return 0;
	};

	let mut prefix_len = 0;
	while prefix_len < first.len() {
		let candidate = &first[prefix_len];
		if segment_sets
			.iter()
			.all(|segments| segments.len() > prefix_len && segments[prefix_len] == *candidate)
		{
			prefix_len += 1;
		} else {
			break;
		}
	}

	prefix_len
}

#[allow(dead_code)]
fn _keep_module_helpers_live() {
	let _ = (
		path_to_id as fn(&[String]) -> i64,
		specifier_to_module_name as fn(&str) -> String,
		assign_unique_module_names as fn(&[String]) -> HashMap<String, String>,
	);
	let _ = PathBuf::new();
}
