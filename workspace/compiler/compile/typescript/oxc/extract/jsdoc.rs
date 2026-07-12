//! JSDoc extraction (OXC-PLAN §1.3, §5.4). Ports deno_doc's *semantics* on top
//! of oxc's tag splitter: leading-`*` strip (handled by `JSDocCommentPart::parsed()`),
//! `@ignore` suppression, `@module` module-doc marker, `@deprecated` → ir deprecation.
//!
//! ## Finder key mechanism (verified against vendored oxc 0.139.0 source)
//!
//! `JSDocFinder::get_one_by_node` takes `(nodes: &AstNodes<'a>, node: &AstNode<'a>)`,
//! NOT a bare `NodeId` as the OXC-API-NOTES.md's abbreviated signature suggests.
//! Internally it:
//!   1. Checks `nodes.flags(node.id()).has_jsdoc()` — fast path if no JSDoc flag set.
//!   2. Keys the `attached` map by `node.kind().span().start` (a `u32`).
//!
//! So attachment is keyed by the **start byte offset of the node's span**.
//! The builder (`JSDocBuilder`) groups JSDoc `/**`-block comments by their
//! `comment.attached_to` field (also a start offset), set during the semantic walk via
//! `retrieve_attached_jsdoc` which is called only for `should_attach_jsdoc` node kinds.
//!
//! Our `jsdoc_for_node` path:
//!   1. Primary: `semantic.jsdoc().get_one_by_node(semantic.nodes(), nodes.get_node(node_id))`.
//!   2. Fallback: manual scan of `semantic.comments()` filtered by `is_jsdoc()` and
//!      `attached_to == node_span_start`, re-parsed via `JSDoc::new`. Needed for node
//!      kinds not listed in `should_attach_jsdoc` (e.g. some TS-specific declarations).
//!
//! For `module_doc` there is no declaration node to key from; we scan `program.comments`
//! for the first `/**` block whose parsed tags contain `@module` (deno_doc semantics).

use oxc_ast::ast::Program;
use oxc_jsdoc::parser::JSDoc;
use oxc_span::{GetSpan, Span};
use oxc_syntax::node::NodeId;

use ir::kind::Deprecation;

use super::{DocFacts, Extractor};

impl<'a> Extractor<'a> {
    /// Resolve the JSDoc attached to `node_id` into [`DocFacts`]: description
    /// text, `@deprecated` → [`Deprecation`], `@ignore` flag.
    ///
    /// Uses `self.semantic.jsdoc().get_one_by_node(nodes, node)` as the primary
    /// path; falls back to a manual scan of `self.semantic.comments()` keyed by
    /// `attached_to == node.kind().span().start` when the finder returns `None`
    /// (i.e. the node kind was not pre-flagged during the semantic walk).
    pub(crate) fn jsdoc_for_node(&self, node_id: NodeId) -> DocFacts {
        let nodes = self.semantic.nodes();
        let node = nodes.get_node(node_id);

        // Primary path via the pre-indexed finder.
        let jsdoc_opt = self.semantic.jsdoc().get_one_by_node(nodes, node);

        // Fallback: manual scan when the node kind was not pre-flagged.
        let jsdoc_opt = jsdoc_opt.or_else(|| {
            let node_start = node.kind().span().start;
            let source = self.semantic.source_text();
            self.semantic
                .comments()
                .iter()
                .find(|c| c.is_jsdoc() && c.attached_to == node_start)
                .map(|c| {
                    let content_span = c.content_span();
                    // content_span covers `/*` to `*/`; strip one extra byte for the
                    // leading `*` that makes this a JSDoc block (`/**`).
                    let jsdoc_span = Span::new(content_span.start + 1, content_span.end);
                    JSDoc::new(jsdoc_span.source_text(source), jsdoc_span)
                })
        });

        match jsdoc_opt {
            Some(jsdoc) => Self::extract_doc_facts(jsdoc),
            None => DocFacts::default(),
        }
    }

    /// Module documentation = the first `/**` block whose parsed tags contain
    /// `@module` (deno_doc semantics — NOT merely the first comment).
    ///
    /// Returns the stripped description text of that block, or `None` if no
    /// `@module` JSDoc is present.
    pub(crate) fn module_doc(&self, program: &Program<'a>) -> Option<String> {
        let source = self.semantic.source_text();

        for comment in program.comments.iter() {
            if !comment.is_jsdoc() {
                continue;
            }

            let content_span = comment.content_span();
            // Strip the extra `*` that distinguishes `/**` from `/*`.
            let jsdoc_span = Span::new(content_span.start + 1, content_span.end);
            let jsdoc = JSDoc::new(jsdoc_span.source_text(source), jsdoc_span);

            if !jsdoc.tags().iter().any(|t| t.kind.parsed() == "module") {
                continue;
            }

            // Found the @module block. @description overrides the body text.
            let desc_override = jsdoc
                .tags()
                .iter()
                .find(|t| t.kind.parsed() == "description")
                .map(|t| t.comment().parsed());

            let text = match desc_override {
                Some(d) => d,
                None => jsdoc.comment().parsed(),
            };

            let trimmed = text.trim().to_string();
            return if trimmed.is_empty() { None } else { Some(trimmed) };
        }

        None
    }

    /// Inner helper: extract [`DocFacts`] from an already-parsed [`JSDoc`] block.
    ///
    /// - `doc`        — description body, overridden by `@description` when present.
    /// - `deprecation`— set when `@deprecated` is found; `note` carries the tag's
    ///                  comment text (trimmed, `None` if empty); `since` is always
    ///                  `None` (`@since` is a separate tag — future headroom).
    /// - `ignore`     — `true` when `@ignore` is present.
    ///
    /// All leading-`*` stripping is handled by `JSDocCommentPart::parsed()`.
    fn extract_doc_facts(jsdoc: JSDoc<'a>) -> DocFacts {
        let tags = jsdoc.tags();

        let mut description_override: Option<String> = None;
        let mut deprecation: Option<Deprecation> = None;
        let mut ignore = false;

        for tag in tags.iter() {
            match tag.kind.parsed() {
                "description" => {
                    let trimmed = tag.comment().parsed().trim().to_string();
                    if !trimmed.is_empty() {
                        description_override = Some(trimmed);
                    }
                }
                "deprecated" => {
                    let note_raw = tag.comment().parsed();
                    let note = {
                        let t = note_raw.trim().to_string();
                        if t.is_empty() { None } else { Some(t) }
                    };
                    deprecation = Some(Deprecation { note, since: None });
                }
                "ignore" => {
                    ignore = true;
                }
                _ => {}
            }
        }

        let doc = match description_override {
            Some(desc) => Some(desc),
            None => {
                let body = jsdoc.comment().parsed();
                let trimmed = body.trim().to_string();
                if trimmed.is_empty() { None } else { Some(trimmed) }
            }
        };

        DocFacts { doc, deprecation, ignore }
    }
}
