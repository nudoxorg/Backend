//! Structural C# XML-doc parsing (CSHARP-PLAN §3.9).
//!
//! Takes the doc XML the oracle transported (already inheritdoc/`<include>`
//! resolved oracle-side) and parses it into a structured [`ParsedDoc`]:
//! `<summary>` + `<remarks>` prose, per-`<param>` / `<typeparam>`
//! descriptions, `<returns>`, `<value>`, per-`<exception>` text, `<example>`
//! blocks, and `<seealso>` references. Inline tags (`<see>`, `<paramref>`,
//! `<c>`, `<code>`, `<para>`, `<list>`) lower to Markdown.
//!
//! Parsing uses **`quick-xml`** for well-formed element walks and entity
//! decoding; unknown tags still degrade to their text content rather than
//! failing (doc comments are often slightly broken).

use std::{collections::HashMap, fmt::Write as _, io::Cursor};

use quick_xml::{Reader, events::Event};

/// A parsed C# doc comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDoc {
    pub summary: Option<String>,
    pub remarks: Option<String>,
    pub value: Option<String>,
    pub returns: Option<String>,
    /// `<param name> → description`.
    pub params: HashMap<String, String>,
    /// `<typeparam name> → description`.
    pub type_params: HashMap<String, String>,
    /// `(raw cref, description)` in declaration order.
    ///
    /// The cref is the **raw** XML attribute value (e.g.
    /// `"T:System.ArgumentException"`), not the stripped display name.
    /// Callers that need a display name should apply
    /// [`strip_doc_id_prefix`] themselves; callers that need to resolve
    /// the type (e.g. for `Function::throws`) need the full doc-id.
    pub exceptions: Vec<(String, String)>,
    /// `<example>` blocks.
    pub examples: Vec<String>,
    /// `<seealso cref>` references — these feed Symbol::doc_links.
    pub see_also: Vec<String>,
}

impl ParsedDoc {
    /// The prose body: summary, then remarks, then any examples.
    pub fn documentation(&self) -> Option<String> {
        let mut sections: Vec<String> = Vec::new();
        if let Some(s) = &self.summary
            && !s.is_empty()
        {
            sections.push(s.clone());
        }
        if let Some(r) = &self.remarks
            && !r.is_empty()
        {
            sections.push(r.clone());
        }
        for ex in &self.examples {
            sections.push(format!("Example:\n{ex}"));
        }
        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }
}

/// Parse doc XML into a [`ParsedDoc`]; `None` for absent/blank docs.
pub fn parse_opt(doc: Option<&str>) -> Option<ParsedDoc> {
    let doc = doc?;
    if doc.trim().is_empty() {
        return None;
    }
    Some(parse(doc))
}

/// Parse doc XML into a [`ParsedDoc`].
pub fn parse(doc: &str) -> ParsedDoc {
    // Strip an outer `<member …>…</member>` wrapper if present.
    let body = strip_member_wrapper(doc);

    let mut parsed = ParsedDoc::default();

    for (tag, attrs, inner) in top_level_elements(body) {
        let text = inline_to_markdown(&inner);
        match tag.as_str() {
            "summary" => parsed.summary = Some(merge(parsed.summary.take(), text)),
            "remarks" => parsed.remarks = Some(merge(parsed.remarks.take(), text)),
            "returns" => parsed.returns = Some(merge(parsed.returns.take(), text)),
            "value" => parsed.value = Some(merge(parsed.value.take(), text)),
            "example" => parsed.examples.push(text),
            "param" => {
                if let Some(name) = attr(&attrs, "name") {
                    parsed.params.insert(name, text);
                }
            }
            "typeparam" => {
                if let Some(name) = attr(&attrs, "name") {
                    parsed.type_params.insert(name, text);
                }
            }
            "exception" => {
                // Keep the raw cref (the full doc-id, e.g. "T:System.IO.IOException")
                // so that lower.rs can resolve the exception type by doc-id.
                // Do NOT strip_doc_id_prefix here; callers that want a display name
                // should strip it themselves.
                let cref = attr(&attrs, "cref").unwrap_or_default();
                parsed.exceptions.push((cref, text));
            }
            "seealso" => {
                if let Some(cref) = attr(&attrs, "cref") {
                    parsed.see_also.push(strip_doc_id_prefix(&cref));
                } else if !text.is_empty() {
                    parsed.see_also.push(text);
                }
            }
            // Unknown top-level tag: fold its text into the summary body.
            _ if !text.is_empty() => {
                parsed.summary = Some(merge(parsed.summary.take(), text));
            }
            _ => {}
        }
    }

    parsed
}

