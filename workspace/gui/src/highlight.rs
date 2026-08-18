//! Syntax highlighting for code blocks in the doc stream.
//!
//! # Why this module exists as a separate seam
//!
//! The chunker (LR-3) already owns "IR → presentation" but it runs
//! synchronously on the async task and must finish fast (< 50 ms, §9.1).
//! Tree-sitter parsing is also fast, but the *first* call for a grammar
//! builds a `HighlightConfg` that validates the query against the language —
//! that work pays for itself once and is then cached.  Keeping it here, away
//! from the chunker, means the chunker stays single-pass and the highlight
//! path is independently testable.
//!
//! # Protocol guarantee (§9.3 invariant 3)
//!
//! A `Highlight` event must only reference a `SectionId` that has already
//! been sent.  This module does not enforce that invariant — it is upheld by
//! the caller (`doc.rs`) which emits each `Section` before calling
//! `highlight_section` for it.  What this module *does* guarantee is that it
//! never panics and never returns an error: an unknown language or a grammar
//! failure returns an empty span list, letting the stream continue.
//!
//! # Tree-sitter version pin
//!
//! All grammar crates (`tree-sitter-rust`, `tree-sitter-python`,
//! `tree-sitter-go`) use `tree-sitter-language 0.1` as their only runtime
//! dependency, matching the `tree-sitter-language 0.1.7` version already
//! locked by `arborium-tree-sitter`.  The main `tree-sitter` crate is `0.26`
//! which also depends on `tree-sitter-language 0.1`, so the grammar C objects
//! and the runtime C library share the same ABI.  Mismatching major versions
//! of `tree-sitter-language` between a grammar and the runtime produces a
//! silent misparse, not a link error — hence the version comment in Cargo.toml.

use std::sync::OnceLock;

// why the upstream `tree-sitter` crate cannot coexist with it.
use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

use nudox_engine::highlight::Highlighter;
use nudox_engine::wire::{HighlightSpan, SharedStr};

// ---------------------------------------------------------------------------
// Per-language grammar descriptor
// ---------------------------------------------------------------------------

/// Everything the highlighter needs for one language, built once and cached.
struct LangConfig {
    /// The tree-sitter language (wraps the C grammar object).
    language: Language,
    /// Compiled query from the grammar's `HIGHLIGHTS_QUERY` constant.
    ///
    /// `OnceLock` rather than a plain field so that grammar compilation
    /// (which validates the query string against the language) happens lazily
    /// on first use rather than at startup.  This keeps the engine boot path
    /// fast.
    query: OnceLock<Option<Query>>,
    /// Raw query source, kept so `query` can be compiled on demand.
    query_source: &'static str,
}

impl LangConfig {
    /// Borrow the compiled `Query`, building it on the first call.
    ///
    /// Returns `None` if the query fails to compile — not expected for the
    /// bundled grammar constants, but treated as a soft failure so that a bad
    /// grammar update cannot crash the engine.
    /// A grammar whose own bundled query does not compile is a packaging bug,
    /// but it is not worth killing a documentation stream over: the reader
    /// loses colour, not content. So the failure is absorbed into `None` and
    /// logged once, rather than panicking the way an `expect` here would.
    fn query(&self) -> Option<&Query> {
        // `get_or_init` cannot express "store nothing on failure", so the
        // fallible build is done first and the `OnceLock` holds an
        // `Option<Query>`. A grammar that fails to compile is retried never —
        // it will fail identically every time.
        self.query
            .get_or_init(|| match Query::new(&self.language, self.query_source) {
                Ok(query) => Some(query),
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "bundled HIGHLIGHTS_QUERY failed to compile; \
                         this language will render without highlighting"
                    );
                    None
                }
            })
            .as_ref()
    }
}

// ---------------------------------------------------------------------------
// Language tag → grammar mapping
// ---------------------------------------------------------------------------

