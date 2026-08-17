//! Small, loss-aware Markdown primitives for MCP tool responses.
//!
//! This module deliberately does not depend on the MCP or wire types.  The
//! typed tool results can be projected into these primitives at the server
//! boundary without making Markdown part of the engine's data model.
//!
//! The helpers distinguish two jobs:
//!
//! * [`MarkdownTable`] renders compact, single-line cells.  Delimiters and
//!   control characters are escaped or made visible so an untrusted value
//!   cannot change the table shape or inject a link/HTML fragment.
//! * [`exact_fenced_snippet`] preserves its body byte-for-byte.  It chooses a
//!   fence longer than every matching delimiter run in the body, so source
//!   text containing fences remains source text.
//!
//! Empty values are rendered as `⟨empty⟩` in compact textual contexts.  This
//! is intentional: a blank cell or blank status would make absence
//! indistinguishable from a formatting error.  Newlines in compact contexts
//! become `↵`; exact fenced snippets are the escape hatch when newlines must
//! remain literal.

use std::fmt::{self, Write as _};

/// The visible marker used when a compact Markdown value is empty.
pub const EMPTY_MARKER: &str = "⟨empty⟩";

/// The visible marker used for a line ending in compact Markdown values.
pub const NEWLINE_MARKER: &str = "↵";

/// Escape a single-line Markdown text fragment.
///
/// This is suitable for status details and other prose that is not inside a
/// table.  It escapes Markdown punctuation, pipes, backslashes and HTML
/// delimiters, and turns line endings/control characters into visible text.
/// It never returns a string containing a literal line ending.
pub fn escape_markdown_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\r' {
            chars.next_if_eq(&'\n');
            out.push_str(NEWLINE_MARKER);
            continue;
        }

        if is_invisible_format(ch) {
            push_unicode_escape(&mut out, ch);
            continue;
        }

        match ch {
            '\n' => out.push_str(NEWLINE_MARKER),
            '\t' => out.push('⇥'),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            // Backslash must be emitted first so a value such as `\|` cannot
            // accidentally consume the escape we add for its pipe.
            '\\' => out.push_str("\\\\"),
            '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.' | '!'
            | '|' | '~' | ':' | '/' | '@' => {
                out.push('\\');
                out.push(ch);
            }
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:04X}}}", c as u32);
            }
            c => out.push(c),
        }
    }

    if out.is_empty() {
        EMPTY_MARKER.to_owned()
    } else {
        out
    }
}

/// Escape a value for one Markdown table cell.
///
/// Table cells use the same conservative escaping as
/// [`escape_markdown_text`].  In particular, `|` is escaped and all line
/// endings become [`NEWLINE_MARKER`], so a value cannot add a column or row.
pub fn escape_table_cell(value: &str) -> String {
    escape_markdown_text(value)
}

/// Render an arbitrary value as a safe inline Markdown code span.
///
/// The delimiter is one backtick longer than the longest consecutive run in
/// `value`, which makes embedded backticks inert.  Newlines and other control
/// characters are shown visibly because an inline span must remain one line.
pub fn code_span(value: &str) -> String {
    let content = compact_code_content(value);
    let delimiter_len = longest_run(&content, '`').saturating_add(1).max(1);
    let delimiter = "`".repeat(delimiter_len);

    // CommonMark removes one leading/trailing space from a code span when the
    // content has both.  Padding preserves ordinary edge spaces; an all-space
    // value is made visible by compact_code_content instead of being allowed
    // to disappear into Markdown's whitespace rules.
    let pad = content.starts_with(' ') || content.ends_with(' ');
    let mut out = String::with_capacity(delimiter.len() * 2 + content.len() + 2);
    out.push_str(&delimiter);
    if pad {
        out.push(' ');
    }
    out.push_str(&content);
    if pad {
        out.push(' ');
    }
    out.push_str(&delimiter);
    out
}

/// Render an exact source/documentation body inside a safe fenced block.
///
/// The returned body is not escaped or normalized.  The function chooses
/// backticks or tildes—whichever needs the shorter fence—and makes that fence
/// longer than every matching run in `body`.  `language` is included only if
/// it is a single conservative language tag; unsafe labels are omitted rather
/// than allowed to inject a second line or fence.
///
/// A trailing newline may be added after `body` when required to put the
/// closing fence on its own line.  That separator is Markdown framing, not a
/// mutation of the body represented inside the block.
pub fn exact_fenced_snippet(language: Option<&str>, body: &str) -> String {
    let backticks = longest_run(body, '`');
    let tildes = longest_run(body, '~');
    let use_tilde = tildes < backticks;
    let marker = if use_tilde { '~' } else { '`' };
    let fence_len = longest_run(body, marker).saturating_add(1).max(3);
    let fence: String = std::iter::repeat_n(marker, fence_len).collect();

    let language = language.and_then(safe_language_tag);
    let mut out = String::with_capacity(
        fence.len() * 2 + body.len() + language.as_ref().map_or(1, |tag| tag.len() + 1) + 2,
    );
    out.push_str(&fence);
    if let Some(language) = language {
        out.push_str(&language);
    }
    out.push('\n');
    out.push_str(body);
    if !body.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&fence);
    out.push('\n');
    out
}

