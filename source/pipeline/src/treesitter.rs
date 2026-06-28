use std::{ops::Range, path::PathBuf};

use arborium_tree_sitter as tree_sitter;
use ir::{entry::NudoxPath, syntax::{ReferenceKind, ResolvedReference, walk_references}};
use nudox_core::{ByteSpan, Language, TreesitterRepr};
use serde::{Deserialize, Serialize};

// ─── Serializable payload stored in TreesitterRepr bytes ─────────────────────

#[derive(Serialize, Deserialize)]
struct TreesitterPayload {
	/// S-expression of the full parsed tree.
	sexp:         String,
	/// Byte span of the extracted snippet within raw_code.
	snippet_span: ByteSpan,
	/// Symbol references found in the snippet.
	references:   Vec<ReferenceEntry>,
}

#[derive(Serialize, Deserialize)]
struct ReferenceEntry {
	name: String,
	kind: String,
	span: ByteSpan,
}

// ─── Language selection
// ───────────────────────────────────────────────────────

fn ts_language(lang: Language) -> Option<tree_sitter::Language> {
	match lang {
		Language::Rust => arborium::get_language("rust"),
		Language::TypeScript => None,
	}
}

// ─── Rust-specific node classifier ───────────────────────────────────────────
//
// Mirrors the categorize_rust logic from the Backend IR tests so the production
// pipeline uses the same classification rules.

fn categorize_rust(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("scoped_identifier")) => None,
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("macro_invocation")) => Some(ReferenceKind::MacroInvocation),
		("identifier", Some("scoped_use_tree")) => Some(ReferenceKind::Import),
		("identifier", Some("use_as_clause")) => Some(ReferenceKind::Import),
		("field_identifier", Some("field_expression")) | ("identifier", Some("field_expression")) => {
			Some(ReferenceKind::FieldAccess)
		}
		("identifier", _) => Some(ReferenceKind::VariableUse),
		("scoped_identifier", Some("scoped_use_tree" | "use_declaration")) => {
			Some(ReferenceKind::Import)
		}
		("scoped_identifier", _) => Some(ReferenceKind::TypeReference),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		_ => None,
	}
}

fn classify_rust(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_rust(kind, parent_kind)?;
	// Full path resolution happens later in the orchestrator via GlobalSymbolStore.
	// Use Local(name) as a placeholder so the reference name is preserved.
	let target = NudoxPath::Local(PathBuf::from(name));
	Some(ResolvedReference { target, span, kind: ref_kind })
}

// ─── Enclosing function finder
// ────────────────────────────────────────────────

/// Returns the byte range of the innermost `function_item` or
/// `closure_expression` node that completely contains [sym_start, sym_end), or
/// `None` if the symbol is not inside any function node.
fn find_enclosing_fn_range(
	tree: &tree_sitter::Tree,
	sym_start: usize,
	sym_end: usize,
) -> Option<Range<usize>> {
	// Walk down directly to the symbol using the built-in descendant lookup,
	// then walk up via parent() until we hit a function-like node.
	let root = tree.root_node();
	let leaf = root.descendant_for_byte_range(sym_start, sym_end)?;
	let mut node = leaf;
	loop {
		if matches!(node.kind(), "function_item" | "closure_expression") {
			return Some(node.byte_range());
		}
		node = node.parent()?;
	}
}

// ─── Line counting helpers
// ────────────────────────────────────────────────────

fn count_lines(s: &str) -> usize { s.bytes().filter(|&b| b == b'\n').count() + 1 }

/// Extract a centered window of `max_lines` lines around the line containing
/// `sym_start`, returning the snippet text and its byte span within `raw_code`.
fn centered_window(raw_code: &str, sym_start: usize, max_lines: usize) -> (String, ByteSpan) {
	let half = max_lines / 2;
	let lines: Vec<&str> = raw_code.split('\n').collect();

	// Find which line index contains sym_start.
	let mut byte_pos = 0usize;
	let mut sym_line = 0usize;
	for (i, line) in lines.iter().enumerate() {
		let next = byte_pos + line.len() + 1; // +1 for '\n'
		if next > sym_start || i + 1 == lines.len() {
			sym_line = i;
			break;
		}
		byte_pos = next;
	}

	let start_line = sym_line.saturating_sub(half);
	let end_line = (sym_line + half).min(lines.len().saturating_sub(1));

	// Compute the byte offset of start_line.
	let mut start_byte = 0usize;
	for line in &lines[..start_line] {
		start_byte += line.len() + 1;
	}

	let snippet = lines[start_line..=end_line].join("\n");
	let end_byte = start_byte + snippet.len();

	(snippet, ByteSpan::covering(start_byte, end_byte))
}