/// Merge two prose fragments (later text appended as a new paragraph).
fn merge(existing: Option<String>, new: String) -> String {
    match existing {
        Some(prev) if !prev.is_empty() && !new.is_empty() => format!("{prev}\n\n{new}"),
        Some(prev) if !prev.is_empty() => prev,
        _ => new,
    }
}

/// Drop the `T:` / `M:` / `P:` / `F:` / `E:` doc-id prefix from a cref,
/// leaving a readable name.
pub fn strip_doc_id_prefix(cref: &str) -> String {
    let bare = match cref.split_once(':') {
        Some((prefix, rest)) if prefix.len() == 1 => rest,
        _ => cref,
    };
    // Keep the trailing member/type segment for readability.
    simple_name(bare.split('(').next().unwrap_or(bare))
}

/// The trailing segment of a dotted qualified name (arity backticks stripped).
pub fn simple_name(qualified: &str) -> String {
    let bare = strip_arity(qualified);
    bare.rsplit('.').next().unwrap_or(&bare).to_string()
}

/// Strip metadata arity backticks (`List\`1` → `List`).
fn strip_arity(name: &str) -> String {
    if !name.contains('`') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Strip an outer `<member …>…</member>` wrapper, returning the inner content.
fn strip_member_wrapper(doc: &str) -> &str {
    let trimmed = doc.trim();
    if let Some(rest) = trimmed.strip_prefix("<member")
        && let Some(gt) = rest.find('>')
    {
        let inner = &rest[gt + 1..];
        if let Some(end) = inner.rfind("</member>") {
            return &inner[..end];
        }
        return inner;
    }
    trimmed
}

/// The attribute value for `name`, if present.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let key = format!("{name}=");
    let start = attrs.find(&key)? + key.len();
    let rest = &attrs[start..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(quote)?;
    Some(decode_entities(&rest[..end]))
}

/// Scan `xml` for top-level elements via `quick-xml`, returning
/// `(tag, attrs, inner_xml)` for each. Text outside any element is ignored.
///
/// Nesting is tracked so `<summary>…<c>…</c>…</summary>` yields one summary
/// whose `inner` still contains the nested tags for the Markdown pass.
fn top_level_elements(xml: &str) -> Vec<(String, String, String)> {
    let mut reader = Reader::from_reader(Cursor::new(xml.as_bytes()));
    reader.config_mut().trim_text(false);
    // Doc comments are often slightly ill-formed; keep going.
    reader.config_mut().check_end_names = false;

    let mut out = Vec::new();
    let mut buf = Vec::new();
    // Stack of open elements: (name, attrs, accumulated inner source spans
    // reconstructed as text).
    let mut stack: Vec<(String, String, String)> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let attrs = attrs_to_string(&e);
                if !stack.is_empty() {
                    // Nested open: re-emit into parent inner so markdown sees it.
                    let open = format!("<{name}{attrs}>");
                    if let Some((_, _, inner)) = stack.last_mut() {
                        inner.push_str(&open);
                    }
                }
                stack.push((name, attrs, String::new()));
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let attrs = attrs_to_string(&e);
                if stack.is_empty() {
                    out.push((name, attrs, String::new()));
                } else if let Some((_, _, inner)) = stack.last_mut() {
                    let _ = write!(inner, "<{name}{attrs}/>");
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if let Some((open_name, attrs, inner)) = stack.pop() {
                    if stack.is_empty() {
                        // Top-level close.
                        out.push((open_name, attrs, inner));
                    } else if let Some((_, _, parent_inner)) = stack.last_mut() {
                        // Nested close: fold child back into parent.
                        parent_inner.push_str(&inner);
                        let _ = write!(parent_inner, "</{name}>");
                        let _ = open_name;
                        let _ = attrs;
                    }
                }
            }
            Ok(Event::Text(t)) => {
                // quick-xml 0.37: `BytesText` has no `decode()`; take the raw
                // bytes and expand the XML named entities ourselves.
                let raw = String::from_utf8_lossy(t.as_ref()).into_owned();
                let text = decode_entities(&raw);
                if let Some((_, _, inner)) = stack.last_mut() {
                    inner.push_str(&text);
                }
            }
            Ok(Event::CData(t)) => {
                let text = String::from_utf8_lossy(&t).into_owned();
                if let Some((_, _, inner)) = stack.last_mut() {
                    inner.push_str(&text);
                }
            }
            // quick-xml 0.37 has no `Event::GeneralRef`: named entities arrive
            // inside `Event::Text` and are expanded by `decode_entities` above.
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
        buf.clear();
    }

    // Unclosed top-level elements still emit what we captured.
    while let Some((name, attrs, inner)) = stack.pop() {
        if stack.is_empty() {
            out.push((name, attrs, inner));
        }
    }

    out
}