/// Return the compact marker for a result with more rows.
///
/// The marker is intentionally independent of cursor text; callers can use
/// [`pagination_line`] when the cursor should be surfaced too.
pub fn truncation_marker(has_more: bool) -> Option<&'static str> {
    has_more.then_some("… more")
}

/// Render a compact pagination footer.
///
/// A cursor is shown only when `has_more` is true and it is non-empty.  An
/// inconsistent/empty cursor never produces a false claim that it can be
/// followed; the line says that more data exists but the cursor is missing.
pub fn pagination_line(shown: usize, has_more: bool, next_cursor: Option<&str>) -> String {
    let mut out = format!("{shown} shown");
    if !has_more {
        out.push_str(" · complete");
        return out;
    }

    out.push_str(" · ");
    if let Some(marker) = truncation_marker(true) {
        out.push_str(marker);
    }
    match next_cursor.filter(|cursor| !cursor.is_empty()) {
        Some(cursor) => {
            out.push_str(" · next ");
            out.push_str(&code_span(cursor));
        }
        None => out.push_str(" · next cursor unavailable"),
    }
    out
}

/// Render a compact status line with an optional escaped detail.
///
/// The status is a code span so values such as `ready` or `building` remain
/// visually distinct.  Empty and hostile values are represented safely by the
/// same rules as [`code_span`] and [`escape_markdown_text`].
pub fn status_line(status: &str, detail: Option<&str>) -> String {
    let mut out = String::from("status: ");
    out.push_str(&code_span(status));
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        out.push_str(" · ");
        out.push_str(&escape_markdown_text(detail));
    }
    out
}

/// An error returned when a table row does not match its header width.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableError {
    expected: usize,
    actual: usize,
}

impl TableError {
    /// The number of cells required by the table header.
    pub const fn expected(&self) -> usize {
        self.expected
    }

    /// The number of cells supplied by the rejected row.
    pub const fn actual(&self) -> usize {
        self.actual
    }
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Markdown table row has {} cells; expected {}",
            self.actual, self.expected
        )
    }
}

impl std::error::Error for TableError {}

/// A small owned Markdown table builder.
///
/// Rows are width-checked when added.  Failing instead of silently padding or
/// dropping cells is important for tool output: a malformed projection must
/// not make a value appear under the wrong column.  Cell values remain owned
/// so callers may build a response from temporary wire projections.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkdownTable {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl MarkdownTable {
    /// Create a table with the supplied column headings.
    pub fn new<I, S>(headers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            headers: headers.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
        }
    }

    /// Add one row, rejecting a cell count that does not match the headers.
    pub fn push_row<I, S>(&mut self, cells: I) -> Result<(), TableError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let cells: Vec<String> = cells.into_iter().map(Into::into).collect();
        if cells.len() != self.headers.len() {
            return Err(TableError {
                expected: self.headers.len(),
                actual: cells.len(),
            });
        }
        self.rows.push(cells);
        Ok(())
    }

    /// Return the number of columns in the table.
    pub fn column_count(&self) -> usize {
        self.headers.len()
    }

    /// Return the number of accepted data rows.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Render the table as compact GitHub-style Markdown.
    ///
    /// A table with no columns renders as an empty string.  A table with
    /// headings but no rows still renders its delimiter row, which lets a
    /// caller distinguish an empty result from a missing renderer output.
    pub fn render(&self) -> String {
        if self.headers.is_empty() {
            return String::new();
        }

        let mut out = String::new();
        render_table_row(&mut out, &self.headers);
        render_separator_row(&mut out, self.headers.len());
        for row in &self.rows {
            render_table_row(&mut out, row);
        }
        out
    }
}

fn render_table_row(out: &mut String, cells: &[String]) {
    out.push('|');
    for cell in cells {
        out.push(' ');
        out.push_str(&escape_table_cell(cell));
        out.push_str(" |");
    }
    out.push('\n');
}

fn render_separator_row(out: &mut String, columns: usize) {
    out.push('|');
    for _ in 0..columns {
        out.push_str(" --- |");
    }
    out.push('\n');
}

fn compact_code_content(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if is_invisible_format(ch) {
            push_unicode_escape(&mut out, ch);
            continue;
        }

        match ch {
            '\r' => {
                chars.next_if_eq(&'\n');
                out.push_str(NEWLINE_MARKER);
            }
            '\n' => out.push_str(NEWLINE_MARKER),
            '\t' => out.push('⇥'),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:04X}}}", c as u32);
            }
            c => out.push(c),
        }
    }

    if out.is_empty() {
        EMPTY_MARKER.to_owned()
    } else if out.chars().all(|ch| ch == ' ') {
        // Markdown's code-span whitespace rules otherwise make an all-space
        // value visually indistinguishable from an empty span.
        out.chars().map(|_| '␠').collect()
    } else {
        out
    }
}

