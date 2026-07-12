//! Production tree-sitter CST extraction.
//!
//! Given a symbol's source and span, finds the enclosing function, extracts a
//! self-contained snippet, re-parses it to an s-expression, and resolves the
//! references within it. Falls back gracefully (full text, no CST) on an
//! unsupported language or a parse failure.
//!
//! The [`spec`] submodule and per-language extractors ([`rust`], [`go`], …)
//! implement the richer [`spec::LanguageSpec`] contract (definitions, imports,
//! qualified references) that the occurrence resolver consumes; the classifiers
//! below remain for the legacy snippet path.

pub mod spec;

pub mod go;
pub mod java;
pub mod nix;
pub mod python;
pub mod rust;
pub mod typescript;

use std::{ops::Range, path::PathBuf};

use arborium_tree_sitter as tree_sitter;
use heart::Language;
use ir::{
	entry::NudoxPath,
	syntax::{ReferenceKind, ResolvedReference, walk_references},
};
use serde::{Deserialize, Deserializer, Serialize};

/// The structural [`spec::LanguageSpec`] extractor for `lang`.
pub fn spec_for(lang: Language) -> &'static dyn spec::LanguageSpec {
	match lang {
		Language::Rust => &rust::RustSpec,
		Language::Python => &python::PythonSpec,
		Language::Typescript => &typescript::TypescriptSpec,
		Language::Go => &go::GoSpec,
		Language::Java => &java::JavaSpec,
		Language::Nix => &nix::NixSpec,
	}
}

// ─── Serializable primitives ─────────────────────────────────────────────────

/// Opaque tree-sitter representation of a source chunk as raw bytes.
///
/// The concrete format is the JSON-serialized [`TreesitterPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreesitterRepr(pub Vec<u8>);

/// A byte range within a source file or buffer.
///
/// Invariant: `start <= end`. Enforced on construction and during
/// deserialization.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ByteSpan {
	start: usize,
	end:   usize,
}

impl ByteSpan {
	/// Returns `None` when `start > end`.
	pub fn new(start: usize, end: usize) -> Option<Self> {
		if start <= end { Some(Self { start, end }) } else { None }
	}

	/// Construct from a range known to satisfy `start <= end`. Panics otherwise.
	#[inline]
	pub fn covering(start: usize, end: usize) -> Self {
		assert!(start <= end, "ByteSpan: start ({start}) > end ({end})");
		Self { start, end }
	}

	/// Inclusive start byte offset.
	#[inline]
	pub fn start(self) -> usize { self.start }

	/// Exclusive end byte offset.
	#[inline]
	pub fn end(self) -> usize { self.end }

	/// Length in bytes.
	#[inline]
	pub fn len(self) -> usize { self.end - self.start }

	/// True when `start == end`.
	#[inline]
	pub fn is_empty(self) -> bool { self.start == self.end }
}

impl<'de> Deserialize<'de> for ByteSpan {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		#[derive(Deserialize)]
		struct Raw {
			start: usize,
			end:   usize,
		}
		let Raw { start, end } = Raw::deserialize(d)?;
		Self::new(start, end)
			.ok_or_else(|| serde::de::Error::custom(format!("ByteSpan: start ({start}) > end ({end})")))
	}
}

// ─── Serializable payload stored in TreesitterRepr bytes ─────────────────────

/// The JSON payload carried inside a [`TreesitterRepr`].
#[derive(Serialize, Deserialize)]
pub struct TreesitterPayload {
	/// S-expression of the full parsed tree.
	pub sexp:         String,
	/// Byte span of the extracted snippet within the raw code.
	pub snippet_span: ByteSpan,
	/// Symbol references found in the snippet.
	pub references:   Vec<ReferenceEntry>,
}

/// One resolved reference in serializable form.
#[derive(Serialize, Deserialize)]
pub struct ReferenceEntry {
	/// The referenced name (path-rendered for external targets).
	pub name: String,
	/// The `ReferenceKind`, Debug-rendered.
	pub kind: String,
	/// The reference's byte span within the snippet.
	pub span: ByteSpan,
}

// ─── Language selection ──────────────────────────────────────────────────────

/// Resolve the arborium grammar for an IR [`Language`].
///
/// TypeScript uses the `"typescript"` grammar. File-level TSX / JavaScript
/// selection (`.tsx` → `"tsx"`, `.js` → `"javascript"`) is handled in
/// [`crate::generate::cst`] where the extension is known.
fn ts_language(lang: Language) -> Option<tree_sitter::Language> {
	match lang {
		Language::Rust => arborium::get_language("rust"),
		Language::Python => arborium::get_language("python"),
		Language::Typescript => arborium::get_language("typescript"),
		Language::Go => arborium::get_language("go"),
		Language::Java => arborium::get_language("java"),
		Language::Nix => arborium::get_language("nix"),
	}
}