/// Serialize start-tag attributes as a leading-space string (` name="v"`).
fn attrs_to_string(e: &quick_xml::events::BytesStart<'_>) -> String {
    let mut out = String::new();
    for a in e.attributes().flatten() {
        let key = String::from_utf8_lossy(a.key.as_ref());
        let val = a.unescape_value().map_or_else(
            |_| String::from_utf8_lossy(&a.value).into_owned(),
            std::borrow::Cow::into_owned,
        );
        out.push(' ');
        out.push_str(&key);
        out.push_str("=\"");
        out.push_str(&val);
        out.push('"');
    }
    out
}

/// Split `<tag attrs...>` body into `(name, attrs)`.
fn split_tag(body: &str) -> (String, String) {
    let body = body.trim();
    body.find(|c: char| c.is_whitespace()).map_or_else(
        || (body.to_string(), String::new()),
        |sp| (body[..sp].to_string(), body[sp..].trim().to_string()),
    )
}

/// Convert an element's inner XML to Markdown, lowering the common inline tags
/// and decoding entities. Leftover unknown tags are stripped to their text.
fn inline_to_markdown(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut i = 0usize;

    while i < inner.len() {
        let Some(lt) = inner[i..].find('<').map(|o| i + o) else {
            out.push_str(&decode_entities(&inner[i..]));
            break;
        };
        out.push_str(&decode_entities(&inner[i..lt]));
        let Some(gt) = inner[lt..].find('>').map(|o| lt + o) else {
            out.push_str(&decode_entities(&inner[lt..]));
            break;
        };
        let tag_body = &inner[lt + 1..gt];
        let self_closing = tag_body.ends_with('/');
        let core = tag_body.trim_end_matches('/').trim();
        let (name, attrs) = split_tag(core);
        let lname = name.trim_start_matches('/').to_ascii_lowercase();

        match lname.as_str() {
            // Self-closing inline references.
            "see" | "seealso" if self_closing => {
                if let Some(cref) = attr(&attrs, "cref") {
                    let _ = write!(out, "`{}`", strip_doc_id_prefix(&cref));
                } else if let Some(kw) = attr(&attrs, "langword") {
                    let _ = write!(out, "`{kw}`");
                } else if let Some(href) = attr(&attrs, "href") {
                    out.push_str(&href);
                }
                i = gt + 1;
            }
            "paramref" | "typeparamref" if self_closing => {
                if let Some(n) = attr(&attrs, "name") {
                    let _ = write!(out, "`{n}`");
                }
                i = gt + 1;
            }
            "para" => {
                out.push_str("\n\n");
                i = gt + 1;
            }
            "br" => {
                out.push('\n');
                i = gt + 1;
            }
            // Inline / block code: reproduce content verbatim in backticks/fences.
            "c" if !self_closing => {
                let (content, next) = take_until_close(inner, gt + 1, "c");
                let _ = write!(out, "`{}`", decode_entities(content.trim()));
                i = next;
            }
            "code" if !self_closing => {
                let (content, next) = take_until_close(inner, gt + 1, "code");
                let _ = write!(
                    out,
                    "\n```\n{}\n```\n",
                    trim_code(&decode_entities(&content))
                );
                i = next;
            }
            // Lists → Markdown bullets (item text only; term/description flattened).
            "item" => {
                out.push_str("\n- ");
                i = gt + 1;
            }
            "list" | "listheader" | "term" | "description" if self_closing => {
                i = gt + 1;
            }
            // Everything else (open/close of unhandled tags): drop the tag, keep text.
            _ => {
                i = gt + 1;
            }
        }
    }

    collapse_whitespace(&out)
}

