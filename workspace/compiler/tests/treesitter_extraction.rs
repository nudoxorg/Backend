//! Pipeline part: **tree-sitter CST extraction** (`compiler::treesitter`).
//!
//! TDD specs for turning a symbol's source + span into a self-contained
//! snippet, an s-expression, and a set of resolved references. Mirrors the
//! `pipeline::treesitter::parse_and_extract` contract.

/// The enclosing function is extracted as the snippet when small enough.
///
/// Arrange: source `fn outer() { fn bar() { foo() } }`, span over `foo`.
/// Act: `parse_and_extract(src, Language::Rust, span, max_lines)`.
/// Assert: the snippet contains `fn bar`, and a `TreesitterRepr` is produced.
#[test]
fn extracts_enclosing_function_as_snippet() {
    todo!("assert the snippet is the enclosing fn and repr is Some");
}

/// When the enclosing function exceeds `max_context_lines`, a centered window is
/// used instead.
///
/// Assert: for a large enclosing fn, the snippet is a line window centered on
///   the symbol, not the whole function.
#[test]
fn falls_back_to_centered_window_for_large_functions() {
    todo!("assert the centered-window fallback");
}

/// The s-expression of the re-parsed snippet names the grammar nodes.
///
/// Assert: the serialized repr's `sexp` contains `source_file` and
///   `function_item`.
#[test]
fn sexp_contains_grammar_node_names() {
    todo!("parse the repr JSON and assert sexp node names");
}

/// References within the snippet are resolved and classified.
///
/// Arrange: a snippet calling `foo()`, invoking `println!`, and using a type.
/// Assert: the repr's references include a `FunctionCall`, a
///   `MacroInvocation`, and a `TypeReference`.
#[test]
fn resolves_and_classifies_references() {
    todo!("assert reference kinds in the extracted repr");
}

/// An unsupported language falls back gracefully (full text, no repr).
///
/// Act: `parse_and_extract(src, Language::TypeScript, ..)` (not yet wired).
/// Assert: returns the full source and a `None` repr rather than erroring.
#[test]
fn unsupported_language_falls_back_gracefully() {
    todo!("assert graceful fallback for an unsupported language");
}
