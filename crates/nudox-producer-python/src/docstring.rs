//! Phase 3 "doc brain": docstring extraction + structured parsing.
//!
//! Two layers live here:
//!
//!   1. A **pure parser** ([`parse`] / [`ParsedDocstring`]) that takes the raw
//!      text of a Python docstring (with or without surrounding quotes) and
//!      pulls out a summary, body prose, per-parameter descriptions, and a
//!      returns description. It detects Google (`Args:`), NumPy (`Parameters`
//!      + dashes), and Sphinx (`:param x:`) conventions. It is deliberately
//!      pragmatic — "good enough for full-resolution IR", not a perfect parser
//!      — and is unit-tested directly at the bottom of this file (no pyrefly
//!      needed).
//!
//!   2. A key-based lookup layer ([`DocCatalog`]) that maps module / top-level
//!      def / top-level class / method names to their parsed docstrings. When
//!      the pyrefly feature is off the catalog is populated by the caller from
//!      whatever AST source is available; when the feature is on, `context.rs`
//!      populates it from the Ruff AST via `pyrefly_python::module::Module`.
//!
//! Faithfully ported from `workspace/compiler/compile/python/docstring.rs`
//! (760 LOC, 0 ir:: references). The only changes are:
//!   - Removed `use pyrefly_python::*` and the `build_catalog` / `raw_doc` /
//!     `collect_items` functions (those depend on the pyrefly AST and live in
//!     `context.rs` behind the `pyrefly` feature).
//!   - `DocCatalog` is now unconditionally available; callers populate it via
//!     the insert methods instead of `build_catalog`.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// ParsedDocstring
// ---------------------------------------------------------------------------

/// A structured view of a single Python docstring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedDocstring {
    /// The one-line (or one-paragraph) summary — the first prose block.
    pub summary: Option<String>,
    /// Extended description prose following the summary, up to the first
    /// recognized section header. `None` when there is no extended body.
    pub body: Option<String>,
    /// Map of parameter name → description, merged across Google / NumPy /
    /// Sphinx styles.
    pub params: HashMap<String, String>,
    /// The `Returns:` / `:returns:` description, if present.
    pub returns: Option<String>,
}

impl ParsedDocstring {
    /// Recombine `summary` + `body` into a single documentation string suitable
    /// for `Symbol.documentation`. Returns `None` when both are empty.
    pub fn documentation(&self) -> Option<String> {
        match (&self.summary, &self.body) {
            (Some(s), Some(b)) => Some(format!("{s}\n\n{b}")),
            (Some(s), None) => Some(s.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        }
    }

    /// Whether this docstring carries no extractable content.
    pub fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.body.is_none()
            && self.params.is_empty()
            && self.returns.is_none()
    }
}

// ---------------------------------------------------------------------------
// DocCatalog
// ---------------------------------------------------------------------------

/// Parsed docstrings for one module, keyed by entry name.
#[derive(Debug, Clone, Default)]
pub struct DocCatalog {
    /// The module-level docstring.
    pub module: Option<ParsedDocstring>,
    /// Top-level function / class docstrings, by name.
    items: HashMap<String, ParsedDocstring>,
    /// Method docstrings, keyed by `(class_name, method_name)`.
    members: HashMap<(String, String), ParsedDocstring>,
}

impl DocCatalog {
    /// Insert (or overwrite) a module-level docstring.
    pub fn set_module(&mut self, doc: ParsedDocstring) {
        self.module = Some(doc);
    }

    /// Insert (or overwrite) a top-level item docstring.
    pub fn insert_item(&mut self, name: impl Into<String>, doc: ParsedDocstring) {
        self.items.insert(name.into(), doc);
    }

    /// Insert (or overwrite) a class-member docstring.
    pub fn insert_member(
        &mut self,
        class: impl Into<String>,
        method: impl Into<String>,
        doc: ParsedDocstring,
    ) {
        self.members.insert((class.into(), method.into()), doc);
    }

    /// The parsed docstring for a top-level def / class named `name`.
    pub fn item(&self, name: &str) -> Option<&ParsedDocstring> {
        self.items.get(name)
    }

