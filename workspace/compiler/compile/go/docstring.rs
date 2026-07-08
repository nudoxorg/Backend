//! Go doc-comment parsing (the "doc brain" for the Go producer).
//!
//! The oracle emits doc text already cleaned by `ast.CommentGroup.Text()`
//! — comment markers stripped and `//go:`-style directives removed — so
//! this parser works on prose and implements the *structure* of the
//! [go/doc comment format]:
//!
//! * paragraphs separated by blank lines;
//! * `# Heading` lines (a `#`, a space, one line, its own block);
//! * lists — blocks whose lines start with a bullet (`-`, `*`, `+`) or a
//!   numbered marker (`1.`, `1)`), with indented continuation lines;
//! * code blocks — blocks in which every line is indented;
//! * link definitions — `[Name]: URL` lines, collected into a link map
//!   and removed from the prose;
//! * the `Deprecated:` convention — the paragraph starting with
//!   `Deprecated:` and everything after it is the deprecation notice.
//!
//! Directive lines (`go:generate`-style, `+build`, `lint:` …) are also
//! stripped defensively in case a caller feeds raw comment text.
//!
//! Mirrors the structured shape of `python::docstring::ParsedDocstring`:
//! a summary (the first sentence, per `doc.Synopsis` — simplified: no
//! abbreviation heuristics), body prose, and a `documentation()`
//! recombiner for `Symbol.documentation`.
//!
//! [go/doc comment format]: https://go.dev/doc/comment

use std::collections::HashMap;

/// A structured view of a single Go doc comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoDoc {
	/// The synopsis — the first sentence of the first paragraph.
	pub summary: Option<String>,

	/// The remaining prose: rest of the first paragraph plus all
	/// following blocks (headings kept as `# H`, lists normalized to
	/// `- item` lines, code blocks kept tab-indented).
	pub body: Option<String>,

	/// The `Deprecated:` notice, when present (text after the marker,
	/// plus any following paragraphs).
	pub deprecated: Option<String>,

	/// Link definitions: name → URL (`[Name]: URL` lines).
	pub links: HashMap<String, String>,
}

impl GoDoc {
	/// Recombine into a single documentation string suitable for
	/// `Symbol.documentation`. The deprecation notice is re-appended as
	/// a final paragraph so no information is dropped.
	pub fn documentation(&self) -> Option<String> {
		let mut parts: Vec<String> = Vec::new();
		if let Some(summary) = &self.summary {
			parts.push(summary.clone());
		}
		if let Some(body) = &self.body {
			parts.push(body.clone());
		}
		if let Some(deprecated) = &self.deprecated {
			parts.push(format!("Deprecated: {deprecated}"));
		}
		if parts.is_empty() { None } else { Some(parts.join("\n\n")) }
	}

	/// Whether this doc comment carries no extractable content.
	pub fn is_empty(&self) -> bool {
		self.summary.is_none()
			&& self.body.is_none()
			&& self.deprecated.is_none()
			&& self.links.is_empty()
	}
}

/// A block of a Go doc comment, per the go/doc comment model.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
	Paragraph(String),
	Heading(String),
	Code(String),
	List(Vec<String>),
}

/// Parse cleaned Go doc-comment text into a [`GoDoc`].
pub fn parse(raw: &str) -> GoDoc {
	let lines = normalize(raw);
	let (blocks, links) = split_blocks(&lines);
	assemble(blocks, links)
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/// Normalize newlines, strip residual comment markers, and drop
/// directive lines. The oracle already hands us `CommentGroup.Text()`
/// output, so marker/directive stripping is defense-in-depth for raw
/// callers.
fn normalize(raw: &str) -> Vec<String> {
	raw.replace("\r\n", "\n")
		.replace('\r', "\n")
		.lines()
		.map(strip_marker)
		.filter(|line| !is_directive(line))
		.map(str::to_string)
		.collect()
}

/// Strip a leading `//` (and one following space) or block-comment
/// remnants from a line.
fn strip_marker(line: &str) -> &str {
	let trimmed_start = line.trim_start();
	if let Some(rest) = trimmed_start.strip_prefix("//") {
		return rest.strip_prefix(' ').unwrap_or(rest);
	}
	line.trim_end_matches("*/")
}

/// Directive lines per the go/doc rules: `//go:generate`-style
/// (`word:rest` with a lowercase-word prefix and no space after the
/// colon) and legacy `+build` constraints.
fn is_directive(line: &str) -> bool {
	let t = line.trim();
	if t.starts_with("+build") || t.starts_with("go:build") {
		return true;
	}
	if let Some((head, rest)) = t.split_once(':') {
		let head_is_word = !head.is_empty()
			&& head.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
		// `go:generate echo`, `lint:ignore`, … — but NOT prose like
		// `Deprecated: use X` (uppercase) or `note: this` (space after
		// the colon).
		return head_is_word && rest.starts_with(|c: char| !c.is_whitespace()) && !rest.is_empty();
	}
	false
}

// ---------------------------------------------------------------------------
// Block splitting
// ---------------------------------------------------------------------------

fn is_blank(line: &str) -> bool {
	line.trim().is_empty()
}

fn is_indented(line: &str) -> bool {
	line.starts_with(' ') || line.starts_with('\t')
}

/// Does this line open a list item? (`- x`, `* x`, `+ x`, `1. x`, `1) x`)
fn is_list_item(line: &str) -> bool {
	let t = line.trim_start();
	if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
		return rest.starts_with(' ');
	}
	let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
	if digits.is_empty() {
		return false;
	}
	let rest = &t[digits.len()..];
	matches!(rest.as_bytes().first(), Some(b'.') | Some(b')'))
		&& rest[1..].starts_with(' ')
}

