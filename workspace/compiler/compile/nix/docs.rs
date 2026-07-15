//! Clean-room doc-comment extraction and structured parsing.
//!
//! The extraction *algorithm* is shared by nix-doc, nixdoc, pesto, and CppNix
//! 2.24 (backward sibling scan skipping whitespace, take the first comment,
//! binding-beats-lambda per RFC 145) — but every one of those is GPL, so this
//! is written from the spec, not ported.
//!
//! Two conventions are recognised and unified into one markdown body plus a
//! structured side-channel (`# Type` signature, per-argument docs, examples):
//!
//!   * **RFC 145** — `/** CommonMark */` with nixdoc's section convention
//!     (`# Arguments`, `# Type` fenced block, `# Examples`).
//!   * **Legacy** — `/* Description … Type: f :: a -> b   Example: … */` plus
//!     `# argname` line comments on formals.

use std::collections::HashMap;

use rnix::{SyntaxKind, SyntaxNode};
use rowan::NodeOrToken;

/// A doc comment split into its markdown body and structured side-channels.
#[derive(Debug, Clone, Default)]
pub struct ParsedDoc {
    /// The recombined human-readable markdown (description + examples).
    pub markdown:  String,
    /// The raw `::` type signature string (from a `# Type` fenced block or a
    /// legacy `Type:` line), handed to `sig` for parsing.
    pub type_sig:  Option<String>,
    /// Per-argument documentation, keyed by formal name (RFC 145 `# Arguments`
    /// bullets, or legacy `# argname` comments merged in by the lambda walk).
    pub arg_docs:  HashMap<String, String>,
    /// Example blocks, in source order.
    pub examples:  Vec<String>,
}

impl ParsedDoc {
    /// Whether this carries any content at all.
    pub fn is_empty(&self) -> bool {
        self.markdown.is_empty()
            && self.type_sig.is_none()
            && self.arg_docs.is_empty()
            && self.examples.is_empty()
    }