// ─── Shared classifier helper ────────────────────────────────────────────────

fn local_ref(name: &str, span: Range<usize>, kind: ReferenceKind) -> ResolvedReference {
	// Full path resolution happens later against the surface index. Use
	// Local(name) as a placeholder so the reference name is preserved.
	ResolvedReference { target: NudoxPath::Local(PathBuf::from(name)), span, kind }
}

// ─── Rust-specific node classifier ───────────────────────────────────────────
//
// Mirrors the categorize_rust logic from the ir::syntax tests so the production
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

pub(crate) fn classify_rust(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_rust(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── Python ──────────────────────────────────────────────────────────────────

fn categorize_python(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("attribute")) => Some(ReferenceKind::FieldAccess),
		// Type annotations wrap names in a `type` node.
		("identifier", Some("type")) => Some(ReferenceKind::TypeReference),
		("identifier", Some("import_statement" | "import_from_statement" | "dotted_name" | "aliased_import")) => {
			Some(ReferenceKind::Import)
		}
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub(crate) fn classify_python(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_python(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── TypeScript / JavaScript ─────────────────────────────────────────────────
//
// Tree-sitter TypeScript/JS use `member_expression` + `property_identifier`
// (not the demo's older `property_access_expression` name).

fn categorize_typescript(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("property_identifier", Some("member_expression" | "property_access_expression")) => {
			Some(ReferenceKind::FieldAccess)
		}
		("identifier", Some("member_expression" | "property_access_expression")) => {
			Some(ReferenceKind::FieldAccess)
		}
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("identifier", Some("import_specifier" | "import_clause" | "named_imports" | "namespace_import")) => {
			Some(ReferenceKind::Import)
		}
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub(crate) fn classify_typescript(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_typescript(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── Go ──────────────────────────────────────────────────────────────────────

fn categorize_go(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("call_expression")) => Some(ReferenceKind::FunctionCall),
		("field_identifier", Some("selector_expression")) => Some(ReferenceKind::FieldAccess),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("package_identifier", Some("import_spec" | "import_declaration")) => {
			Some(ReferenceKind::Import)
		}
		// Bare package path segments in import_spec are interpreted_string_literal,
		// so classify identifiers that appear under import nodes as Import.
		("identifier", Some("import_spec" | "import_declaration")) => Some(ReferenceKind::Import),
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub(crate) fn classify_go(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_go(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── Java ────────────────────────────────────────────────────────────────────

fn categorize_java(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		("identifier", Some("method_invocation")) => Some(ReferenceKind::FunctionCall),
		("identifier", Some("field_access")) => Some(ReferenceKind::FieldAccess),
		("type_identifier", _) => Some(ReferenceKind::TypeReference),
		("scoped_identifier" | "identifier", Some("import_declaration")) => {
			Some(ReferenceKind::Import)
		}
		("identifier", _) => Some(ReferenceKind::VariableUse),
		_ => None,
	}
}

pub(crate) fn classify_java(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_java(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── Nix ─────────────────────────────────────────────────────────────────────
//
// Arborium-nix / tree-sitter-nix node names vary slightly across versions.
// Accept both the longer `*_expression` forms and the shorter aliases.

fn categorize_nix(kind: &str, parent: Option<&str>) -> Option<ReferenceKind> {
	match (kind, parent) {
		// Function application: `f x` / `f.g x`.
		("identifier" | "variable_expression" | "attr_identifier", Some("apply_expression" | "apply" | "app")) => {
			Some(ReferenceKind::FunctionCall)
		}
		// Attribute selection: `foo.bar`.
		("identifier" | "attr_identifier", Some("select_expression" | "select" | "attrpath")) => {
			Some(ReferenceKind::FieldAccess)
		}
		// `inherit (pkgs) foo` / bare `inherit foo`.
		("identifier" | "attr_identifier", Some("inherit" | "inherited_attrs" | "attrspath")) => {
			Some(ReferenceKind::Import)
		}
		("identifier" | "variable_expression" | "attr_identifier", _) => {
			Some(ReferenceKind::VariableUse)
		}
		_ => None,
	}
}

pub(crate) fn classify_nix(
	name: &str,
	kind: &str,
	parent_kind: Option<&str>,
	span: Range<usize>,
) -> Option<ResolvedReference> {
	let ref_kind = categorize_nix(kind, parent_kind)?;
	Some(local_ref(name, span, ref_kind))
}

// ─── Classifier dispatch ─────────────────────────────────────────────────────

/// Select the language-specific reference classifier for `lang`.
pub(crate) fn classify_for(
	lang: Language,
) -> fn(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference> {
	match lang {
		Language::Rust => classify_rust,
		Language::Python => classify_python,
		Language::Typescript => classify_typescript,
		Language::Go => classify_go,
		Language::Java => classify_java,
		Language::Nix => classify_nix,
	}
}

// ─── Enclosing function finder ───────────────────────────────────────────────

/// True when `kind` is a function-like node for any supported language.
fn is_function_like(kind: &str) -> bool {
	matches!(
		kind,
		// Rust
		"function_item"
			| "closure_expression"
			// Python
			| "function_definition"
			| "lambda"
			// TypeScript / JavaScript / Go (Go shares `function_declaration`)
			| "function_declaration"
			| "function_expression"
			| "arrow_function"
			| "method_definition"
			| "generator_function"
			| "generator_function_declaration"
			// Go
			| "method_declaration"
			| "func_literal"
			// Java
			| "constructor_declaration"
			| "lambda_expression"
			// Nix (long and short names used by different grammar revisions)
			| "function"
	)
}

/// Returns the byte range of the innermost function-like node that completely
/// contains [sym_start, sym_end), or `None` if the symbol is not inside any.
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
		if is_function_like(node.kind()) {
			return Some(node.byte_range());
		}
		node = node.parent()?;
	}
}

// ─── Line counting helpers ───────────────────────────────────────────────────

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

// ─── Reference entry conversion ──────────────────────────────────────────────

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

// ─── Public entry point ──────────────────────────────────────────────────────

/// Parse `raw_code` with the tree-sitter grammar for `lang`, extract the
/// smallest enclosing function node around `symbol_span` (falling back to a
/// centered line window when that node is larger than `max_context_lines`),
/// walk symbol references in the snippet, and return:
///
/// - the extracted snippet text (used as the embedding chunk),
/// - its byte span within `raw_code`,
/// - a `TreesitterRepr` carrying a JSON payload with the snippet's tree
///   s-expression, the snippet span, and the collected references.
///
/// On an unsupported language or any parse failure the function falls back
/// gracefully to the full `raw_code` with no repr.
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
	let classify = classify_for(lang);
	let (references, sexp) = {
		let mut snippet_parser = tree_sitter::Parser::new();
		if snippet_parser.set_language(&ts_lang).is_ok() {
			if let Some(snippet_tree) = snippet_parser.parse(snippet.as_bytes(), None) {
				let sexp = snippet_tree.root_node().to_sexp();
				let refs = walk_references(&snippet_tree, &snippet, classify)
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

// ─── Example rendering (the "Show" step) ─────────────────────────────────────

/// A usage rendered as an example: the enclosing-function snippet plus the byte
/// range *within the snippet* that the use site occupies, for highlighting.
///
/// This is the payoff of the references feature (REFERENCES-PLAN §0/§4.5): each
/// answer to "where is X used?" can be shown as the caller's source with the
/// call site marked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Example {
	/// The enclosing-function snippet (or a centered window fallback).
	pub snippet: String,
	/// The use site's byte range within `snippet`.
	pub highlight: Range<usize>,
}

/// Render an occurrence at `occurrence_span` in `source` as an [`Example`]:
/// extract its enclosing-function snippet and locate the use site inside it.
///
/// The first production caller of [`parse_and_extract`]. Falls back to the
/// whole file (or a centered window for large functions) exactly as
/// `parse_and_extract` does; the highlight is clamped to the snippet bounds so
/// it is always a valid sub-range.
pub fn render_example(
	source: &str,
	lang: Language,
	occurrence_span: Range<usize>,
	max_context_lines: usize,
) -> Example {
	let Some(span) = ByteSpan::new(occurrence_span.start, occurrence_span.end) else {
		return Example { snippet: source.to_string(), highlight: 0..0 };
	};
	let (snippet, snippet_span, _) = parse_and_extract(source, lang, span, max_context_lines);

	// Translate the absolute occurrence span into snippet-relative coordinates,
	// clamped so the highlight never escapes the snippet.
	let base = snippet_span.start();
	let start = occurrence_span.start.saturating_sub(base).min(snippet.len());
	let end = occurrence_span.end.saturating_sub(base).min(snippet.len());
	Example { snippet, highlight: start..end.max(start) }
}
