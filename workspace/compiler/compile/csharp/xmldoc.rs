//! Structural C# XML-doc parsing (CSHARP-PLAN §3.9).
//!
//! Takes the doc XML the oracle transported (already inheritdoc/`<include>`
//! resolved oracle-side) and parses it into a structured [`ParsedDoc`]:
//! `<summary>` + `<remarks>` prose, per-`<param>` / `<typeparam>`
//! descriptions, `<returns>`, `<value>`, per-`<exception>` text, `<example>`
//! blocks, and `<seealso>` references. Inline tags (`<see>`, `<paramref>`,
//! `<c>`, `<code>`, `<para>`, `<list>`) lower to Markdown.
//!
//! The workspace has no XML dependency (mirroring `java::javadoc`), so this is
//! a small hand-rolled element scanner. It is deliberately lenient: unknown
//! tags degrade to their text content rather than failing.

use std::collections::HashMap;

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
	/// `(exception cref, description)` in declaration order.
	pub exceptions: Vec<(String, String)>,
	/// `<example>` blocks.
	pub examples: Vec<String>,
	/// `<seealso cref>` references.
	pub see_also: Vec<String>,
}

impl ParsedDoc {
	/// The prose body: summary, then remarks, then any examples.
	pub fn documentation(&self) -> Option<String> {
		let mut sections: Vec<String> = Vec::new();
		if let Some(s) = &self.summary {
			if !s.is_empty() {
				sections.push(s.clone());
			}
		}
		if let Some(r) = &self.remarks {
			if !r.is_empty() {
				sections.push(r.clone());
			}
		}
		for ex in &self.examples {
			sections.push(format!("Example:\n{ex}"));
		}
		if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
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
				let cref = attr(&attrs, "cref").map(|c| strip_doc_id_prefix(&c)).unwrap_or_default();
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

/// Drop the `T:` / `M:` / `P:` / `F:` / `E:` doc-id prefix from a cref, leaving
/// a readable name.
fn strip_doc_id_prefix(cref: &str) -> String {
	let bare = match cref.split_once(':') {
		Some((prefix, rest)) if prefix.len() == 1 => rest,
		_ => cref,
	};
	// Keep the trailing member/type segment for readability.
	super::types::simple_name(bare.split('(').next().unwrap_or(bare))
}

/// Strip an outer `<member …>…</member>` wrapper, returning the inner content.
fn strip_member_wrapper(doc: &str) -> &str {
	let trimmed = doc.trim();
	if let Some(rest) = trimmed.strip_prefix("<member") {
		if let Some(gt) = rest.find('>') {
			let inner = &rest[gt + 1..];
			if let Some(end) = inner.rfind("</member>") {
				return &inner[..end];
			}
			return inner;
		}
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

/// Scan `xml` for top-level elements, returning `(tag, attrs, inner)` for each.
/// Text outside any element is ignored (doc XML always tags its sections).
fn top_level_elements(xml: &str) -> Vec<(String, String, String)> {
	let bytes = xml.as_bytes();
	let mut out = Vec::new();
	let mut i = 0usize;

	while i < xml.len() {
		let Some(lt) = xml[i..].find('<').map(|o| i + o) else {
			break;
		};
		// Skip comments / declarations.
		if xml[lt..].starts_with("<!--") {
			match xml[lt + 4..].find("-->") {
				Some(end) => {
					i = lt + 4 + end + 3;
					continue;
				}
				None => break,
			}
		}
		let Some(gt) = xml[lt..].find('>').map(|o| lt + o) else {
			break;
		};
		let tag_body = &xml[lt + 1..gt];
		// A closing or self-closing top-level element carries no inner content.
		if tag_body.starts_with('/') {
			i = gt + 1;
			continue;
		}
		let self_closing = tag_body.ends_with('/');
		let (name, attrs) = split_tag(tag_body.trim_end_matches('/'));
		if name.is_empty() {
			i = gt + 1;
			continue;
		}
		if self_closing {
			out.push((name, attrs, String::new()));
			i = gt + 1;
			continue;
		}
		// Find the matching close tag, tracking nesting of the same tag name.
		let close = format!("</{name}>");
		let open_prefix = format!("<{name}");
		let mut depth = 1usize;
		let mut scan = gt + 1;
		let content_start = gt + 1;
		let inner_end;
		loop {
			let next_close = xml[scan..].find(&close).map(|o| scan + o);
			let Some(nc) = next_close else {
				inner_end = xml.len();
				break;
			};
			// Count same-name opens strictly before this close.
			let mut opens = 0usize;
			let mut probe = scan;
			while let Some(rel) = xml[probe..nc].find(&open_prefix) {
				let at = probe + rel;
				// Ensure it's a real element open (`<name` followed by space/>//).
				let after = bytes.get(at + open_prefix.len()).copied();
				if matches!(after, Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\t') | Some(b'\n')) {
					opens += 1;
				}
				probe = at + open_prefix.len();
			}
			depth += opens;
			depth -= 1;
			if depth == 0 {
				inner_end = nc;
				scan = nc + close.len();
				break;
			}
			scan = nc + close.len();
		}
		let inner = xml[content_start..inner_end].to_string();
		out.push((name, attrs, inner));
		i = scan.max(gt + 1);
	}

	out
}

/// Split `<tag attrs...>` body into `(name, attrs)`.
fn split_tag(body: &str) -> (String, String) {
	let body = body.trim();
	match body.find(|c: char| c.is_whitespace()) {
		Some(sp) => (body[..sp].to_string(), body[sp..].trim().to_string()),
		None => (body.to_string(), String::new()),
	}
}

/// Convert an element's inner XML to Markdown, lowering the common inline tags
/// and decoding entities. Leftover unknown tags are stripped to their text.
fn inline_to_markdown(inner: &str) -> String {
	let mut out = String::with_capacity(inner.len());
	let bytes = inner.as_bytes();
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
					out.push_str(&format!("`{}`", strip_doc_id_prefix(&cref)));
				} else if let Some(kw) = attr(&attrs, "langword") {
					out.push_str(&format!("`{kw}`"));
				} else if let Some(href) = attr(&attrs, "href") {
					out.push_str(&href);
				}
				i = gt + 1;
			}
			"paramref" | "typeparamref" if self_closing => {
				if let Some(n) = attr(&attrs, "name") {
					out.push_str(&format!("`{n}`"));
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
				out.push_str(&format!("`{}`", decode_entities(content.trim())));
				i = next;
			}
			"code" if !self_closing => {
				let (content, next) = take_until_close(inner, gt + 1, "code");
				out.push_str(&format!("\n```\n{}\n```\n", trim_code(&decode_entities(&content))));
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
		let _ = bytes; // silence unused in some cfgs
	}

	collapse_whitespace(&out)
}

/// From just after an opening tag, return the content up to the matching
/// `</name>` and the index past it.
fn take_until_close(s: &str, from: usize, name: &str) -> (String, usize) {
	let close = format!("</{name}>");
	match s[from..].find(&close) {
		Some(rel) => {
			let end = from + rel;
			(s[from..end].to_string(), end + close.len())
		}
		None => (s[from..].to_string(), s.len()),
	}
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
fn decode_entities(text: &str) -> String {
	text.replace("&lt;", "<")
		.replace("&gt;", ">")
		.replace("&quot;", "\"")
		.replace("&apos;", "'")
		.replace("&amp;", "&")
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
		assert_eq!(parsed.params.get("a").map(String::as_str), Some("the first"));
		assert_eq!(parsed.returns.as_deref(), Some("the sum"));
		assert_eq!(parsed.exceptions.len(), 1);
		assert_eq!(parsed.exceptions[0].0, "OverflowException");
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
}
