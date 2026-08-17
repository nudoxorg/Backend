//! Structural javadoc parsing: block tags, inline tags, HTML, JEP 467.
//!
//! Ported faithfully from `workspace/compiler/compile/java/javadoc.rs`.
//! This is pure logic with no IR dependencies — a self-contained parser.
//!
//! Two flavors are handled:
//!
//!   * **Traditional** (`/** … */`): HTML + `{@tag}` inline tags. HTML is
//!     lowered to clean text (paragraphs, list bullets, entity decoding).
//!   * **Markdown** (`/// …`, JEP 467): CommonMark text is preserved as-is;
//!     `[java.util.List]` / `[label][ref]` element references are resolved
//!     and `{@tag}` inline tags (still legal per the JEP) are expanded.
//!
//! Everything here is pure and unit-tested at the bottom of the file.

use std::collections::HashMap;

/// The comment flavor, per JEP 467.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocFlavor {
    /// `/** … */` — HTML plus javadoc tags.
    Traditional,
    /// `/// …` — CommonMark Markdown plus javadoc tags (JEP 467).
    Markdown,
}

/// Decide the flavor from the oracle's `docKind` report.
pub fn flavor(doc_kind: Option<&str>) -> DocFlavor {
    match doc_kind {
        Some("END_OF_LINE") => DocFlavor::Markdown,
        Some(_) | None => DocFlavor::Traditional,
    }
}

/// A structured view of one javadoc comment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedJavadoc {
    pub summary: Option<String>,
    pub body: Option<String>,
    /// `@param name …` descriptions, keyed by parameter name.
    pub params: HashMap<String, String>,
    /// `@param <T> …` descriptions, keyed by type-parameter name (no `<>`).
    pub type_params: HashMap<String, String>,
    pub returns: Option<String>,
    /// `@throws`/`@exception` entries: (resolved exception reference, text).
    pub throws: Vec<(String, String)>,
    /// The `@deprecated` explanation (empty string for a bare tag).
    pub deprecated: Option<String>,
    pub since: Option<String>,
    pub version: Option<String>,
    pub authors: Vec<String>,
    pub see: Vec<String>,
    /// Every `{@link}` / `{@linkplain}` / `[ref]` target encountered.
    pub links: Vec<String>,
}

impl ParsedJavadoc {
    /// Recombine `summary` + `body` into one documentation string.
    pub fn documentation(&self) -> Option<String> {
        match (&self.summary, &self.body) {
            (Some(s), Some(b)) => Some(format!("{s}\n\n{b}")),
            (Some(s), None) => Some(s.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        }
    }

    /// A labelled section for `@param <T>` descriptions, sorted for determinism.
    pub fn type_params_section(&self) -> Option<String> {
        if self.type_params.is_empty() {
            return None;
        }
        let mut entries: Vec<(&String, &String)> = self.type_params.iter().collect();
        entries.sort_by_key(|(name, _)| name.as_str());
        let lines: Vec<String> = entries
            .iter()
            .map(|(name, text)| format!("- `{name}` — {text}"))
            .collect();
        Some(format!("Type parameters:\n{}", lines.join("\n")))
    }

    /// Whether this comment carries no extractable content.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.body.is_none()
            && self.params.is_empty()
            && self.type_params.is_empty()
            && self.returns.is_none()
            && self.throws.is_empty()
            && self.deprecated.is_none()
    }
}