    /// Human-facing documentation markdown: body plus any example blocks
    /// folded under an `## Examples` heading. Empty when neither body nor
    /// examples are present.
    pub fn to_documentation(&self) -> Option<String> {
        let mut out = self.markdown.trim().to_string();
        if !self.examples.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str("## Examples\n\n");
            for (i, ex) in self.examples.iter().enumerate() {
                if i > 0 {
                    out.push_str("\n\n");
                }
                let trimmed = ex.trim();
                // Preserve fenced blocks as-is; wrap bare examples in a fence.
                if trimmed.starts_with("```") {
                    out.push_str(trimmed);
                } else {
                    out.push_str("```nix\n");
                    out.push_str(trimmed);
                    out.push_str("\n```");
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// Find the raw (dedented, marker-stripped) doc comment attached to a
/// documentable node: scan backwards over siblings, skip whitespace, and take
/// the first comment. `#`-runs are coalesced into one block.
pub fn raw_doc_for(node: &SyntaxNode) -> Option<String> {
    let mut element = node.prev_sibling_or_token();

    // Skip intervening whitespace (but bail if a blank line separates us — an
    // unrelated comment further up should not attach).
    while let Some(NodeOrToken::Token(tok)) = &element {
        match tok.kind() {
            SyntaxKind::TOKEN_WHITESPACE => {
                if tok.text().matches('\n').count() >= 2 {
                    return None;
                }
                element = tok.prev_sibling_or_token();
            }
            SyntaxKind::TOKEN_COMMENT => break,
            _ => return None,
        }
    }

    let NodeOrToken::Token(comment) = element? else { return None };
    if comment.kind() != SyntaxKind::TOKEN_COMMENT {
        return None;
    }

    // A `#` line comment: coalesce the contiguous run above it.
    let text = comment.text();
    if text.starts_with('#') {
        let mut lines = vec![strip_hash(text)];
        let mut cursor = comment.prev_sibling_or_token();
        // Walk further up while we see (single-newline whitespace, # comment)*.
        loop {
            match cursor {
                Some(NodeOrToken::Token(t)) if t.kind() == SyntaxKind::TOKEN_WHITESPACE => {
                    if t.text().matches('\n').count() >= 2 {
                        break;
                    }
                    cursor = t.prev_sibling_or_token();
                }
                Some(NodeOrToken::Token(t))
                    if t.kind() == SyntaxKind::TOKEN_COMMENT && t.text().starts_with('#') =>
                {
                    lines.push(strip_hash(t.text()));
                    cursor = t.prev_sibling_or_token();
                }
                _ => break,
            }
        }
        lines.reverse();
        let joined = lines.join("\n");
        let trimmed = dedent(&joined);
        return (!trimmed.trim().is_empty()).then_some(trimmed);
    }

    // A `/* */` or `/** */` block comment.
    let body = strip_block(text);
    let trimmed = dedent(&body);
    (!trimmed.trim().is_empty()).then_some(trimmed)
}

/// The `# argname`-style doc for a single formal (legacy convention): the
/// comment immediately preceding a pattern entry node.
pub fn formal_doc_for(entry_node: &SyntaxNode) -> Option<String> {
    raw_doc_for(entry_node)
}

/// Parse a raw doc block into markdown + structured side-channels, handling
/// both the RFC 145 section convention and the legacy `Type:`/`Example:` form.
pub fn parse_doc(raw: &str) -> ParsedDoc {
    let mut doc = ParsedDoc::default();

    // nixdoc-style sections are `# Heading` lines at column 0 inside a `/**`.
    if has_sections(raw) {
        parse_sections(raw, &mut doc);
        return doc;
    }

    // Legacy: scan for `Type:` and `Example:` markers line-by-line.
    let mut description = Vec::new();
    let mut mode = LegacyMode::Description;
    let mut example = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("Type:") {
            flush_example(&mut example, &mut doc);
            let sig = rest.trim();
            if !sig.is_empty() {
                doc.type_sig = Some(sig.to_string());
            }
            mode = LegacyMode::Type;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Example:") {
            flush_example(&mut example, &mut doc);
            mode = LegacyMode::Example;
            let rest = rest.trim();
            if !rest.is_empty() {
                example.push(rest.to_string());
            }
            continue;
        }
        match mode {
            LegacyMode::Description => description.push(line.to_string()),
            LegacyMode::Type => {
                // A `Type:` block may span multiple lines; append until blank.
                if trimmed.is_empty() {
                    mode = LegacyMode::Description;
                } else if let Some(existing) = &mut doc.type_sig {
                    existing.push('\n');
                    existing.push_str(trimmed);
                } else {
                    doc.type_sig = Some(trimmed.to_string());
                }
            }
            LegacyMode::Example => example.push(line.to_string()),
        }
    }
    flush_example(&mut example, &mut doc);
    doc.markdown = description.join("\n").trim().to_string();
    doc
}

// ───────────────────────────────────────────────────────────────────────────
// RFC 145 section parsing
// ───────────────────────────────────────────────────────────────────────────

fn has_sections(raw: &str) -> bool {
    raw.lines().any(|l| {
        let t = l.trim();
        t == "# Type" || t == "# Arguments" || t == "# Examples" || t == "# Example"
    })
}

fn parse_sections(raw: &str, doc: &mut ParsedDoc) {
    #[derive(PartialEq)]
    enum Section {
        Body,
        Type,
        Arguments,
        Examples,
    }
    let mut section = Section::Body;
    let mut body = Vec::new();
    let mut type_block = Vec::new();
    let mut in_fence = false;
    let mut arg_lines: Vec<String> = Vec::new();
    let mut example_lines: Vec<String> = Vec::new();

    for line in raw.lines() {
        let t = line.trim();
        match t {
            "# Type" => {
                section = Section::Type;
                continue;
            }
            "# Arguments" => {
                section = Section::Arguments;
                continue;
            }
            "# Examples" | "# Example" => {
                section = Section::Examples;
                continue;
            }
            _ => {}
        }
        match section {
            Section::Body => body.push(line.to_string()),
            Section::Type => {
                if t.starts_with("```") {
                    in_fence = !in_fence;
                    continue;
                }
                if in_fence || !t.is_empty() {
                    type_block.push(t.to_string());
                }
            }
            Section::Arguments => arg_lines.push(line.to_string()),
            Section::Examples => example_lines.push(line.to_string()),
        }
    }

    doc.markdown = body.join("\n").trim().to_string();
    if !type_block.is_empty() {
        doc.type_sig = Some(type_block.join("\n").trim().to_string());
    }
    // Arguments: `: description` bullets keyed by the preceding `foo` term, or
    // markdown definition-list style `foo\n: desc`.
    parse_arg_bullets(&arg_lines, &mut doc.arg_docs);
    let examples = example_lines.join("\n").trim().to_string();
    if !examples.is_empty() {
        doc.examples.push(examples);
    }
}

/// Parse `# Arguments` content. Supports the CommonMark definition-list form
/// nixdoc emits: a term line (`name`) followed by `: description` line(s).
fn parse_arg_bullets(lines: &[String], out: &mut HashMap<String, String>) {
    let mut current: Option<String> = None;
    let mut desc = String::new();
    let mut flush = |current: &mut Option<String>, desc: &mut String, out: &mut HashMap<String, String>| {
        if let Some(name) = current.take() {
            let d = desc.trim().to_string();
            if !d.is_empty() {
                out.insert(name, d);
            }
            desc.clear();
        }
    };
    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix(':') {
            desc.push_str(rest.trim());
            desc.push(' ');
        } else {
            // A new term.
            flush(&mut current, &mut desc, out);
            // Strip common markdown emphasis / backticks around the name.
            let name = t.trim_matches(|c| c == '`' || c == '*' || c == '_').to_string();
            current = Some(name);
        }
    }
    flush(&mut current, &mut desc, out);
}

// ───────────────────────────────────────────────────────────────────────────
// Legacy helpers
// ───────────────────────────────────────────────────────────────────────────

enum LegacyMode {
    Description,
    Type,
    Example,
}

fn flush_example(example: &mut Vec<String>, doc: &mut ParsedDoc) {
    if !example.is_empty() {
        let e = example.join("\n").trim().to_string();
        if !e.is_empty() {
            doc.examples.push(e);
        }
        example.clear();
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Comment-marker stripping & dedent
// ───────────────────────────────────────────────────────────────────────────

fn strip_hash(text: &str) -> String {
    text.trim_start_matches('#').trim_end().to_string()
}

/// Strip `/*`…`*/` (and the RFC-145 extra `*`), plus aligned leading `*`.
fn strip_block(text: &str) -> String {
    let inner = text
        .strip_prefix("/**")
        .or_else(|| text.strip_prefix("/*"))
        .unwrap_or(text);
    let inner = inner.strip_suffix("*/").unwrap_or(inner);
    // Drop aligned leading `* ` decoration common in block comments.
    let lines: Vec<String> = inner
        .lines()
        .map(|l| {
            let trimmed = l.trim_start();
            if let Some(rest) = trimmed.strip_prefix("* ") {
                rest.to_string()
            } else if trimmed == "*" {
                String::new()
            } else {
                l.to_string()
            }
        })
        .collect();
    lines.join("\n")
}

/// Remove the longest common leading-whitespace prefix from all non-blank
/// lines.
fn dedent(text: &str) -> String {
    let indent = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    text.lines()
        .map(|l| if l.len() >= indent { &l[indent..] } else { l })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}