/// A `[Name]: URL` link-definition line.
fn parse_link_def(line: &str) -> Option<(String, String)> {
	let t = line.trim();
	let rest = t.strip_prefix('[')?;
	let (name, tail) = rest.split_once("]:")?;
	let url = tail.trim();
	if name.is_empty() || url.is_empty() || url.contains(char::is_whitespace) {
		return None;
	}
	Some((name.to_string(), url.to_string()))
}

/// A `# Heading` line (exactly one `#`, then a space, then text).
fn parse_heading(line: &str) -> Option<&str> {
	let rest = line.strip_prefix("# ")?;
	let text = rest.trim();
	if text.is_empty() { None } else { Some(text) }
}

/// Group lines into go/doc blocks and collect link definitions.
fn split_blocks(lines: &[String]) -> (Vec<Block>, HashMap<String, String>) {
	let mut blocks = Vec::new();
	let mut links = HashMap::new();
	let mut i = 0;

	while i < lines.len() {
		if is_blank(&lines[i]) {
			i += 1;
			continue;
		}

		// Headings are single-line blocks.
		if let Some(text) = parse_heading(&lines[i]) {
			blocks.push(Block::Heading(text.to_string()));
			i += 1;
			continue;
		}

		// List block: items with indented continuations. Checked before
		// code blocks because list items are conventionally indented
		// (`  - item`) yet are lists, not code, per go/doc.
		if is_list_item(&lines[i]) {
			let mut items: Vec<String> = Vec::new();
			while i < lines.len() && !is_blank(&lines[i]) {
				if is_list_item(&lines[i]) {
					items.push(item_text(&lines[i]).to_string());
				} else if is_indented(&lines[i]) {
					if let Some(last) = items.last_mut() {
						last.push(' ');
						last.push_str(lines[i].trim());
					}
				} else {
					break;
				}
				i += 1;
			}
			blocks.push(Block::List(items));
			continue;
		}

		// Code block: a run of indented (or blank-interior) lines that
		// did not parse as a list.
		if is_indented(&lines[i]) {
			let start = i;
			let mut end = i;
			while i < lines.len() && (is_indented(&lines[i]) || is_blank(&lines[i])) {
				if is_list_item(&lines[i]) {
					break;
				}
				if !is_blank(&lines[i]) {
					end = i;
				}
				i += 1;
			}
			let code = lines[start..=end].join("\n");
			blocks.push(Block::Code(code));
			continue;
		}

		// Paragraph: consecutive flush-left, non-blank prose lines,
		// harvesting link definitions out of the prose.
		let mut prose: Vec<&str> = Vec::new();
		while i < lines.len()
			&& !is_blank(&lines[i])
			&& !is_indented(&lines[i])
			&& parse_heading(&lines[i]).is_none()
			&& !is_list_item(&lines[i])
		{
			if let Some((name, url)) = parse_link_def(&lines[i]) {
				links.insert(name, url);
			} else {
				prose.push(lines[i].trim_end());
			}
			i += 1;
		}
		if !prose.is_empty() {
			blocks.push(Block::Paragraph(prose.join("\n")));
		}
	}

	(blocks, links)
}