/// Parse an optional raw doc comment.
pub fn parse_opt(
    doc: Option<&str>,
    doc_kind: Option<&str>,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
) -> Option<ParsedJavadoc> {
    let raw = doc?;
    let parsed = parse(raw, flavor(doc_kind), self_type, resolver);
    if parsed.is_empty() && parsed.links.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

/// Parse the raw text of a javadoc comment into a [`ParsedJavadoc`].
pub fn parse(
    raw: &str,
    flavor: DocFlavor,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
) -> ParsedJavadoc {
    let mut out = ParsedJavadoc::default();
    let text = raw.replace("\r\n", "\n").replace('\r', "\n");

    // --- 1. Split the main description from the block-tag section. -----------
    let mut main_lines: Vec<&str> = Vec::new();
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(tag_name) = block_tag_name(trimmed) {
            if let Some((name, lines)) = current.take() {
                tags.push((name, lines.join("\n")));
            }
            let content = trimmed[1 + tag_name.len()..].trim_start();
            current = Some((tag_name, vec![content]));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        } else {
            main_lines.push(line);
        }
    }
    if let Some((name, lines)) = current.take() {
        tags.push((name, lines.join("\n")));
    }

    // --- 2. Render the main description and split summary / body. -----------
    let main = render(
        &main_lines.join("\n"),
        flavor,
        self_type,
        resolver,
        &mut out.links,
    );
    let (summary, mut body) = split_summary(&main);
    out.summary = summary;

    // --- 3. Interpret block tags. -------------------------------------------
    for (name, content) in tags {
        let rendered = render(&content, flavor, self_type, resolver, &mut out.links);
        let rendered = rendered.trim().to_string();
        match name.as_str() {
            "param" => {
                let (head, raw_desc) = split_first_token(&content);
                let rendered_desc = render(&raw_desc, flavor, self_type, resolver, &mut out.links);
                let desc = rendered_desc
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Some(type_param) = head.strip_prefix('<').and_then(|h| h.strip_suffix('>')) {
                    out.type_params
                        .entry(type_param.to_string())
                        .or_insert(desc);
                } else if !head.is_empty() {
                    out.params.entry(head).or_insert(desc);
                }
            }
            "return" | "returns" => {
                if out.returns.is_none() && !rendered.is_empty() {
                    out.returns = Some(rendered);
                }
            }
            "throws" | "exception" => {
                let (head, desc) = split_first_token(&rendered);
                if !head.is_empty() {
                    let resolved = resolve_reference(&head, self_type, resolver);
                    out.links.push(resolved.clone());
                    out.throws.push((resolved, desc));
                }
            }
            "deprecated" => out.deprecated = Some(rendered),
            "since" => out.since = Some(rendered),
            "version" => out.version = Some(rendered),
            "author" => out.authors.push(rendered),
            "see" => {
                let (head, _) = split_first_token(&rendered);
                if is_element_reference(&head) {
                    let resolved = resolve_reference(&head, self_type, resolver);
                    out.links.push(resolved.clone());
                    out.see.push(resolved);
                } else if !rendered.is_empty() {
                    out.see.push(rendered);
                }
            }
            "apiNote" | "implSpec" | "implNote" if !rendered.is_empty() => {
                let label = match name.as_str() {
                    "apiNote" => "API note",
                    "implSpec" => "Implementation requirements",
                    _ => "Implementation note",
                };
                let section = format!("{label}: {rendered}");
                body = Some(match body.take() {
                    Some(b) => format!("{b}\n\n{section}"),
                    None => section,
                });
            }
            _ => {}
        }
    }
    out.body = body;
    out
}

fn block_tag_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix('@')?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if name.is_empty() || !name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(name)
}

fn split_first_token(text: &str) -> (String, String) {
    let trimmed = text.trim_start();
    match trimmed.split_once(char::is_whitespace) {
        Some((head, rest)) => (head.to_string(), rest.trim().to_string()),
        None => (trimmed.to_string(), String::new()),
    }
}

fn split_summary(main: &str) -> (Option<String>, Option<String>) {
    let text = main.trim();
    if text.is_empty() {
        return (None, None);
    }

    let bytes = text.as_bytes();
    let mut sentence_end = None;
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '!' | '?') {
            let next = bytes.get(i + 1);
            if next.is_none() || next.is_some_and(|b| b.is_ascii_whitespace()) {
                sentence_end = Some(i + 1);
                break;
            }
        }
        if c == '\n' && text[..i].ends_with('\n') {
            sentence_end = Some(i);
            break;
        }
    }

    let split_at = sentence_end.unwrap_or(text.len());
    let summary = text[..split_at]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let rest = text[split_at..].trim();
    (
        if summary.is_empty() {
            None
        } else {
            Some(summary)
        },
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        },
    )
}

// ---------------------------------------------------------------------------
// Rendering: inline tags + flavor-specific cleanup
// ---------------------------------------------------------------------------

fn render(
    text: &str,
    flavor: DocFlavor,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
    links: &mut Vec<String>,
) -> String {
    let expanded = expand_inline_tags(text, flavor, self_type, resolver, links, 0);
    match flavor {
        DocFlavor::Traditional => html_to_text(&expanded),
        DocFlavor::Markdown => resolve_markdown_refs(&expanded, self_type, resolver, links),
    }
}