/// The languages for which we carry a grammar and a highlight query.
///
/// This list is exhaustive and explicit.  An unknown tag returns `None` from
/// `config_for`; the caller emits no spans (not an error).  Adding a language
/// means: (1) add the grammar crate to `Cargo.toml`, (2) add a variant here,
/// (3) add a match arm in `config_for`.
enum SupportedLang {
    Rust,
    Python,
    Go,
}

/// Map the fenced-code-block info string to a supported language.
///
/// The info string is the word after the opening fence (e.g. ` ```rust `
/// yields `"rust"`).  We normalise to lowercase so `Rust` and `RUST` both
/// work.  Unknown tags return `None` — they produce no spans, never a panic.
fn classify_lang(tag: &str) -> Option<SupportedLang> {
    match tag.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => Some(SupportedLang::Rust),
        "python" | "py" => Some(SupportedLang::Python),
        "go" | "golang" => Some(SupportedLang::Go),
        _ => None,
    }
}

/// Return the cached `LangConfig` for a supported language.
///
/// Each variant has its own `OnceLock<LangConfig>` so that the per-language
/// query is compiled at most once per process lifetime.
fn config_for(lang: SupportedLang) -> &'static LangConfig {
    match lang {
        SupportedLang::Rust => {
            static CFG: OnceLock<LangConfig> = OnceLock::new();
            CFG.get_or_init(|| LangConfig {
                language: tree_sitter_rust::LANGUAGE.into(),
                query: OnceLock::new(),
                query_source: tree_sitter_rust::HIGHLIGHTS_QUERY,
            })
        }
        SupportedLang::Python => {
            static CFG: OnceLock<LangConfig> = OnceLock::new();
            CFG.get_or_init(|| LangConfig {
                language: tree_sitter_python::LANGUAGE.into(),
                query: OnceLock::new(),
                query_source: tree_sitter_python::HIGHLIGHTS_QUERY,
            })
        }
        SupportedLang::Go => {
            static CFG: OnceLock<LangConfig> = OnceLock::new();
            CFG.get_or_init(|| LangConfig {
                language: tree_sitter_go::LANGUAGE.into(),
                query: OnceLock::new(),
                query_source: tree_sitter_go::HIGHLIGHTS_QUERY,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Capture name → class string
// ---------------------------------------------------------------------------

/// Map a tree-sitter capture name (e.g. `"type.builtin"`) to the class string
/// the GUI's `class_colour` function understands (§9.3, `docs.rs`).
///
/// The GUI's `class_colour` accepts dot-qualified names (`"type.builtin"`,
/// `"function.method"`) but also plain names (`"keyword"`, `"comment"`).
/// Rather than stripping the qualifier, we pass the full name through and let
/// `class_colour`'s `_` arm provide a legible fallback.  We only normalise
/// the cases where the grammar uses a name that differs from the class the GUI
/// explicitly handles.
///
/// Grammar capture names use the tree-sitter convention (`keyword`, `type`,
/// `function`, `comment`, `string`, `number`, `operator`, `punctuation`,
/// `attribute`, `constant`, `variable`, `property`).  The GUI's `class_colour`
/// match arm covers all of these verbatim plus aliases like `"kw"` and `"str"`.
/// We pass the capture name directly; unknown names fall back to `fg_default`
/// in the GUI which is a safe, legible colour.
/// The fall-through arm returns the capture name itself, so the result borrows
/// from the input rather than being `'static`. That is deliberate: the GUI's
/// `class_colour` has a legible default, so an unrecognised capture should be
/// passed through verbatim rather than flattened to one bucket — a grammar
/// update that adds a capture name then degrades gracefully instead of
/// silently recolouring every new token as plain text.
fn capture_class(capture_name: &str) -> &str {
    // The `#[rustfmt::skip]` keeps the exhaustive-looking table readable.
    // Every entry here is a case where we want to remap the grammar's name to
    // the class the GUI explicitly styles.  Anything not listed passes through
    // unchanged via the final arm.
    match capture_name {
        // Grammar convention                 → GUI class_colour arm
        "keyword" => "keyword",
        "keyword.control" => "keyword",
        "keyword.operator" => "keyword",
        "keyword.function" => "keyword",
        "keyword.storage" => "keyword",
        "keyword.other" => "keyword",
        "storage" => "keyword",
        "storage.type" => "keyword",
        "storage.modifier" => "keyword",
        "type" => "type",
        "type.builtin" => "type.builtin",
        "type.definition" => "type",
        "class" => "type",
        "struct" => "type",
        "interface" => "type",
        "function" => "function",
        "function.method" => "function.method",
        "function.builtin" => "function",
        "function.macro" => "macro",
        "method" => "function.method",
        "method.call" => "function.method",
        "constructor" => "function",
        "comment" => "comment",
        "comment.documentation" => "comment.doc",
        "comment.line" => "comment",
        "comment.block" => "comment",
        "string" => "string",
        "string.special" => "string.special",
        "string.escape" => "string",
        "char_literal" | "char" => "char",
        "escape_sequence" => "string",
        "number" => "number",
        "integer_literal" | "float_literal" => "number",
        "constant" => "constant",
        "constant.builtin" => "constant",
        "boolean" => "boolean",
        "boolean_literal" => "boolean",
        "variable" => "variable",
        "variable.builtin" => "variable",
        "variable.parameter" => "variable",
        "property" => "property",
        "field" => "field",
        "field_identifier" => "field",
        "attribute" => "attribute",
        "attribute_item" => "attribute",
        "inner_attribute_item" => "attribute",
        "label" => "variable",
        "lifetime" => "variable",
        "operator" => "operator",
        "punctuation" => "punctuation",
        "punctuation.bracket" => "punctuation",
        "punctuation.delimiter" => "punctuation",
        "macro" => "macro",
        "macro_invocation" => "macro",
        // Pass-through: the GUI will apply the `_` arm (fg_default).
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Public highlighting entry point
// ---------------------------------------------------------------------------

/// Compute syntax-highlight spans for a code block.
///
/// Returns a list of non-overlapping, byte-offset spans sorted by `start`.
/// For an unknown language or a parse failure the list is empty — the caller
/// should emit a `Highlight` event with zero spans rather than skipping it,
/// so the GUI knows highlighting has been attempted (not merely pending).
///
/// # Span ordering guarantee
///
/// Tree-sitter's query cursor visits captures in document order.  However,
/// overlapping captures are possible (e.g. `@keyword` on `fn` and `@function`
/// on the whole function signature).  We deduplicate by keeping, for each byte
/// position, the *first* (most specific, innermost) capture — the tree-sitter
/// grammar convention is that more-specific patterns come first in the query.
///
/// The output is guaranteed to be:
/// 1. Sorted by `start` (ascending).
/// 2. Non-overlapping (the next span's `start >= previous span's `end`).
/// 3. Non-empty spans only (`start < end`).
pub fn highlight(source: &str, lang_tag: &str) -> Vec<HighlightSpan> {
    let Some(lang) = classify_lang(lang_tag) else {
        // Unknown language — return empty; not an error.
        return Vec::new();
    };

    let cfg = config_for(lang);

    // If the query failed to compile (shouldn't happen with bundled grammars,
    // but treated as a soft failure — see module-level comment).
    let Some(query) = cfg.query() else {
        return Vec::new();
    };

    // `Parser` is not `Send`/`Sync` (it holds C heap state), so we create one
    // per call.  Tree-sitter parsers are extremely cheap to allocate (a few
    // hundred bytes) — the grammar itself is a static C object already loaded.
    let mut parser = Parser::new();

    // `set_language` fails if the grammar's ABI version is incompatible with
    // the runtime.  With pinned crate versions this never fires, but we treat
    // it as a soft failure to avoid `unwrap()` on third-party code.
    if parser.set_language(&cfg.language).is_err() {
        return Vec::new();
    }

    // `parse` returns `None` only if the parser was cancelled or timed out
    // (neither of which we configure), so the `?`-equivalent here is the only
    // reasonable failure mode: a zero-byte source.
    let Some(tree) = parser.parse(source.as_bytes(), None) else {
        return Vec::new();
    };

    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, tree.root_node(), source.as_bytes());

    // Accumulate spans, skipping any that overlap with already-emitted spans.
    // `last_end` tracks the exclusive end of the last accepted span; a new
    // span that starts before `last_end` is overlapping and discarded.
    //
    // Why discard overlapping rather than truncating: truncating would split a
    // token mid-byte, which corrupts multi-byte UTF-8 sequences.  Discarding
    // is safe because the GUI's character-paint loop is order-dependent and
    // expects non-overlapping intervals; a missed colour on overlap is
    // invisible to the user compared to a garbled glyph.
    let mut spans: Vec<HighlightSpan> = Vec::new();
    let mut last_end: u32 = 0;

    let capture_names = query.capture_names();

    while let Some((mat, cap_idx)) = captures.next() {
        let capture = &mat.captures[*cap_idx];
        let node = capture.node;

        let start = node.start_byte() as u32;
        let end = node.end_byte() as u32;

        // Zero-length spans are meaningless (no bytes to paint).
        if start >= end {
            continue;
        }

        // Overlapping spans: keep the first, skip the rest.
        if start < last_end {
            continue;
        }

        let name = capture_names[capture.index as usize];
        let class = capture_class(name);

        spans.push(HighlightSpan {
            start,
            end,
            class: SharedStr::from(class),
        });

        last_end = end;
    }

    spans
}

// ---------------------------------------------------------------------------
// Helper that the doc stream uses
// ---------------------------------------------------------------------------

/// Extract code text and language tag from a `RenderSection`, if it is a
/// code-bearing section.
///
/// Returns `(section_id, source_text, lang_tag)` for:
/// - `RenderSection::CodeBlock` — the top-level standalone code block.
/// - Any `ProseBlock::Code` inside `Prose`, `Examples`, or `Callout` — these
///   are fenced code blocks embedded in documentation.
///
/// Returning a `Vec` rather than a single value because a single section can
/// contain multiple prose code blocks (a section with two ```` ```rust ````
/// fences), and each one independently benefits from highlighting.
///
/// The caller emits one `Highlight` event per section with all spans
/// concatenated and offset-adjusted.  Specifically: prose code blocks are not
/// independently addressable by `SectionId`, so the `Highlight` event for
/// their containing section must carry *all* spans for *all* code blocks in
/// that section, with byte offsets relative to the section's *first* code
/// block.  This is the simplest contract the GUI can rely on — one
/// `SectionId` → one span list — and it avoids inventing a sub-section
/// addressing scheme.
///
/// For standalone `CodeBlock` sections there is always exactly one code
/// source, which keeps the common case simple.
pub fn code_sources(
    section: &nudox_engine::wire::RenderSection,
) -> Vec<(nudox_engine::wire::SectionId, &str, &str)> {
    use nudox_engine::wire::{ProseBlock, RenderSection};

    match section {
        RenderSection::CodeBlock { id, lang, text, .. } => {
            vec![(*id, text.as_ref(), lang.0.as_ref())]
        }
        RenderSection::Prose { id, blocks }
        | RenderSection::Examples { id, blocks }
        | RenderSection::Callout { id, blocks, .. } => {
            // Collect all fenced code blocks inside this prose section.
            blocks
                .iter()
                .filter_map(|b| {
                    if let ProseBlock::Code { lang, text, .. } = b {
                        Some((*id, text.as_ref(), lang.0.as_ref()))
                    } else {
                        None
                    }
                })
                .collect()
        }
        // Members and Fields contain no code text to highlight.
        //
        // `RenderSection` is `#[non_exhaustive]` (LD-7: a newer engine may add
        // section kinds this binary has never heard of), so the wildcard is
        // required and is the correct behaviour: an unrecognised section
        // renders without colour rather than failing to render.
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Assert that `spans` is non-overlapping and in ascending `start` order.
    ///
    /// This is the invariant the GUI depends on for its sequential paint loop.
    fn assert_ordered_nonoverlapping(spans: &[HighlightSpan]) {
        for window in spans.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            assert!(
                a.start < a.end,
                "span {:?}..{:?} is empty or inverted",
                a.start,
                a.end
            );
            assert!(
                b.start >= a.end,
                "spans overlap: {:?}..{:?} vs {:?}..{:?}",
                a.start,
                a.end,
                b.start,
                b.end
            );
        }
    }

    // ── Content assertions ────────────────────────────────────────────────────

    /// For `pub fn foo() -> u32 { 1 }`, `pub` and `fn` must be keyword spans
    /// and `u32` must be a type span.
    ///
    /// This test validates the full pipeline: parse → query → span map → class
    /// assignment.  If the grammar ships a different capture name for these
    /// tokens, the `capture_class` mapping must be updated accordingly.
    #[test]
    fn rust_keyword_and_type_spans() {
        let src = "pub fn foo() -> u32 { 1 }";
        let spans = highlight(src, "rust");

        // Locate the span covering `pub`.
        let pub_start = src.find("pub").unwrap() as u32;
        let pub_span = spans.iter().find(|s| s.start == pub_start);
        assert!(
            pub_span.is_some(),
            "`pub` at byte {pub_start} has no highlight span; got: {spans:?}"
        );
        assert_eq!(
            &*pub_span.unwrap().class,
            "keyword",
            "`pub` must be classified as `keyword`"
        );

        // Locate the span covering `fn`.
        let fn_start = src.find("fn").unwrap() as u32;
        let fn_span = spans.iter().find(|s| s.start == fn_start);
        assert!(
            fn_span.is_some(),
            "`fn` at byte {fn_start} has no highlight span; got: {spans:?}"
        );
        assert_eq!(
            &*fn_span.unwrap().class,
            "keyword",
            "`fn` must be classified as `keyword`"
        );

        // Locate the span covering `u32`.
        let u32_start = src.find("u32").unwrap() as u32;
        let u32_span = spans.iter().find(|s| s.start == u32_start);
        assert!(
            u32_span.is_some(),
            "`u32` at byte {u32_start} has no highlight span; got: {spans:?}"
        );
        assert!(
            &*u32_span.unwrap().class == "type" || &*u32_span.unwrap().class == "type.builtin",
            "`u32` must be classified as `type` or `type.builtin`, got `{}`",
            u32_span.unwrap().class
        );
    }

    /// A Rust comment must be classified as `comment` or `comment.doc`.
    #[test]
    fn rust_comment_span() {
        let src = "// a comment\nfn bar() {}";
        let spans = highlight(src, "rust");
        let comment_start = 0u32;
        let comment_span = spans.iter().find(|s| s.start == comment_start);
        assert!(
            comment_span.is_some(),
            "comment at byte 0 has no highlight span; got: {spans:?}"
        );
        assert!(
            comment_span.unwrap().class.starts_with("comment"),
            "comment must be classified as `comment*`, got `{}`",
            comment_span.unwrap().class
        );
    }

    /// A Rust string literal must be classified as `string`.
    #[test]
    fn rust_string_span() {
        let src = r#"let s = "hello";"#;
        let spans = highlight(src, "rust");
        let str_start = src.find('"').unwrap() as u32;
        let str_span = spans.iter().find(|s| s.start == str_start);
        assert!(
            str_span.is_some(),
            "string literal at byte {str_start} has no highlight span; got: {spans:?}"
        );
        assert!(
            str_span.unwrap().class.starts_with("string"),
            "string literal must be classified as `string*`, got `{}`",
            str_span.unwrap().class
        );
    }

    /// `highlight` for an unknown language tag must return zero spans and not
    /// panic.  This is a hard requirement: doc blocks can reference any language
    /// and the engine must never crash on unrecognised tags.
    #[test]
    fn unknown_language_returns_empty() {
        let src = "SELECT * FROM foo WHERE id = 1;";
        let spans = highlight(src, "sql");
        assert!(
            spans.is_empty(),
            "unknown language `sql` must yield empty spans; got: {spans:?}"
        );
    }

    /// Empty language tag must also return zero spans without panicking.
    #[test]
    fn empty_language_tag_returns_empty() {
        let spans = highlight("x = 1", "");
        assert!(spans.is_empty());
    }

    /// Spans must be non-overlapping and in ascending start order.
    ///
    /// The GUI applies spans sequentially: if two spans cover the same byte,
    /// the second silently clobbers the first with an incorrect colour.  Worse,
    /// overlapping spans at a UTF-8 codepoint boundary corrupt the rendered
    /// glyph (painting half a multi-byte character in two different colours).
    #[test]
    fn rust_spans_are_ordered_and_nonoverlapping() {
        let src = r#"
pub struct Foo {
    x: u32,
    y: f64,
}

impl Foo {
    /// Create a new Foo.
    pub fn new(x: u32, y: f64) -> Self {
        Self { x, y }
    }
}
"#;
        let spans = highlight(src, "rust");
        assert!(!spans.is_empty(), "non-trivial Rust must produce spans");
        assert_ordered_nonoverlapping(&spans);
    }

    /// Python keywords and types must be classified correctly.
    #[test]
    fn python_keyword_and_type_spans() {
        let src = "def greet(name: str) -> None:\n    return name";
        let spans = highlight(src, "python");
        assert!(!spans.is_empty(), "Python must produce spans");

        // `def` must be a keyword.
        let def_start = src.find("def").unwrap() as u32;
        let def_span = spans.iter().find(|s| s.start == def_start);
        assert!(
            def_span.is_some(),
            "`def` at byte {def_start} has no highlight span; got: {spans:?}"
        );
        assert_eq!(
            &*def_span.unwrap().class,
            "keyword",
            "`def` must be classified as `keyword`"
        );

        assert_ordered_nonoverlapping(&spans);
    }

    /// Go keywords must be classified correctly.
    #[test]
    fn go_keyword_spans() {
        let src = "func add(a, b int) int {\n\treturn a + b\n}";
        let spans = highlight(src, "go");
        assert!(!spans.is_empty(), "Go must produce spans");

        let func_start = src.find("func").unwrap() as u32;
        let func_span = spans.iter().find(|s| s.start == func_start);
        assert!(
            func_span.is_some(),
            "`func` at byte {func_start} has no highlight span; got: {spans:?}"
        );
        assert_eq!(
            &*func_span.unwrap().class,
            "keyword",
            "`func` must be classified as `keyword`"
        );

        assert_ordered_nonoverlapping(&spans);
    }

    /// The `rs` alias must resolve to Rust highlighting.
    #[test]
    fn rs_alias_resolves_to_rust() {
        let src = "fn main() {}";
        let via_rust = highlight(src, "rust");
        let via_rs = highlight(src, "rs");
        assert_eq!(
            via_rust.len(),
            via_rs.len(),
            "`rust` and `rs` must produce the same spans"
        );
    }

    /// All spans must have `start < end` (no zero-length spans).
    #[test]
    fn no_empty_spans() {
        let src = "pub fn foo() -> u32 { 1 }";
        let spans = highlight(src, "rust");
        for s in &spans {
            assert!(s.start < s.end, "empty span: {:?}..{:?}", s.start, s.end);
        }
    }
}

// ---------------------------------------------------------------------------
// The engine seam
// ---------------------------------------------------------------------------

/// `lindsey`'s implementation of the engine's highlighting capability.
///
/// The engine cannot link a tree-sitter runtime of its own — it compiles into
/// two workspaces that already have different, mutually exclusive owners of the
/// `tree-sitter` C library (see `nudox_engine::highlight`). So the host that
/// actually renders colour supplies it, using the grammars its own graph
/// already carries.
#[derive(Debug, Default, Clone, Copy)]
pub struct TreeSitterHighlighter;

impl Highlighter for TreeSitterHighlighter {
    fn highlight(&self, source: &str, language: &str) -> Vec<HighlightSpan> {
        highlight(source, language)
    }
}