    /// The parsed docstring for `class_name::method_name`.
    pub fn member(&self, class_name: &str, method_name: &str) -> Option<&ParsedDocstring> {
        self.members
            .get(&(class_name.to_string(), method_name.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Public parse entry-point
// ---------------------------------------------------------------------------

/// Parse the raw text of a docstring into a [`ParsedDocstring`].
///
/// `raw` may be the literal source slice (including surrounding `"""`/`'''`/
/// quote characters and an optional `r`/`b`/`f` prefix) or an already-unquoted
/// string — both are normalized.
pub fn parse(raw: &str) -> ParsedDocstring {
    let lines = normalize(raw);
    if lines.is_empty() {
        return ParsedDocstring::default();
    }

    let (summary, body, sections_start) = split_summary_body(&lines);
    let mut params = HashMap::new();
    let mut returns = None;
    parse_sections(&lines[sections_start..], &mut params, &mut returns);

    ParsedDocstring {
        summary,
        body,
        params,
        returns,
    }
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

/// Normalize raw docstring source into dedented logical lines: strip quotes,
/// expand tabs, normalize newlines, and remove common leading indentation.
fn normalize(raw: &str) -> Vec<String> {
    let unified = raw
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\t', "    ");
    let unquoted = strip_quotes(unified.trim());
    let trimmed = unquoted.trim_matches('\n');
    if trimmed.is_empty() {
        return Vec::new();
    }

    let raw_lines: Vec<&str> = trimmed.lines().collect();
    let min_indent = raw_lines
        .iter()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .min()
        .unwrap_or(0);

    raw_lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if l.trim().is_empty() {
                String::new()
            } else if i == 0 {
                l.trim_end().to_string()
            } else {
                l[min_indent.min(l.len())..].trim_end().to_string()
            }
        })
        .collect()
}

/// Strip an optional string prefix (`r`/`b`/`f`/`u`, any case/combo) and the
/// surrounding quote markers from a string literal. A no-op for plain text.
fn strip_quotes(text: &str) -> &str {
    let trimmed = text.trim();
    let after_prefix = trimmed.trim_start_matches(|c: char| {
        matches!(c, 'r' | 'R' | 'b' | 'B' | 'u' | 'U' | 'f' | 'F')
    });
    for q in ["\"\"\"", "'''", "\"", "'"] {
        if let Some(inner) = after_prefix.strip_prefix(q) {
            if let Some(inner) = inner.strip_suffix(q) {
                return inner;
            }
        }
    }
    trimmed
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|&b| b == b' ').count()
}

// ---------------------------------------------------------------------------
// Summary / body split
// ---------------------------------------------------------------------------

fn split_summary_body(lines: &[String]) -> (Option<String>, Option<String>, usize) {
    let mut i = 0;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }

    let mut summary_lines: Vec<&str> = Vec::new();
    while i < lines.len() && !lines[i].trim().is_empty() && !is_section_start(lines, i) {
        summary_lines.push(lines[i].trim());
        i += 1;
    }
    let summary = join_nonempty(&summary_lines, " ");

    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let body_start = i;
    while i < lines.len() && !is_section_start(lines, i) {
        i += 1;
    }
    let body = {
        let block = lines[body_start..i].join("\n");
        let trimmed = block.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    (summary, body, i)
}

fn join_nonempty(parts: &[&str], sep: &str) -> Option<String> {
    let joined = parts.join(sep);
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// Section detection
// ---------------------------------------------------------------------------

const PARAM_HEADERS: [&str; 5] = [
    "args",
    "arguments",
    "parameters",
    "keyword args",
    "keyword arguments",
];
const RETURN_HEADERS: [&str; 4] = ["returns", "return", "yields", "yield"];
const OTHER_HEADERS: [&str; 10] = [
    "raises",
    "examples",
    "example",
    "note",
    "notes",
    "attributes",
    "see also",
    "references",
    "warns",
    "warnings",
];

fn is_section_start(lines: &[String], idx: usize) -> bool {
    section_kind(lines, idx).is_some()
}

#[derive(Clone, Copy, PartialEq)]
enum SectionKind {
    Params,
    Returns,
    Other,
}

fn section_kind(lines: &[String], idx: usize) -> Option<SectionKind> {
    let line = &lines[idx];
    let trimmed = line.trim();

    if trimmed.starts_with(":param") || trimmed.starts_with(":parameter") {
        return Some(SectionKind::Params);
    }
    if trimmed.starts_with(":return") || trimmed.starts_with(":rtype") || trimmed.starts_with(":yield") {
        return Some(SectionKind::Returns);
    }
    if trimmed.starts_with(":raises") || trimmed.starts_with(":raise") {
        return Some(SectionKind::Other);
    }

    if let Some(name) = trimmed.strip_suffix(':') {
        if let Some(kind) = header_kind(name) {
            return Some(kind);
        }
    }

    if header_kind(trimmed).is_some()
        && idx + 1 < lines.len()
        && is_dashes(lines[idx + 1].trim())
    {
        return header_kind(trimmed);
    }

    None
}

fn header_kind(name: &str) -> Option<SectionKind> {
    let key = name.trim().to_ascii_lowercase();
    if PARAM_HEADERS.contains(&key.as_str()) {
        Some(SectionKind::Params)
    } else if RETURN_HEADERS.contains(&key.as_str()) {
        Some(SectionKind::Returns)
    } else if OTHER_HEADERS.contains(&key.as_str()) {
        Some(SectionKind::Other)
    } else {
        None
    }
}

fn is_dashes(line: &str) -> bool {
    !line.is_empty() && line.chars().all(|c| c == '-')
}

fn parse_sections(
    lines: &[String],
    params: &mut HashMap<String, String>,
    returns: &mut Option<String>,
) {
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();

        if trimmed.starts_with(":param") || trimmed.starts_with(":parameter") {
            i = parse_sphinx_param(lines, i, params);
            continue;
        }
        if trimmed.starts_with(":return") || trimmed.starts_with(":yield") {
            i = parse_sphinx_returns(lines, i, returns);
            continue;
        }

        match section_kind(lines, i) {
            Some(SectionKind::Params) => {
                let numpy = is_numpy_header(lines, i);
                i = parse_param_block(lines, i, numpy, params);
            }
            Some(SectionKind::Returns) => {
                let numpy = is_numpy_header(lines, i);
                i = parse_returns_block(lines, i, numpy, returns);
            }
            _ => i += 1,
        }
    }
}

fn is_numpy_header(lines: &[String], idx: usize) -> bool {
    idx + 1 < lines.len() && is_dashes(lines[idx + 1].trim())
}

// ---------------------------------------------------------------------------
// Google / NumPy block parsing
// ---------------------------------------------------------------------------

fn parse_param_block(
    lines: &[String],
    header_idx: usize,
    numpy: bool,
    params: &mut HashMap<String, String>,
) -> usize {
    let header_indent = leading_spaces(&lines[header_idx]);
    let mut i = header_idx + 1;
    if numpy && i < lines.len() && is_dashes(lines[i].trim()) {
        i += 1;
    }

    let block_start = i;
    let end = block_end(lines, block_start, header_indent, numpy);
    parse_param_entries(&lines[block_start..end], numpy, params);
    end
}

fn block_end(lines: &[String], start: usize, header_indent: usize, numpy: bool) -> usize {
    let mut i = start;
    while i < lines.len() {
        if i > start && is_section_start(lines, i) {
            break;
        }
        if !lines[i].trim().is_empty() {
            let indent = leading_spaces(&lines[i]);
            let dedented = if numpy {
                indent < header_indent
            } else {
                indent <= header_indent
            };
            if dedented {
                break;
            }
        }
        i += 1;
    }
    i
}

fn parse_param_entries(block: &[String], numpy: bool, params: &mut HashMap<String, String>) {
    let entry_indent = block
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| leading_spaces(l))
        .min()
        .unwrap_or(0);

    let mut cur_name: Option<String> = None;
    let mut cur_desc: Vec<String> = Vec::new();

    for line in block {
        if line.trim().is_empty() {
            continue;
        }
        if leading_spaces(line) == entry_indent {
            commit_param(&mut cur_name, &mut cur_desc, params);
            if let Some((name, inline_desc)) = split_param_head(line.trim(), numpy) {
                cur_name = Some(name);
                if !inline_desc.is_empty() {
                    cur_desc.push(inline_desc);
                }
            }
        } else if cur_name.is_some() {
            cur_desc.push(line.trim().to_string());
        }
    }
    commit_param(&mut cur_name, &mut cur_desc, params);
}

fn split_param_head(line: &str, numpy: bool) -> Option<(String, String)> {
    let (head, rest) = match line.split_once(':') {
        Some((h, r)) => (h, r),
        None if numpy => (line, ""),
        None => return None,
    };

    let name = clean_param_name(head);
    if name.is_empty() {
        return None;
    }

    let inline_desc = if numpy {
        String::new()
    } else {
        rest.trim().to_string()
    };
    Some((name, inline_desc))
}

fn clean_param_name(head: &str) -> String {
    head.split('(')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_start_matches('*')
        .to_string()
}

fn commit_param(
    cur_name: &mut Option<String>,
    cur_desc: &mut Vec<String>,
    params: &mut HashMap<String, String>,
) {
    if let Some(name) = cur_name.take() {
        let desc = cur_desc.join(" ").trim().to_string();
        cur_desc.clear();
        if !desc.is_empty() {
            params.entry(name).or_insert(desc);
        }
    }
}

fn parse_returns_block(
    lines: &[String],
    header_idx: usize,
    numpy: bool,
    returns: &mut Option<String>,
) -> usize {
    let header_indent = leading_spaces(&lines[header_idx]);
    let mut i = header_idx + 1;
    if numpy && i < lines.len() && is_dashes(lines[i].trim()) {
        i += 1;
    }

    let block_start = i;
    let end = block_end(lines, block_start, header_indent, numpy);

    let mut collected: Vec<String> = Vec::new();
    for line in &lines[block_start..end] {
        let t = line.trim();
        if !t.is_empty() {
            collected.push(t.to_string());
        }
    }
    if !collected.is_empty() && returns.is_none() {
        *returns = Some(collected.join(" "));
    }
    end
}

// ---------------------------------------------------------------------------
// Sphinx field parsing
// ---------------------------------------------------------------------------

fn parse_sphinx_param(
    lines: &[String],
    idx: usize,
    params: &mut HashMap<String, String>,
) -> usize {
    let base_indent = leading_spaces(&lines[idx]);
    let trimmed = lines[idx].trim();
    let after = trimmed
        .trim_start_matches(":parameter")
        .trim_start_matches(":param")
        .trim_start();
    let (name_part, desc_part) = match after.split_once(':') {
        Some(parts) => parts,
        None => return idx + 1,
    };
    let name = name_part
        .split_whitespace()
        .last()
        .unwrap_or("")
        .trim_matches(',')
        .trim_start_matches('*')
        .to_string();

    let mut desc: Vec<String> = Vec::new();
    let first = desc_part.trim();
    if !first.is_empty() {
        desc.push(first.to_string());
    }

    let mut i = idx + 1;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() {
            break;
        }
        if t.starts_with(':') || leading_spaces(&lines[i]) <= base_indent {
            break;
        }
        desc.push(t.to_string());
        i += 1;
    }