fn expand_inline_tags(
    text: &str,
    flavor: DocFlavor,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
    links: &mut Vec<String>,
    depth: u8,
) -> String {
    if depth > 8 {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{'
            && bytes.get(i + 1) == Some(&b'@')
            && let Some(end) = matching_brace(text, i)
        {
            let inner = &text[i + 2..end];
            let (tag, raw_body) = match inner.split_once(char::is_whitespace) {
                Some((t, b)) => (t, b.trim()),
                None => (inner, ""),
            };
            let body = match tag {
                "code" | "literal" | "snippet" | "systemProperty" => raw_body.to_string(),
                _ => expand_inline_tags(raw_body, flavor, self_type, resolver, links, depth + 1),
            };
            out.push_str(&render_inline_tag(
                tag, &body, flavor, self_type, resolver, links,
            ));
            i = end + 1;
            continue;
        }
        let c = text[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn matching_brace(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn render_inline_tag(
    tag: &str,
    body: &str,
    flavor: DocFlavor,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
    links: &mut Vec<String>,
) -> String {
    let protect = |s: &str| match flavor {
        DocFlavor::Traditional => s.replace('&', "&amp;").replace('<', "&lt;"),
        DocFlavor::Markdown => s.to_string(),
    };
    match tag {
        "code" => format!("`{}`", protect(body)),
        "literal" => protect(body),
        "link" | "linkplain" => {
            let (reference, label) = split_link_body(body);
            if !reference.is_empty() {
                links.push(resolve_reference(&reference, self_type, resolver));
            }
            if label.is_empty() {
                format!("`{reference}`")
            } else {
                label
            }
        }
        "value" => {
            if !body.is_empty() {
                links.push(resolve_reference(body, self_type, resolver));
            }
            format!("`{body}`")
        }
        "inheritDoc" => String::new(),
        "snippet" => match body.split_once(':') {
            Some((_, code)) => {
                format!("\n```\n{}\n```\n", protect(code.trim_matches('\n')))
            }
            None => String::new(),
        },
        "docRoot" => String::new(),
        "index" => split_first_token(body).0,
        "systemProperty" => format!("`{}`", protect(body)),
        _ => body.to_string(),
    }
}

fn split_link_body(body: &str) -> (String, String) {
    let body = body.trim();
    let mut parens = 0usize;
    for (i, c) in body.char_indices() {
        match c {
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            c if c.is_whitespace() && parens == 0 => {
                return (body[..i].to_string(), body[i..].trim().to_string());
            }
            _ => {}
        }
    }
    (body.to_string(), String::new())
}

// ---------------------------------------------------------------------------
// Reference resolution
// ---------------------------------------------------------------------------

/// Resolve a javadoc element reference to a fully qualified form where possible.
pub fn resolve_reference(
    reference: &str,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
) -> String {
    let (type_part, member) = match reference.split_once('#') {
        Some((t, m)) => (t, Some(m)),
        None => (reference, None),
    };

    let resolved_type = if type_part.is_empty() {
        self_type.unwrap_or("").to_string()
    } else if type_part.contains('.') {
        type_part.to_string()
    } else {
        resolver
            .get(type_part)
            .cloned()
            .unwrap_or_else(|| type_part.to_string())
    };

    match member {
        Some(m) if resolved_type.is_empty() => format!("#{m}"),
        Some(m) => format!("{resolved_type}#{m}"),
        None => resolved_type,
    }
}

fn is_element_reference(s: &str) -> bool {
    if s.is_empty()
        || s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with('"')
        || s.starts_with('<')
    {
        return false;
    }
    let (type_part, member) = match s.split_once('#') {
        Some((t, m)) => (t, Some(m)),
        None => (s, None),
    };
    let type_ok = type_part.is_empty()
        || type_part.split('.').all(|seg| {
            !seg.is_empty()
                && seg
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && seg
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        });
    let member_ok = member.is_none_or(|m| {
        let name_part = m.split('(').next().unwrap_or("");
        !name_part.is_empty()
            && name_part
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
    });
    type_ok && member_ok && !(type_part.is_empty() && member.is_none())
}

// ---------------------------------------------------------------------------
// Traditional flavor: HTML → clean text
// ---------------------------------------------------------------------------

fn html_to_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let rest = &text[i + 1..];
        let Some(close) = rest.find('>') else {
            out.push('<');
            continue;
        };
        let tag_body = &rest[..close];
        let tag = tag_body
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let closing = tag_body.starts_with('/');
        match (tag.as_str(), closing) {
            ("p", false) => out.push_str("\n\n"),
            ("br", _) => out.push('\n'),
            ("li", false) => out.push_str("\n- "),
            ("tr", false)
            | ("ul", true)
            | ("ol", true)
            | ("table", true)
            | ("pre", true)
            | ("blockquote", true) => {
                out.push('\n');
            }
            ("pre", false) => out.push('\n'),
            _ => {}
        }
        for _ in 0..=close {
            chars.next();
        }
    }

    collapse_blank_lines(&decode_entities(&out))
        .trim()
        .to_string()
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some(semi) = text[i..].find(';').filter(|&s| s <= 12)
        {
            let entity = &text[i + 1..i + semi];
            let decoded = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix('#')
                    .and_then(|num| {
                        if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                            u32::from_str_radix(hex, 16).ok()
                        } else {
                            num.parse::<u32>().ok()
                        }
                    })
                    .and_then(char::from_u32),
            };
            if let Some(ch) = decoded {
                out.push(ch);
                i += semi + 1;
                continue;
            }
        }
        let c = text[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank_run > 0 {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(line.trim_end());
    }
    out
}

// ---------------------------------------------------------------------------
// Markdown flavor (JEP 467): element-reference links
// ---------------------------------------------------------------------------

fn resolve_markdown_refs(
    text: &str,
    self_type: Option<&str>,
    resolver: &HashMap<String, String>,
    links: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && bytes.get(i + 1) == Some(&b'[') {
            out.push('[');
            i += 2;
            continue;
        }
        if bytes[i] == b'['
            && let Some(close) = text[i + 1..].find(']').map(|o| i + 1 + o)
        {
            let first = &text[i + 1..close];
            let after = bytes.get(close + 1);
            match after {
                Some(b'(') => {}
                Some(b'[') => {
                    if let Some(close2) = text[close + 2..].find(']').map(|o| close + 2 + o) {
                        let reference = &text[close + 2..close2];
                        if is_element_reference(reference) {
                            links.push(resolve_reference(reference, self_type, resolver));
                            out.push_str(first);
                            i = close2 + 1;
                            continue;
                        }
                    }
                }
                _ => {
                    if is_element_reference(first) {
                        links.push(resolve_reference(first, self_type, resolver));
                        out.push_str(&format!("`{first}`"));
                        i = close + 1;
                        continue;
                    }
                }
            }
        }
        let c = text[i..].chars().next().unwrap_or('\u{FFFD}');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

// ---------------------------------------------------------------------------
// Tests (pure — no oracle required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("Shape".to_string(), "com.example.Shape".to_string());
        m.insert("Circle".to_string(), "com.example.Circle".to_string());
        m
    }

    #[test]
    fn traditional_summary_tags_and_links() {
        let raw = "Compute the area of a {@link Shape}.\n\n<p>Uses {@code Math.PI} internally,\nsee <b>notes</b>.\n\n@param scale the scale factor,\n    spanning two lines\n@param <T> the element type\n@return the area in square units\n@throws ArithmeticException if geometry breaks\n@since 1.2\n@deprecated use {@link Circle#area()} instead\n";
        let parsed = parse(
            raw,
            DocFlavor::Traditional,
            Some("com.example.Shape"),
            &resolver(),
        );
        assert_eq!(
            parsed.summary.as_deref(),
            Some("Compute the area of a `Shape`.")
        );
        assert_eq!(
            parsed.body.as_deref(),
            Some("Uses `Math.PI` internally,\nsee notes.")
        );
        assert_eq!(
            parsed.params.get("scale").map(String::as_str),
            Some("the scale factor, spanning two lines")
        );
        assert_eq!(
            parsed.type_params.get("T").map(String::as_str),
            Some("the element type")
        );
        assert_eq!(parsed.returns.as_deref(), Some("the area in square units"));
        assert_eq!(parsed.throws.len(), 1);
        assert_eq!(parsed.throws[0].0, "ArithmeticException");
        assert_eq!(parsed.throws[0].1, "if geometry breaks");
        assert_eq!(parsed.since.as_deref(), Some("1.2"));
        assert_eq!(
            parsed.deprecated.as_deref(),
            Some("use `Circle#area()` instead")
        );
        assert!(parsed.links.contains(&"com.example.Shape".to_string()));
        assert!(
            parsed
                .links
                .contains(&"com.example.Circle#area()".to_string())
        );
    }

    #[test]
    fn link_labels_and_hash_references() {
        let raw =
            "See {@link Shape#area() the area method} and {@linkplain #parse(String) parsing}.";
        let parsed = parse(
            raw,
            DocFlavor::Traditional,
            Some("com.example.Shape"),
            &resolver(),
        );
        assert_eq!(
            parsed.summary.as_deref(),
            Some("See the area method and parsing.")
        );
        assert!(
            parsed
                .links
                .contains(&"com.example.Shape#area()".to_string())
        );
        assert!(
            parsed
                .links
                .contains(&"com.example.Shape#parse(String)".to_string())
        );
    }

    #[test]
    fn markdown_reference_links() {
        let raw = "A circle, documented in *Markdown*.\n\nLinked: [java.util.List] and [the shape][Shape#area()],\nnot [this](https://example.com).\n\n@param radius the radius";
        let parsed = parse(
            raw,
            DocFlavor::Markdown,
            Some("com.example.Circle"),
            &resolver(),
        );
        assert_eq!(
            parsed.summary.as_deref(),
            Some("A circle, documented in *Markdown*.")
        );
        assert!(parsed.body.as_deref().unwrap().contains("`java.util.List`"));
        assert!(parsed.body.as_deref().unwrap().contains("the shape"));
        assert!(
            parsed
                .body
                .as_deref()
                .unwrap()
                .contains("[this](https://example.com)")
        );
        assert!(parsed.links.contains(&"java.util.List".to_string()));
        assert!(
            parsed
                .links
                .contains(&"com.example.Shape#area()".to_string())
        );
        assert_eq!(
            parsed.params.get("radius").map(String::as_str),
            Some("the radius")
        );
    }

    #[test]
    fn html_entities_and_lists() {
        let raw = "Options:\n<ul>\n<li>fast &amp; loose</li>\n<li>&lt;strict&gt;</li>\n</ul>";
        let parsed = parse(raw, DocFlavor::Traditional, None, &HashMap::new());
        let body_or_summary = parsed.documentation().unwrap();
        assert!(body_or_summary.contains("- fast & loose"));
        assert!(body_or_summary.contains("- <strict>"));
    }

    #[test]
    fn snippet_and_literal() {
        let raw = "Use it like {@literal a<b>}:\n{@snippet lang=java :\nint x = 1;\n}";
        let parsed = parse(raw, DocFlavor::Traditional, None, &HashMap::new());
        let doc = parsed.documentation().unwrap();
        assert!(doc.contains("a<b>"));
        assert!(doc.contains("int x = 1;"));
    }

    #[test]
    fn see_and_authors() {
        let raw = "Summary.\n\n@see Shape\n@see \"The Geometry Book\"\n@author Miles\n@version 2.0";
        let parsed = parse(raw, DocFlavor::Traditional, None, &resolver());
        assert_eq!(parsed.see[0], "com.example.Shape");
        assert!(parsed.see[1].contains("Geometry"));
        assert_eq!(parsed.authors, vec!["Miles".to_string()]);
        assert_eq!(parsed.version.as_deref(), Some("2.0"));
    }

    #[test]
    fn first_sentence_split() {
        let (summary, body) = split_summary("One. Two three.\n\nFour.");
        assert_eq!(summary.as_deref(), Some("One."));
        assert_eq!(body.as_deref(), Some("Two three.\n\nFour."));
    }

    #[test]
    fn empty_comment_is_empty() {
        assert!(parse("", DocFlavor::Traditional, None, &HashMap::new()).is_empty());
        assert!(parse("  \n ", DocFlavor::Markdown, None, &HashMap::new()).is_empty());
    }

    #[test]
    fn inherit_doc_is_dropped() {
        let parsed = parse(
            "{@inheritDoc}",
            DocFlavor::Traditional,
            None,
            &HashMap::new(),
        );
        assert!(parsed.summary.is_none());
    }
}
