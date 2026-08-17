//! JSDoc extraction (ported from `workspace/compiler/compile/typescript/oxc/extract/jsdoc.rs`).
//!
//! Carries over nearly verbatim; the only change is the output type:
//! `DocFacts` now uses `nudox_ir::entry::Deprecation` directly (owned),
//! rather than the old `ir::kind::Deprecation`.
//!
//! ## Finder key mechanism
//!
//! `semantic.jsdoc().get_one_by_node()` is keyed by span.start. For TS-only
//! AST kinds (TSInterfaceDeclaration, TSTypeAliasDeclaration, …) that OXC's
//! `should_attach_jsdoc` list excludes, we fall back to:
//!   1. `get_all_by_span` on the node's span start.
//!   2. Raw comment table: `attached_to == span.start`.
//!   3. Leading-trivia scan: nearest preceding JSDoc with only export/declare
//!      whitespace between comment end and node start.

use oxc_ast::ast::{Comment, Program};
use oxc_jsdoc::parser::JSDoc;
use oxc_semantic::Semantic;
use oxc_span::{GetSpan, Span};
use oxc_syntax::node::NodeId;

use super::{DeprecationOwned, DocFacts};

// ── Public helpers ────────────────────────────────────────────────────────────

/// Resolve JSDoc attached to `node_id` within `semantic`.
pub fn jsdoc_for_node<'a>(semantic: &'a Semantic<'a>, node_id: NodeId) -> DocFacts {
    if node_id == NodeId::DUMMY {
        return DocFacts::default();
    }
    let nodes = semantic.nodes();
    let node = nodes.get_node(node_id);

    if let Some(jsdoc) = semantic.jsdoc().get_one_by_node(nodes, node) {
        return extract_doc_facts(jsdoc);
    }

    jsdoc_for_span(semantic, node.kind().span())
}

/// Resolve JSDoc for an arbitrary span (fallback for TS-only AST kinds).
pub fn jsdoc_for_span<'a>(semantic: &'a Semantic<'a>, span: Span) -> DocFacts {
    if let Some(docs) = semantic.jsdoc().get_all_by_span(span)
        && let Some(jsdoc) = docs.last()
    {
        return extract_doc_facts(jsdoc.clone());
    }

    let source = semantic.source_text();
    let node_start = span.start;

    if let Some(jsdoc) = comment_jsdoc_at(semantic, node_start) {
        return extract_doc_facts(jsdoc);
    }

    if let Some(jsdoc) = leading_jsdoc_before(semantic, node_start, source) {
        return extract_doc_facts(jsdoc);
    }

    DocFacts::default()
}

/// Module documentation: first `/**` block with `@module` tag.
pub fn module_doc<'a>(semantic: &'a Semantic<'a>, program: &Program<'a>) -> Option<String> {
    let source = semantic.source_text();
    for comment in program.comments.iter() {
        if !comment.is_jsdoc() {
            continue;
        }
        let jsdoc = parse_comment_jsdoc(comment, source);
        if !jsdoc.tags().iter().any(|t| t.kind.parsed() == "module") {
            continue;
        }
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
        return if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
    }
    None
}

// ── Direct JSDoc parsing (for unit tests) ─────────────────────────────────────

/// Parse a raw JSDoc comment string (the inner text without `/**` / `*/`)
/// and return `DocFacts`.
pub fn parse_raw_jsdoc(raw: &str) -> DocFacts {
    let span = Span::new(0, raw.len() as u32);
    let jsdoc = JSDoc::new(raw, span);
    extract_doc_facts(jsdoc)
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn comment_jsdoc_at<'a>(semantic: &'a Semantic<'a>, offset: u32) -> Option<JSDoc<'a>> {
    let source = semantic.source_text();
    semantic.comments().iter().find_map(|c| {
        if c.is_jsdoc() && c.attached_to == offset {
            Some(parse_comment_jsdoc(c, source))
        } else {
            None
        }
    })
}

fn leading_jsdoc_before<'a>(
    semantic: &'a Semantic<'a>,
    node_start: u32,
    source: &'a str,
) -> Option<JSDoc<'a>> {
    let mut best: Option<(u32, JSDoc<'a>)> = None;
    for c in semantic.comments().iter() {
        if !c.is_jsdoc() {
            continue;
        }
        if c.span.end > node_start || c.attached_to > node_start {
            continue;
        }
        if !is_leading_export_trivia(source, c.span.end, node_start) {
            continue;
        }
        let jsdoc = parse_comment_jsdoc(c, source);
        match &best {
            Some((prev_end, _)) if *prev_end >= c.span.end => {}
            _ => best = Some((c.span.end, jsdoc)),
        }
    }
    best.map(|(_, j)| j)
}

fn parse_comment_jsdoc<'a>(comment: &Comment, source: &'a str) -> JSDoc<'a> {
    let content_span = comment.content_span();
    let jsdoc_span = Span::new(content_span.start + 1, content_span.end);
    JSDoc::new(jsdoc_span.source_text(source), jsdoc_span)
}

fn extract_doc_facts(jsdoc: JSDoc<'_>) -> DocFacts {
    let tags = jsdoc.tags();
    let mut description_override: Option<String> = None;
    let mut deprecation: Option<DeprecationOwned> = None;
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
                deprecation = Some(DeprecationOwned { note, since: None });
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
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
    };

    DocFacts {
        doc,
        deprecation,
        ignore,
    }
}

fn is_leading_export_trivia(source: &str, from: u32, to: u32) -> bool {
    if from > to {
        return false;
    }
    let (from, to) = (from as usize, to as usize);
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
