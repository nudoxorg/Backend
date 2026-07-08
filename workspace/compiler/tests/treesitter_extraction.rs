//! Pipeline part: **tree-sitter CST extraction** (`compiler::treesitter`).
//!
//! Specs for turning a symbol's source + span into a self-contained snippet,
//! an s-expression, and a set of resolved references. Mirrors the
//! `pipeline::treesitter::parse_and_extract` contract.

use compiler::treesitter::{ByteSpan, TreesitterPayload, parse_and_extract};
use heart::Language;

/// The byte span of `needle`'s first occurrence in `haystack`.
fn span_of(haystack: &str, needle: &str) -> ByteSpan {
    let start = haystack.find(needle).expect("needle present in source");
    ByteSpan::covering(start, start + needle.len())
}

fn payload(repr: &compiler::treesitter::TreesitterRepr) -> TreesitterPayload {
    serde_json::from_slice(&repr.0).expect("repr bytes are the JSON payload")
}

/// The enclosing function is extracted as the snippet when small enough.
///
/// Arrange: source `fn outer() { fn bar() { foo() } }`, span over `foo`.
/// Act: `parse_and_extract(src, Language::Rust, span, max_lines)`.
/// Assert: the snippet contains `fn bar`, and a `TreesitterRepr` is produced.
#[test]
fn extracts_enclosing_function_as_snippet() {
    let src = "fn outer() {\n    fn bar() {\n        foo()\n    }\n}\n";
    let (snippet, _span, repr) = parse_and_extract(src, Language::Rust, span_of(src, "foo"), 40);

    assert!(snippet.contains("fn bar"), "snippet should be the enclosing fn: {snippet:?}");
    assert!(
        !snippet.contains("fn outer"),
        "the *innermost* enclosing fn is the snippet: {snippet:?}"
    );
    assert!(repr.is_some(), "a TreesitterRepr should be produced");
}

/// When the enclosing function exceeds `max_context_lines`, a centered window is
/// used instead.
///
/// Assert: for a large enclosing fn, the snippet is a line window centered on
///   the symbol, not the whole function.
#[test]
fn falls_back_to_centered_window_for_large_functions() {
    let max_lines = 7;
    let mut src = String::from("fn big() {\n");
    for i in 0..30 {
        src.push_str(&format!("    let filler_{i} = {i};\n"));
    }
    src.push_str("    foo();\n"); // the symbol, near the end
    for i in 30..60 {
        src.push_str(&format!("    let filler_{i} = {i};\n"));
    }
    src.push_str("}\n");

    let (snippet, _span, repr) =
        parse_and_extract(&src, Language::Rust, span_of(&src, "foo"), max_lines);

    let snippet_lines = snippet.lines().count();
    assert!(
        snippet_lines <= max_lines + 1,
        "snippet should be a bounded window, got {snippet_lines} lines"
    );
    assert!(snippet.contains("foo()"), "window is centered on the symbol: {snippet:?}");
    assert!(
        !snippet.contains("fn big"),
        "the whole oversized function must not be the snippet"
    );
    assert!(repr.is_some());
}

/// The s-expression of the re-parsed snippet names the grammar nodes.
///
/// Assert: the serialized repr's `sexp` contains `source_file` and
///   `function_item`.
#[test]
fn sexp_contains_grammar_node_names() {
    let src = "fn outer() {\n    fn bar() {\n        foo()\n    }\n}\n";
    let (_snippet, _span, repr) = parse_and_extract(src, Language::Rust, span_of(src, "foo"), 40);

    let payload = payload(&repr.expect("repr produced"));
    assert!(payload.sexp.contains("source_file"), "sexp: {}", payload.sexp);
    assert!(payload.sexp.contains("function_item"), "sexp: {}", payload.sexp);
}

/// References within the snippet are resolved and classified.
///
/// Arrange: a snippet calling `foo()`, invoking `println!`, and using a type.
/// Assert: the repr's references include a `FunctionCall`, a
///   `MacroInvocation`, and a `TypeReference`.
#[test]
fn resolves_and_classifies_references() {
    let src = "fn wrapper() {\n    let x: MyType = foo();\n    println!(\"{x:?}\");\n}\n";
    let (_snippet, _span, repr) = parse_and_extract(src, Language::Rust, span_of(src, "foo"), 40);

    let payload = payload(&repr.expect("repr produced"));
    let kinds: Vec<&str> = payload.references.iter().map(|r| r.kind.as_str()).collect();

    assert!(kinds.contains(&"FunctionCall"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"MacroInvocation"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"TypeReference"), "kinds: {kinds:?}");
}

/// An unsupported language falls back gracefully (full text, no repr).
///
/// Act: `parse_and_extract(src, Language::Typescript, ..)` (not yet wired).
/// Assert: returns the full source and a `None` repr rather than erroring.
#[test]
fn unsupported_language_falls_back_gracefully() {
    let src = "export function greet(name: string): string { return `hi ${name}`; }\n";
    let (snippet, span, repr) =
        parse_and_extract(src, Language::Typescript, ByteSpan::covering(16, 21), 40);

    assert_eq!(snippet, src, "full source is returned untouched");
    assert_eq!((span.start(), span.end()), (0, src.len()));
    assert!(repr.is_none(), "no repr for an unsupported language");
}