    if !name.is_empty() {
        let joined = desc.join(" ").trim().to_string();
        if !joined.is_empty() {
            params.entry(name).or_insert(joined);
        }
    }
    i
}

fn parse_sphinx_returns(
    lines: &[String],
    idx: usize,
    returns: &mut Option<String>,
) -> usize {
    let base_indent = leading_spaces(&lines[idx]);
    let trimmed = lines[idx].trim();
    let after = trimmed
        .trim_start_matches(":returns")
        .trim_start_matches(":return")
        .trim_start_matches(":yields")
        .trim_start_matches(":yield")
        .trim_start_matches(':')
        .trim();

    let mut desc: Vec<String> = Vec::new();
    if !after.is_empty() {
        desc.push(after.to_string());
    }
    let mut i = idx + 1;
    while i < lines.len() {
        let t = lines[i].trim();
        if t.is_empty() || t.starts_with(':') || leading_spaces(&lines[i]) <= base_indent {
            break;
        }
        desc.push(t.to_string());
        i += 1;
    }
    if returns.is_none() {
        let joined = desc.join(" ").trim().to_string();
        if !joined.is_empty() {
            *returns = Some(joined);
        }
    }
    i
}

// ---------------------------------------------------------------------------
// Tests (pure parser — no pyrefly required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_summary_params_and_returns() {
        let doc = "\"\"\"Compute a sum.\n\n    A longer explanation of the\n    computation.\n\n    Args:\n        a (int): the first addend.\n        b: the second addend,\n            spanning two lines.\n\n    Returns:\n        int: the total.\n    \"\"\"";
        let parsed = parse(doc);
        assert_eq!(parsed.summary.as_deref(), Some("Compute a sum."));
        assert_eq!(
            parsed.body.as_deref(),
            Some("A longer explanation of the\ncomputation.")
        );
        assert_eq!(
            parsed.params.get("a").map(String::as_str),
            Some("the first addend.")
        );
        assert_eq!(
            parsed.params.get("b").map(String::as_str),
            Some("the second addend, spanning two lines.")
        );
        assert_eq!(parsed.returns.as_deref(), Some("int: the total."));
    }

    #[test]
    fn numpy_params_and_returns() {
        let doc = "\"\"\"Short summary.\n\n    Parameters\n    ----------\n    a : int\n        first value\n    b : str\n        second value\n\n    Returns\n    -------\n    bool\n        whether it worked\n    \"\"\"";
        let parsed = parse(doc);
        assert_eq!(parsed.summary.as_deref(), Some("Short summary."));
        assert_eq!(
            parsed.params.get("a").map(String::as_str),
            Some("first value")
        );
        assert_eq!(
            parsed.params.get("b").map(String::as_str),
            Some("second value")
        );
        assert_eq!(parsed.returns.as_deref(), Some("bool whether it worked"));
    }

    #[test]
    fn sphinx_params_and_returns() {
        let doc = "\"\"\"Do a thing.\n\n    :param int a: the first param\n    :param b: the second param\n        with continuation\n    :returns: the result\n    \"\"\"";
        let parsed = parse(doc);
        assert_eq!(parsed.summary.as_deref(), Some("Do a thing."));
        assert_eq!(
            parsed.params.get("a").map(String::as_str),
            Some("the first param")
        );
        assert_eq!(
            parsed.params.get("b").map(String::as_str),
            Some("the second param with continuation")
        );
        assert_eq!(parsed.returns.as_deref(), Some("the result"));
    }

    #[test]
    fn summary_only() {
        let parsed = parse("\"\"\"Just a summary.\"\"\"");
        assert_eq!(parsed.summary.as_deref(), Some("Just a summary."));
        assert!(parsed.body.is_none());
        assert!(parsed.params.is_empty());
        assert!(parsed.returns.is_none());
    }

    #[test]
    fn unquoted_input_is_accepted() {
        let parsed = parse("Summary line.\n\nArgs:\n    x: a value\n");
        assert_eq!(parsed.summary.as_deref(), Some("Summary line."));
        assert_eq!(parsed.params.get("x").map(String::as_str), Some("a value"));
    }

    #[test]
    fn documentation_joins_summary_and_body() {
        let parsed = ParsedDocstring {
            summary: Some("Sum.".into()),
            body: Some("More.".into()),
            params: HashMap::new(),
            returns: None,
        };
        assert_eq!(parsed.documentation().as_deref(), Some("Sum.\n\nMore."));
    }

    #[test]
    fn empty_docstring_is_empty() {
        assert!(parse("\"\"\"\"\"\"").is_empty());
        assert!(parse("   ").is_empty());
    }

    #[test]
    fn doc_catalog_item_and_member() {
        let mut catalog = DocCatalog::default();
        let doc = parse("\"\"\"A class.\"\"\"");
        catalog.insert_item("MyClass", doc.clone());
        catalog.insert_member("MyClass", "my_method", parse("\"\"\"A method.\"\"\""));

        assert_eq!(
            catalog.item("MyClass").and_then(|d| d.summary.as_deref()),
            Some("A class.")
        );
        assert_eq!(
            catalog
                .member("MyClass", "my_method")
                .and_then(|d| d.summary.as_deref()),
            Some("A method.")
        );
        assert!(catalog.item("NoSuchItem").is_none());
    }
}