/// From just after an opening tag, return the content up to the matching
/// `</name>` and the index past it.
fn take_until_close(s: &str, from: usize, name: &str) -> (String, usize) {
    let close = format!("</{name}>");
    s[from..].find(&close).map_or(
        (s[from..].to_string(), s.len()),
        |rel| {
            let end = from + rel;
            (s[from..end].to_string(), end + close.len())
        },
    )
}

/// Trim shared leading blank lines from a code block.
fn trim_code(code: &str) -> String {
    code.trim_matches('\n').to_string()
}

/// Collapse runs of spaces/tabs and cap consecutive blank lines at one.
fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    let mut newlines = 0usize;
    for ch in s.chars() {
        match ch {
            ' ' | '\t' => {
                if !last_was_space && newlines == 0 {
                    out.push(' ');
                }
                last_was_space = true;
            }
            '\n' => {
                if newlines < 2 {
                    out.push('\n');
                }
                newlines += 1;
                last_was_space = false;
            }
            _ => {
                last_was_space = false;
                newlines = 0;
                out.push(ch);
            }
        }
    }
    out.trim().to_string()
}

/// Decode the XML entities that appear in doc comments.
///
/// Prefers `quick_xml::escape::unescape` (handles numeric entities too);
/// falls back to the five XML named entities on error.
fn decode_entities(text: &str) -> String {
    quick_xml::escape::unescape(text).map_or_else(
        |_| {
            text.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&amp;", "&")
        },
        std::borrow::Cow::into_owned,
    )
}

// ---------------------------------------------------------------------------
// Tests (pure)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_and_params() {
        let xml = r#"<member name="M:N.C.Add(System.Int32,System.Int32)">
            <summary>Adds <paramref name="a"/> and <paramref name="b"/>.</summary>
            <param name="a">the first</param>
            <param name="b">the second</param>
            <returns>the sum</returns>
            <exception cref="T:System.OverflowException">on overflow</exception>
        </member>"#;
        let parsed = parse(xml);
        assert_eq!(parsed.summary.as_deref(), Some("Adds `a` and `b`."));
        assert_eq!(
            parsed.params.get("a").map(String::as_str),
            Some("the first")
        );
        assert_eq!(parsed.returns.as_deref(), Some("the sum"));
        assert_eq!(parsed.exceptions.len(), 1);
        // The raw cref is preserved (NOT stripped) so that lower.rs can resolve
        // the exception type by its Roslyn doc-id. Callers needing a display name
        // apply strip_doc_id_prefix themselves.
        assert_eq!(parsed.exceptions[0].0, "T:System.OverflowException");
        assert_eq!(parsed.exceptions[0].1, "on overflow");
    }

    #[test]
    fn inline_see_and_code() {
        let xml = r#"<summary>Use <see cref="T:System.String"/> or <c>null</c>.</summary>"#;
        let parsed = parse(xml);
        assert_eq!(parsed.summary.as_deref(), Some("Use `String` or `null`."));
    }

    #[test]
    fn langword_and_entities() {
        let xml = r#"<summary>Returns <see langword="true"/> if a &lt; b.</summary>"#;
        let parsed = parse(xml);
        assert_eq!(parsed.summary.as_deref(), Some("Returns `true` if a < b."));
    }

    #[test]
    fn blank_doc_is_none() {
        assert_eq!(parse_opt(Some("   ")), None);
        assert_eq!(parse_opt(None), None);
    }

    #[test]
    fn seealso_populates_see_also() {
        let xml = r#"<summary>See <seealso cref="T:System.IDisposable"/> for cleanup.</summary><seealso cref="M:Foo.Bar"/>"#;
        let parsed = parse(xml);
        assert!(parsed.see_also.contains(&"Bar".to_string()));
    }
}
