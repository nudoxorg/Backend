//! Flake-level metadata extraction.
//!
//! Reads the static surface of `flake.nix` (description binding, README)
//! to produce a [`FlakeMeta`] struct that drives the root module entry and
//! the overall package name. Everything here is offline and evaluation-free;
//! the FlakeHub registry supplies the canonical name upstream.

use std::path::Path;

use super::syntax::{self, StaticTable};

// ──────────────────────────────────────────────────────────────────────────
// Public types
// ──────────────────────────────────────────────────────────────────────────

/// Flake-level metadata gathered from the static layer alone.
///
/// Populated by [`flake_metadata`]; consumed by `item::lower_static` to
/// build the root [`ir::kind::Entry::Module`] and, later, by the
/// traversal layer to populate search metadata.
///
/// Network-facing operations (FlakeHub name lookup, `meta.homepage`,
/// `meta.license`) are intentionally out of scope here — they belong in
/// the dynamic layer (`eval`) and the acquisition layer (`traversal`).
#[derive(Debug, Clone, Default)]
pub struct FlakeMeta {
	/// The flake's human-readable name. `None` at the static layer;
	/// the FlakeHub registry (or `meta.pname` in the dynamic layer) fills
	/// this in later.
	pub name: Option<String>,

	/// The `description` string from the top-level `flake.nix` attrset.
	pub description: Option<String>,

	/// A short summary of `README.md` (first ~40 lines), used as
	/// supplementary documentation on the root module entry.
	pub readme: Option<String>,
}

// ──────────────────────────────────────────────────────────────────────────
// Public API
// ──────────────────────────────────────────────────────────────────────────

/// Extract flake-level metadata from the static table.
///
/// This is purely offline: no evaluation, no network access.
///
/// # Description
///
/// Finds a top-level `description = "…"` binding inside `flake.nix` and
/// slices the literal text from the source span (quotes stripped).
///
/// # Readme
///
/// Reads `README.md` (or bare `README`) from `root` if present and
/// captures the first 40 lines as a short summary string.
///
/// # Name
///
/// Left `None` at the static layer — the FlakeHub registry or the dynamic
/// `meta.pname` path provides the canonical package name.
pub fn flake_metadata(table: &StaticTable, root: &Path) -> FlakeMeta {
	let description = extract_description(table);
	let readme = read_readme(root);
	FlakeMeta { name: None, description, readme }
}

// ──────────────────────────────────────────────────────────────────────────
// Internals
// ──────────────────────────────────────────────────────────────────────────

/// Find `description = "…"` at the top level of `flake.nix` and return the
/// unquoted string value.
fn extract_description(table: &StaticTable) -> Option<String> {
	let file = table.file(Path::new("flake.nix"))?;

	for binding in &file.bindings {
		// We want a single-segment path that is exactly `["description"]`.
		if !binding.is_fully_static() {
			continue;
		}
		let segs = binding.path_strings();
		if segs.as_slice() != ["description"] {
			continue;
		}

		// Slice the raw value text from the source span.
		let raw = binding.value_span.slice(&file.source);
		return Some(unquote_string(raw));
	}

	None
}

/// Strip surrounding double-quotes from a simple Nix string literal.
///
/// Handles the common `"…"` form; multiline `''…''` strings and
/// interpolated strings are returned as-is (a best-effort display).
fn unquote_string(raw: &str) -> String {
	let trimmed = raw.trim();
	if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
		// Unescape basic Nix string escapes (\n, \\, \", \t).
		let inner = &trimmed[1..trimmed.len() - 1];
		return unescape_nix(inner);
	}
	// Multiline string ''…'' — return inner text without delimiters.
	if trimmed.starts_with("''") && trimmed.ends_with("''") && trimmed.len() >= 4 {
		return trimmed[2..trimmed.len() - 2].trim().to_string();
	}
	trimmed.to_string()
}

/// Unescape Nix double-quoted string escape sequences.
fn unescape_nix(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars().peekable();
	while let Some(c) = chars.next() {
		if c == '\\' {
			match chars.next() {
				Some('n') => out.push('\n'),
				Some('t') => out.push('\t'),
				Some('r') => out.push('\r'),
				Some('"') => out.push('"'),
				Some('\\') => out.push('\\'),
				Some('$') => out.push('$'),
				Some(other) => {
					out.push('\\');
					out.push(other);
				}
				None => out.push('\\'),
			}
		} else {
			out.push(c);
		}
	}
	out
}

/// Read `README.md` or `README` from `root`, returning the first 40 lines
/// as a condensed summary string. Returns `None` if neither file exists.
fn read_readme(root: &Path) -> Option<String> {
	const MAX_LINES: usize = 40;

	let candidates = ["README.md", "README.rst", "README.txt", "README"];
	for name in &candidates {
		let path = root.join(name);
		if let Ok(text) = std::fs::read_to_string(&path) {
			let summary: Vec<&str> = text.lines().take(MAX_LINES).collect();
			let joined = summary.join("\n").trim().to_string();
			if !joined.is_empty() {
				return Some(joined);
			}
		}
	}
	None
}