// ─── Reference entry conversion
// ───────────────────────────────────────────────

fn ref_to_entry(r: ResolvedReference) -> ReferenceEntry {
	let name = match &r.target {
		NudoxPath::Local(p) => p.display().to_string(),
		NudoxPath::External { dependency, path } => {
			let p = path.display().to_string();
			if p.is_empty() { dependency.clone() } else { format!("{dependency}::{p}") }
		}
	};
	ReferenceEntry {
		name,
		kind: format!("{:?}", r.kind),
		span: ByteSpan::covering(r.span.start, r.span.end),
	}
}

// ─── Public entry point
// ───────────────────────────────────────────────────────

/// Parse `raw_code` with the tree-sitter grammar for `lang`, extract the
/// smallest enclosing function node around `symbol_span` (falling back to a
/// centered line window when that node is larger than `max_context_lines`),
/// walk symbol references in the snippet, and return:
///
/// - the extracted snippet text (used as the embedding chunk),
/// - its byte span within `raw_code`,
/// - a `TreesitterRepr` carrying a JSON payload with the full tree
///   s-expression, the snippet span, and the collected references.
///
/// On any parse failure the function falls back gracefully to the full
/// `raw_code` with `treesitter_repr: None`.
pub fn parse_and_extract(
	raw_code: &str,
	lang: Language,
	symbol_span: ByteSpan,
	max_context_lines: usize,
) -> (String, ByteSpan, Option<TreesitterRepr>) {
	let fallback = || (raw_code.to_string(), ByteSpan::covering(0, raw_code.len()), None);

	let Some(ts_lang) = ts_language(lang) else {
		return fallback();
	};

	let mut parser = tree_sitter::Parser::new();
	if parser.set_language(&ts_lang).is_err() {
		return fallback();
	}

	let Some(tree) = parser.parse(raw_code.as_bytes(), None) else {
		return fallback();
	};

	// ── Snippet extraction ────────────────────────────────────────────────────
	let (snippet, snippet_span) = if let Some(fn_range) =
		find_enclosing_fn_range(&tree, symbol_span.start(), symbol_span.end())
	{
		let fn_text = &raw_code[fn_range.clone()];
		if count_lines(fn_text) <= max_context_lines {
			(fn_text.to_string(), ByteSpan::covering(fn_range.start, fn_range.end))
		} else {
			centered_window(raw_code, symbol_span.start(), max_context_lines)
		}
	} else if count_lines(raw_code) <= max_context_lines {
		(raw_code.to_string(), ByteSpan::covering(0, raw_code.len()))
	} else {
		centered_window(raw_code, symbol_span.start(), max_context_lines)
	};

	// ── Parse snippet, walk references, and capture its sexp ─────────────────
	//
	// The sexp and references are scoped to the *snippet* (enclosing function or
	// line window), not the full file, so that consumers see a self-contained
	// structural representation of the embedding chunk.
	let (references, sexp) = {
		let mut snippet_parser = tree_sitter::Parser::new();
		if snippet_parser.set_language(&ts_lang).is_ok() {
			if let Some(snippet_tree) = snippet_parser.parse(snippet.as_bytes(), None) {
				let sexp = snippet_tree.root_node().to_sexp();
				let refs = walk_references(&snippet_tree, &snippet, classify_rust)
					.into_iter()
					.map(ref_to_entry)
					.collect::<Vec<_>>();
				(refs, sexp)
			} else {
				(vec![], String::new())
			}
		} else {
			(vec![], String::new())
		}
	};

	// ── Serialize payload ─────────────────────────────────────────────────────
	let payload = TreesitterPayload { sexp, snippet_span, references };

	let repr_bytes = serde_json::to_vec(&payload).unwrap_or_default();
	(snippet, snippet_span, Some(TreesitterRepr(repr_bytes)))
}
