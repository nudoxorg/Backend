//! The syntax-highlighting seam (§9.3).
//!
//! # Why this is a trait and not an implementation
//!
//! Highlighting needs a tree-sitter runtime, and this crate cannot name one.
//! `nudox-engine` is compiled into two dependency graphs that each already have
//! a different owner of the tree-sitter C library:
//!
//! | graph                | owner                  | pulled in by                       |
//! |----------------------|------------------------|------------------------------------|
//! | root workspace       | `arborium-tree-sitter` | `workspace/driver`, `workspace/index` |
//! | `lindsey` workspace  | upstream `tree-sitter` | `gpui-component` (pinned by rev)   |
//!
//! Both declare `links = "tree-sitter"`, and cargo permits exactly one package
//! per `links` value in a graph. Crucially that is enforced at *resolve* time,
//! so an `optional = true` dependency does not escape it — merely naming either
//! crate here makes one of the two workspaces fail to resolve. Neither owner is
//! removable: gpui-component's is a pinned git rev, and driver/index are
//! load-bearing root members.
//!
//! So the engine keeps what it can actually own — *when* a section gets
//! highlighted, and the protocol ordering guarantee that a `Highlight` never
//! precedes its `Section` — and the host supplies *how*. LR-3 is intact: the
//! engine still decides that IR becomes presentation and when. It just does not
//! link a C parser to do it.
//!
//! # No highlighter is a supported state
//!
//! `nudox-mcp` serves text and installs none. Sections still get an empty
//! `Highlight` event rather than none at all, so a consumer's state machine is
//! identical either way: uncoloured code, never code stuck waiting for a
//! highlight that will never arrive.

use std::sync::Arc;

use crate::wire::HighlightSpan;

/// Turns a source fragment into highlight spans.
///
/// Implementations must be total and infallible: an unknown language, a parse
/// failure, or a malformed grammar query all yield an empty span list. A
/// documentation stream must never fail because colour was unavailable.
///
/// Spans must be **non-overlapping and in ascending byte order**. Consumers
/// apply them in sequence to slice the source, so an overlapping or unsorted
/// span list corrupts the rendered text rather than merely miscolouring it.
pub trait Highlighter: Send + Sync + 'static {
    /// Spans for `source`, interpreted as `language`.
    ///
    /// `language` is the fenced-code-block info string as it appeared in the
    /// doc comment (`rust`, `py`, `Go`, …) — normalise case and aliases in the
    /// implementation, not at the call site.
    fn highlight(&self, source: &str, language: &str) -> Vec<HighlightSpan>;
}

/// A shared highlighter, or none.
///
/// `Option` rather than a null-object default because "this build has no
/// grammars" is a real, supported configuration worth being able to see in a
/// type rather than inferring from empty output.
pub type SharedHighlighter = Option<Arc<dyn Highlighter>>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A highlighter that claims a span past the end of the input would make
    /// the consumer slice out of bounds. The trait cannot enforce its contract
    /// at compile time, so this test documents it against a stub — and gives
    /// implementations in other crates something to copy.
    struct Stub;

    impl Highlighter for Stub {
        fn highlight(&self, source: &str, language: &str) -> Vec<HighlightSpan> {
            if language != "rust" || source.is_empty() {
                return Vec::new();
            }
            vec![HighlightSpan {
                start: 0,
                end: source.len().min(3) as u32,
                class: "keyword".into(),
            }]
        }
    }

    #[test]
    fn an_unknown_language_yields_no_spans() {
        assert!(
            Stub.highlight("fn main() {}", "brainfuck").is_empty(),
            "an unsupported language must degrade to plain text, not error"
        );
    }

    #[test]
    fn spans_stay_within_the_source() {
        let src = "fn";
        for span in Stub.highlight(src, "rust") {
            assert!(
                span.end as usize <= src.len(),
                "a span past the end of the source makes the consumer slice \
                 out of bounds — this is a corruption bug, not a colour bug"
            );
        }
    }

    #[test]
    fn a_missing_highlighter_is_representable() {
        let none: SharedHighlighter = None;
        assert!(none.is_none(), "no-grammar builds are a supported state");
        let some: SharedHighlighter = Some(Arc::new(Stub));
        assert!(some.is_some());
    }
}