/// The text of a list-item line, marker removed.
fn item_text(line: &str) -> &str {
	let t = line.trim_start();
	if let Some(rest) = t.strip_prefix(['-', '*', '+']) {
		return rest.trim_start();
	}
	let digits = t.chars().take_while(char::is_ascii_digit).count();
	t[digits + 1..].trim_start()
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// Assemble parsed blocks into the summary / body / deprecated split.
fn assemble(blocks: Vec<Block>, links: HashMap<String, String>) -> GoDoc {
	let mut doc = GoDoc { links, ..GoDoc::default() };

	let mut rendered: Vec<String> = Vec::new();
	let mut deprecated: Vec<String> = Vec::new();
	let mut in_deprecated = false;
	let mut first_paragraph = true;

	for block in blocks {
		// The `Deprecated:` paragraph and everything after it belong to
		// the deprecation notice.
		if !in_deprecated {
			if let Block::Paragraph(text) = &block {
				if let Some(rest) = text.strip_prefix("Deprecated:") {
					in_deprecated = true;
					let rest = rest.trim();
					if !rest.is_empty() {
						deprecated.push(flatten(rest));
					}
					continue;
				}
			}
		}
		if in_deprecated {
			deprecated.push(render(&block));
			continue;
		}

		if first_paragraph {
			if let Block::Paragraph(text) = &block {
				first_paragraph = false;
				let flat = flatten(text);
				let (summary, rest) = split_synopsis(&flat);
				doc.summary = Some(summary);
				if !rest.is_empty() {
					rendered.push(rest);
				}
				continue;
			}
		}
		rendered.push(render(&block));
	}

	if !rendered.is_empty() {
		doc.body = Some(rendered.join("\n\n"));
	}
	if !deprecated.is_empty() {
		doc.deprecated = Some(deprecated.join("\n\n"));
	}
	doc
}

/// Render a block back to normalized text for the body.
fn render(block: &Block) -> String {
	match block {
		Block::Paragraph(text) => flatten(text),
		Block::Heading(text) => format!("# {text}"),
		Block::Code(code) => code.clone(),
		Block::List(items) => {
			items.iter().map(|item| format!("- {item}")).collect::<Vec<_>>().join("\n")
		}
	}
}

/// Collapse a paragraph's soft line breaks into single spaces.
fn flatten(text: &str) -> String {
	text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split the synopsis (first sentence) from a flattened paragraph,
/// following `go/doc.Synopsis`' core rule: the sentence ends at the
/// first period followed by whitespace (or at the end of the text).
/// The abbreviation heuristics ("e.g.", initials) are intentionally
/// omitted — acceptable imprecision for doc summaries.
fn split_synopsis(text: &str) -> (String, String) {
	let bytes = text.as_bytes();
	for (idx, window) in bytes.windows(2).enumerate() {
		if window[0] == b'.' && window[1] == b' ' {
			let summary = text[..=idx].trim().to_string();
			let rest = text[idx + 1..].trim().to_string();
			return (summary, rest);
		}
	}
	(text.trim().to_string(), String::new())
}

// ---------------------------------------------------------------------------
// Tests (pure parser — no oracle required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn summary_and_body_split() {
		let doc = parse("Sum adds values. It never overflows.\n\nMore detail here.");
		assert_eq!(doc.summary.as_deref(), Some("Sum adds values."));
		assert_eq!(doc.body.as_deref(), Some("It never overflows.\n\nMore detail here."));
	}

	#[test]
	fn headings_lists_and_code() {
		let text = "Fixture does things.\n\n# Usage\n\nCall it:\n\n\tresult := Do()\n\nOptions:\n  - fast mode\n  - slow mode";
		let doc = parse(text);
		assert_eq!(doc.summary.as_deref(), Some("Fixture does things."));
		let body = doc.body.unwrap();
		assert!(body.contains("# Usage"));
		assert!(body.contains("\tresult := Do()"));
		assert!(body.contains("- fast mode\n- slow mode"));
	}

	#[test]
	fn deprecated_paragraph_is_extracted() {
		let doc = parse("Old does a thing.\n\nDeprecated: use New instead.\nIt will be removed.");
		assert_eq!(doc.summary.as_deref(), Some("Old does a thing."));
		assert_eq!(doc.deprecated.as_deref(), Some("use New instead. It will be removed."));
		assert!(doc.body.is_none());
		// documentation() re-appends the notice.
		assert!(doc.documentation().unwrap().contains("Deprecated: use New instead."));
	}

	#[test]
	fn link_definitions_are_collected() {
		let doc = parse("See [Go] for details.\n\n[Go]: https://go.dev");
		assert_eq!(doc.links.get("Go").map(String::as_str), Some("https://go.dev"));
		assert_eq!(doc.summary.as_deref(), Some("See [Go] for details."));
	}

	#[test]
	fn directives_are_stripped() {
		let doc = parse("Run runs.\ngo:generate echo hi\n+build linux");
		assert_eq!(doc.summary.as_deref(), Some("Run runs."));
		assert!(doc.body.is_none());
	}

	#[test]
	fn prose_colon_is_not_a_directive() {
		let doc = parse("Note: this stays.");
		assert_eq!(doc.summary.as_deref(), Some("Note: this stays."));
	}

	#[test]
	fn empty_input_is_empty() {
		assert!(parse("").is_empty());
		assert!(parse("   \n  ").is_empty());
	}

	#[test]
	fn raw_slash_comments_are_accepted() {
		let doc = parse("// Reader reads.\n// It buffers.");
		assert_eq!(doc.summary.as_deref(), Some("Reader reads."));
		assert_eq!(doc.body.as_deref(), Some("It buffers."));
	}
}