fn longest_run(value: &str, needle: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for ch in value.chars() {
        if ch == needle {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{061C}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2060}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
            | '\u{FEFF}'
    )
}

fn push_unicode_escape(out: &mut String, ch: char) {
    let _ = write!(out, "\\u{{{:04X}}}", ch as u32);
}

fn safe_language_tag(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '.' | '#' | '/' | '-'))
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_cells_cannot_inject_columns_rows_or_markup() {
        let mut table = MarkdownTable::new(["name", "value", "empty"]);
        table
            .push_row(["a|b", "line\r\nbreak `x` <tag> https://x.test", ""])
            .unwrap();

        let rendered = table.render();
        assert!(rendered.contains(r"a\|b"));
        assert!(rendered.contains("line↵break"));
        assert!(rendered.contains(r"\`x\`"));
        assert!(rendered.contains("&lt;tag&gt;"));
        assert!(rendered.contains(r"https\:\/\/x\.test"));
        assert!(rendered.contains(EMPTY_MARKER));
        assert_eq!(rendered.lines().nth(1), Some("| --- | --- | --- |"));
        assert_eq!(rendered.lines().count(), 3);

        let bidi = escape_table_cell("safe\u{202E}text");
        assert!(bidi.contains(r"\u{202E}"));
    }

    #[test]
    fn table_rejects_wrong_width_without_mutating_rows() {
        let mut table = MarkdownTable::new(["a", "b"]);
        let error = table.push_row(["only one"]).unwrap_err();
        assert_eq!(error.expected(), 2);
        assert_eq!(error.actual(), 1);
        assert_eq!(table.row_count(), 0);

        table.push_row(["one", "two"]).unwrap();
        assert_eq!(table.row_count(), 1);
    }

    #[test]
    fn empty_and_backtick_heavy_code_spans_remain_single_inline_values() {
        assert_eq!(code_span(""), "`⟨empty⟩`");
        assert_eq!(code_span("line\nnext"), "`line↵next`");

        let rendered = code_span("left ``` right");
        assert!(rendered.starts_with("````"));
        assert!(rendered.ends_with("````"));
        assert!(rendered.contains("left ``` right"));
        assert!(!rendered[4..rendered.len() - 4].contains("````"));
    }

    #[test]
    fn exact_fence_preserves_body_and_cannot_be_closed_by_body_text() {
        let body = "before\n````rust\n~~~\nafter";
        let rendered = exact_fenced_snippet(Some("rust\n```"), body);

        // The unsafe language label is omitted, and the shorter safe tilde
        // fence is chosen because the body contains a longer backtick run.
        assert!(rendered.starts_with("~~~~\n"));
        assert!(!rendered.starts_with("~~~~rust"));
        assert!(rendered.contains(body));
        assert!(rendered.ends_with("\n~~~~\n"));
        assert_eq!(rendered.matches("\n~~~~\n").count(), 1);

        let backtick_body = "before\n```\n~~~\nafter";
        let backtick_rendered = exact_fenced_snippet(None, backtick_body);
        assert!(backtick_rendered.starts_with("````\n"));
        assert!(backtick_rendered.contains(backtick_body));
        assert_eq!(backtick_rendered.matches("\n````\n").count(), 1);
    }

    #[test]
    fn exact_empty_fence_is_still_visibly_nonempty() {
        let rendered = exact_fenced_snippet(None, "");
        assert!(rendered.starts_with("```\n"));
        assert_eq!(rendered, "```\n\n```\n");
    }

    #[test]
    fn pagination_does_not_claim_a_missing_cursor_is_followable() {
        assert_eq!(
            pagination_line(3, false, Some("stale")),
            "3 shown · complete"
        );
        assert_eq!(
            pagination_line(3, true, Some("")),
            "3 shown · … more · next cursor unavailable"
        );
        assert_eq!(
            pagination_line(3, true, Some("a`b\nc")),
            "3 shown · … more · next ``a`b↵c``"
        );
    }

    #[test]
    fn status_line_is_one_line_and_marks_empty_status() {
        let rendered = status_line("rea`dy\n", Some("detail | <unsafe>"));
        assert_eq!(rendered, "status: ``rea`dy↵`` · detail \\| &lt;unsafe&gt;");
        assert!(!rendered.contains('\n'));
        assert!(status_line("", Some("")).contains(EMPTY_MARKER));
    }

    #[test]
    fn truncation_marker_is_explicit() {
        assert_eq!(truncation_marker(false), None);
        assert_eq!(truncation_marker(true), Some("… more"));
    }
}
