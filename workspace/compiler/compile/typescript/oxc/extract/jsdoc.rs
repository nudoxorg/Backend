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
//! **Critical gap:** oxc's `should_attach_jsdoc` list does **not** include any
//! TypeScript-only kinds (`TSInterfaceDeclaration`, `TSTypeAliasDeclaration`,
//! `TSPropertySignature`, `TSEnumDeclaration`, …). JSDoc on those forms is either
//! attached to the wrapping `ExportNamedDeclaration` / `ExportDefaultDeclaration`
//! (which *are* listed) or left in `not_attached` / only recoverable via the raw
//! comment's `attached_to` field.
//!
//! Our resolution path therefore:
//!   1. Primary: `semantic.jsdoc().get_one_by_node(…)` when the node is flagged.
//!   2. Span map: `get_all_by_span` on the node's span start.
//!   3. Comment table: any `is_jsdoc()` comment with `attached_to == span.start`.
//!   4. Leading-trivia scan: nearest preceding JSDoc whose gap to the node is only
//!      whitespace + `export`/`default`/`declare`/`async`/`abstract` keywords
//!      (covers `/** … */ export interface Foo`).

use oxc_ast::ast::Program;
use oxc_jsdoc::parser::JSDoc;
use oxc_span::{GetSpan, Span};
use oxc_syntax::node::NodeId;

use ir::kind::Deprecation;

use super::{DocFacts, Extractor};

impl<'a> Extractor<'a> {
    /// Resolve the JSDoc attached to `node_id` into [`DocFacts`].
    pub(crate) fn jsdoc_for_node(&self, node_id: NodeId) -> DocFacts {
        // Guard against dummy / unset node ids from unvisited nodes.
        if node_id == NodeId::DUMMY {
            return DocFacts::default();
        }

        let nodes = self.semantic.nodes();
        // `get_node` panics on out-of-range; guard with len when possible.
        let node = nodes.get_node(node_id);

        // 1. Primary path via the pre-indexed finder.
        if let Some(jsdoc) = self.semantic.jsdoc().get_one_by_node(nodes, node) {
            return Self::extract_doc_facts(jsdoc);
        }

        // 2–4. Span / comment / leading-trivia fallbacks.
        self.jsdoc_for_span(node.kind().span())
    }

    /// Resolve JSDoc for an arbitrary AST span (properties, methods, signatures
    /// that lack a convenient `NodeId` walk, or TS kinds oxc never flags).
    pub(crate) fn jsdoc_for_span(&self, span: Span) -> DocFacts {
        // 2. Finder's attached map keyed by span.start (works when the *parent*
        //    ExportNamedDeclaration was flagged, if we pass that span; also
        //    works for Class PropertyDefinition which *is* listed).
        if let Some(docs) = self.semantic.jsdoc().get_all_by_span(span) {
            if let Some(jsdoc) = docs.last() {
                return Self::extract_doc_facts(jsdoc.clone());
            }
        }

        let source = self.semantic.source_text();
        let node_start = span.start;

        // 3. Raw comment table: attached_to == this span start.
        if let Some(jsdoc) = self.comment_jsdoc_at(node_start) {
            return Self::extract_doc_facts(jsdoc);
        }

        // 4. Leading trivia: nearest preceding JSDoc with only export/declare
        //    keywords between the comment and the node. This recovers docs for
        //    `/** … */ export interface Foo` where attached_to points at `export`.
        if let Some(jsdoc) = self.leading_jsdoc_before(node_start, source) {
            return Self::extract_doc_facts(jsdoc);
        }

        DocFacts::default()
    }

    /// Parse a JSDoc comment whose `attached_to` equals `offset`, if any.
    fn comment_jsdoc_at(&self, offset: u32) -> Option<JSDoc<'a>> {
        let source = self.semantic.source_text();
        self.semantic.comments().iter().find_map(|c| {
            if c.is_jsdoc() && c.attached_to == offset {
                Some(Self::parse_comment_jsdoc(c, source))
            } else {
                None
            }
        })
    }

    /// Nearest preceding JSDoc whose intervening text is only export/declare trivia.
    fn leading_jsdoc_before(&self, node_start: u32, source: &'a str) -> Option<JSDoc<'a>> {
        let mut best: Option<(u32, JSDoc<'a>)> = None;
        for c in self.semantic.comments().iter() {
            if !c.is_jsdoc() {
                continue;
            }
            // Comment must end before the node, and be attached at or before it.
            if c.span.end > node_start {
                continue;
            }
            if c.attached_to > node_start {
                continue;
            }
            // Only whitespace + declaration keywords between comment end and node.
            if !is_leading_export_trivia(source, c.span.end, node_start) {
                continue;
            }
            let jsdoc = Self::parse_comment_jsdoc(c, source);
            match &best {
                Some((prev_end, _)) if *prev_end >= c.span.end => {}
                _ => best = Some((c.span.end, jsdoc)),
            }
        }
        best.map(|(_, j)| j)
    }

    fn parse_comment_jsdoc(comment: &oxc_ast::ast::Comment, source: &'a str) -> JSDoc<'a> {
        let content_span = comment.content_span();
        // content_span covers `/*` to `*/`; strip one extra byte for the
        // leading `*` that makes this a JSDoc block (`/**`).
        let jsdoc_span = Span::new(content_span.start + 1, content_span.end);
        JSDoc::new(jsdoc_span.source_text(source), jsdoc_span)
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

            let jsdoc = Self::parse_comment_jsdoc(comment, source);

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

/// True when `source[from..to]` is only whitespace and declaration-prefix keywords
/// (`export` / `default` / `declare` / `async` / `abstract`). Used to decide
/// whether a preceding JSDoc is the leading doc for a node rather than a sibling's.
fn is_leading_export_trivia(source: &str, from: u32, to: u32) -> bool {
    if from > to {
        return false;
    }
    let from = from as usize;
    let to = to as usize;
    if to > source.len() || from > source.len() {
        return false;
    }
    let between = &source[from..to];
    between.split_whitespace().all(|tok| {
        matches!(
            tok,
            "export" | "default" | "declare" | "async" | "abstract" | "const" | "type"
        )
    })
}
