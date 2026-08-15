//! Compact Markdown projection for occurrence queries.
//!
//! This module deliberately knows nothing about Trustfall, the graph adapter,
//! or MCP transport.  The tools layer only has to map its query rows into
//! [`OccurrenceRow`] values and call [`render_occurrences`].  Keeping the
//! projection here makes the important occurrence semantics visible at the
//! formatting boundary:
//!
//! * `span_start` and `span_end` are relative to the owner's declaration span;
//! * `target_key` survives even when the optional target vertex is unloaded;
//! * repeated target and owner identity is represented by headings, not rows;
//! * the cursor is opaque and is rendered without being interpreted.

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// The display fields available for a resolved symbol.
///
/// The key is kept separately on [`OccurrenceRow::target_key`] for targets so
/// an unloaded target can still be identified.  For owners the key is always
/// present because an occurrence cannot exist without its owner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SymbolSummary {
    /// Complete rendered declaration signature, when the target/owner is
    /// loaded. This is the primary identity shown to an agent.
    pub signature: Option<String>,
    /// The unqualified symbol name, when the graph query selected it.
    pub name: Option<String>,
    /// The canonical kind label, when the graph query selected it.
    pub kind: Option<String>,
    /// The package-relative declaration path, when the graph query selected it.
    pub path: Option<String>,
}

/// The owner of an occurrence: the symbol whose body contains the reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccurrenceOwner {
    /// The owner's stable symbol key.
    pub key: String,
    /// Optional display enrichment selected by the query.
    pub summary: SymbolSummary,
}

/// One exact reference to a target symbol.
///
/// `target` is `None` when the target key could not be resolved in the loaded
/// corpus.  That is not the same as an absent target key: the required
/// [`OccurrenceRow::target_key`] is still rendered in that case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OccurrenceRow {
    /// The stable key named by this occurrence.
    pub target_key: String,
    /// Resolved target display fields, or `None` for an unloaded target.
    pub target: Option<SymbolSummary>,
    /// The symbol containing this reference.
    pub owner: OccurrenceOwner,
    /// The reference category supplied by the graph (`call`, `type`, etc.).
    pub reference_kind: String,
    /// The producer's confidence label for this occurrence.
    pub confidence: String,
    /// Start offset relative to the owner's declaration span start.
    pub span_start: i64,
    /// End offset relative to the owner's declaration span start.
    pub span_end: i64,
}

/// Whether the rendered page is complete or represents a partial result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OccurrenceStatus {
    /// The page is the complete result and has no unfinished backend state.
    Complete,
    /// Rows are usable, but the backend has more work or information to expose.
    Partial {
        /// Optional human-readable reason, such as `indexing` or `more rows`.
        detail: Option<String>,
    },
    /// The occurrence projection is currently unavailable.
    Unavailable {
        /// The reason an agent should act on or report.
        detail: String,
    },
}

/// Render occurrence rows as compact, grouped Markdown.
///
/// Rows are grouped by target key and then owner key.  Target and owner
/// identity therefore appears once in a heading, while the table contains one
/// row per occurrence.  Within an owner group rows are sorted by relative
/// span, making output deterministic even when a graph backend does not
/// guarantee row order.
///
/// `next_cursor` is opaque: it is copied into the status line and never
/// parsed, shortened, or used to infer a page size.  A cursor is enough to
/// communicate pagination, so no redundant `truncated` flag is emitted.
pub fn render_occurrences(
    rows: &[OccurrenceRow],
    status: &OccurrenceStatus,
    next_cursor: Option<&str>,
) -> String {
    let mut output = String::new();
    let page_qualifier = if next_cursor.is_some() {
        " on this page"
    } else {
        ""
    };

    if rows.is_empty() {
        let _ = writeln!(
            output,
            "## Occurrences · {}{}\n",
            rows.len(),
            page_qualifier
        );
        render_status(&mut output, status, next_cursor);
        output.push('\n');
        output.push_str("No occurrences on this page.\n\n");
    } else {
        let target_groups = group_by_target(rows);
        let target_count = target_groups.len();
        let target_qualifier = if target_count > 1 {
            format!(" · {target_count} targets")
        } else {
            String::new()
        };
        let _ = writeln!(
            output,
            "## Occurrences · {}{}{}\n",
            rows.len(),
            page_qualifier,
            target_qualifier
        );
        render_status(&mut output, status, next_cursor);
        output.push('\n');

        for (target_key, target_rows) in target_groups {
            render_target_group(&mut output, target_key, &target_rows);
        }
    }

    output
}

fn group_by_target<'a>(rows: &'a [OccurrenceRow]) -> Vec<(&'a str, Vec<&'a OccurrenceRow>)> {
    let mut grouped: BTreeMap<&str, Vec<&OccurrenceRow>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.target_key.as_str())
            .or_default()
            .push(row);
    }
    grouped.into_iter().collect()
}

fn group_by_owner<'a>(rows: &[&'a OccurrenceRow]) -> Vec<(&'a str, Vec<&'a OccurrenceRow>)> {
    let mut grouped: BTreeMap<&str, Vec<&OccurrenceRow>> = BTreeMap::new();
    for row in rows {
        grouped
            .entry(row.owner.key.as_str())
            .or_default()
            .push(*row);
    }
    grouped.into_iter().collect()
}

fn render_target_group(output: &mut String, target_key: &str, rows: &[&OccurrenceRow]) {
    let resolved = rows.iter().find_map(|row| row.target.as_ref());
    let unresolved_rows = rows.iter().filter(|row| row.target.is_none()).count();

    let _ = write!(output, "### target {}", inline_code(target_key));
    match resolved {
        Some(summary) => {
            render_summary(output, summary);
            if unresolved_rows != 0 {
                let _ = write!(output, " · {}", inline_code("partially unresolved"));
            }
        }
        None => {
            let _ = write!(output, " · {}", inline_code("unloaded target"));
        }
    }
    output.push('\n');

    for (owner_key, mut owner_rows) in group_by_owner(rows) {
        owner_rows.sort_by(|left, right| {
            left.span_start
                .cmp(&right.span_start)
                .then_with(|| left.span_end.cmp(&right.span_end))
                .then_with(|| left.reference_kind.cmp(&right.reference_kind))
                .then_with(|| left.confidence.cmp(&right.confidence))
        });
        render_owner_group(output, owner_key, &owner_rows);
    }
}

fn render_owner_group(output: &mut String, owner_key: &str, rows: &[&OccurrenceRow]) {
    let owner = rows
        .first()
        .expect("an owner group is created from at least one row")
        .owner
        .summary
        .clone();

    let _ = write!(output, "#### owner {}", inline_code(owner_key));
    render_summary(output, &owner);
    output.push_str("\n\n");
    output.push_str("| owner-relative bytes | reference | confidence |\n");
    output.push_str("|---:|---|---|\n");

    for row in rows {
        let span = format!("{:+}..{:+}", row.span_start, row.span_end);
        let _ = writeln!(
            output,
            "| {} | {} | {} |",
            inline_code(&span),
            inline_code(&row.reference_kind),
            inline_code(&row.confidence),
        );
    }
    output.push('\n');
}

fn render_summary(output: &mut String, summary: &SymbolSummary) {
    if let Some(signature) = summary.signature.as_deref() {
        let _ = write!(output, " · {}", inline_code(signature));
    } else {
        for value in [&summary.name, &summary.kind, &summary.path]
            .into_iter()
            .flatten()
        {
            let _ = write!(output, " · {}", inline_code(value));
        }
    }
}

fn render_status(output: &mut String, status: &OccurrenceStatus, next_cursor: Option<&str>) {
    output.push_str("status: ");
    match status {
        OccurrenceStatus::Complete => output.push_str("complete"),
        OccurrenceStatus::Partial { detail } => {
            output.push_str("partial");
            render_detail(output, detail.as_deref());
        }
        OccurrenceStatus::Unavailable { detail } => {
            output.push_str("unavailable");
            render_detail(output, Some(detail));
        }
    }

    if let Some(cursor) = next_cursor {
        let _ = write!(output, " · next: {}", inline_code(cursor));
    }
    output.push('\n');
}

fn render_detail(output: &mut String, detail: Option<&str>) {
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        let _ = write!(output, " · {}", inline_code(detail));
    }
}

/// Render untrusted text as an inline Markdown code value.
///
/// A normal value uses the shortest safe backtick fence.  Values that could
/// terminate a pipe table or inject a line break use HTML entities inside
/// `<code>` instead; this keeps the displayed value faithful while preserving
/// the table's column structure.
fn inline_code(value: &str) -> String {
    let needs_html = value.contains('|') || value.chars().any(char::is_control);
    let value = visible_controls(value);
    if value.is_empty()
        || needs_html
        || value.starts_with(char::is_whitespace)
        || value.ends_with(char::is_whitespace)
    {
        return html_code(&value);
    }

    let fence_len = longest_backtick_run(&value).saturating_add(1);
    let fence = "`".repeat(fence_len);
    format!("{fence}{value}{fence}")
}

fn visible_controls(value: &str) -> String {
    let mut rendered = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(rendered, "\\u{{{:04x}}}", character as u32);
            }
            character => rendered.push(character),
        }
    }
    rendered
}

fn longest_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in value.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn html_code(value: &str) -> String {
    let mut rendered = String::from("<code>");
    for character in value.chars() {
        match character {
            '&' => rendered.push_str("&amp;"),
            '<' => rendered.push_str("&lt;"),
            '>' => rendered.push_str("&gt;"),
            '"' => rendered.push_str("&quot;"),
            '\'' => rendered.push_str("&#39;"),
            '|' => rendered.push_str("&#124;"),
            character => rendered.push(character),
        }
    }
    rendered.push_str("</code>");
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(key: &str, name: &str, path: &str) -> OccurrenceOwner {
        OccurrenceOwner {
            key: key.to_owned(),
            summary: SymbolSummary {
                signature: None,
                name: Some(name.to_owned()),
                kind: Some("function".to_owned()),
                path: Some(path.to_owned()),
            },
        }
    }

    fn row(
        target_key: &str,
        target: Option<SymbolSummary>,
        owner: OccurrenceOwner,
        start: i64,
        end: i64,
    ) -> OccurrenceRow {
        OccurrenceRow {
            target_key: target_key.to_owned(),
            target,
            owner,
            reference_kind: "call".to_owned(),
            confidence: "exact".to_owned(),
            span_start: start,
            span_end: end,
        }
    }

    fn target(name: &str) -> SymbolSummary {
        SymbolSummary {
            signature: None,
            name: Some(name.to_owned()),
            kind: Some("function".to_owned()),
            path: Some("src/lib.rs".to_owned()),
        }
    }

    #[test]
    fn labels_spans_as_owner_relative_and_keeps_signed_offsets() {
        let rows = vec![row(
            "cargo:demo#target",
            Some(target("retry")),
            owner("cargo:demo#owner", "run", "src/main.rs"),
            -2,
            7,
        )];

        let rendered = render_occurrences(&rows, &OccurrenceStatus::Complete, None);

        assert!(rendered.contains("owner-relative bytes"));
        assert!(rendered.contains("`-2..+7`"));
        assert!(!rendered.contains("absolute"));
    }

    #[test]
    fn missing_target_is_explicit_but_target_key_is_not_lost() {
        let rows = vec![row(
            "cargo:missing#target",
            None,
            owner("cargo:demo#owner", "run", "src/main.rs"),
            12,
            18,
        )];

        let rendered = render_occurrences(&rows, &OccurrenceStatus::Complete, None);

        assert!(rendered.contains("`cargo:missing#target`"));
        assert!(rendered.contains("`unloaded target`"));
        assert_eq!(rendered.matches("cargo:missing#target").count(), 1);
    }

    #[test]
    fn groups_multiple_owners_without_repeating_identity_in_rows() {
        let rows = vec![
            row(
                "cargo:demo#target",
                Some(target("retry")),
                owner("cargo:demo#z-owner", "z", "src/z.rs"),
                30,
                35,
            ),
            row(
                "cargo:demo#target",
                Some(target("retry")),
                owner("cargo:demo#a-owner", "a", "src/a.rs"),
                2,
                8,
            ),
        ];

        let rendered = render_occurrences(&rows, &OccurrenceStatus::Complete, None);

        assert_eq!(rendered.matches("### target").count(), 1);
        assert_eq!(rendered.matches("#### owner").count(), 2);
        assert_eq!(rendered.matches("cargo:demo#target").count(), 1);
        assert_eq!(rendered.matches("cargo:demo#a-owner").count(), 1);
        assert_eq!(rendered.matches("cargo:demo#z-owner").count(), 1);
        assert!(rendered.find("+2..+8").unwrap() < rendered.find("+30..+35").unwrap());
    }

    #[test]
    fn hostile_markdown_values_cannot_break_the_table() {
        let mut occurrence = row(
            "target|`key`<&",
            Some(SymbolSummary {
                signature: None,
                name: Some("name * [x]".to_owned()),
                kind: Some("kind|x".to_owned()),
                path: Some("src/<bad>\nnext".to_owned()),
            }),
            owner("owner|key", "owner `name`", "src/a|b.rs"),
            1,
            2,
        );
        occurrence.reference_kind = "call|pipe".to_owned();
        occurrence.confidence = "[exact]*".to_owned();

        let rendered = render_occurrences(
            &[occurrence],
            &OccurrenceStatus::Partial {
                detail: Some("still *loading*".to_owned()),
            },
            Some("cursor|next"),
        );

        assert!(rendered.contains("target&#124;`key`&lt;&amp;"));
        assert!(rendered.contains("owner&#124;key"));
        assert!(rendered.contains("src/&lt;bad&gt;\\nnext"));
        assert!(rendered.contains("call&#124;pipe"));
        assert!(rendered.contains("cursor&#124;next"));
        assert!(!rendered.contains("target|`key`<&"));
        assert!(!rendered.contains("owner|key"));
    }

    #[test]
    fn zero_rows_still_expose_status_without_an_empty_table() {
        let rendered = render_occurrences(&[], &OccurrenceStatus::Complete, None);

        assert!(rendered.contains("## Occurrences · 0"));
        assert!(rendered.contains("No occurrences on this page."));
        assert!(rendered.contains("status: complete"));
        assert!(!rendered.contains("owner-relative bytes"));
        assert!(!rendered.contains("|---"));
    }

    #[test]
    fn pagination_and_unavailable_status_are_compact_but_unambiguous() {
        let rows = vec![row(
            "cargo:demo#target",
            None,
            owner("cargo:demo#owner", "run", "src/main.rs"),
            0,
            1,
        )];
        let rendered = render_occurrences(
            &rows,
            &OccurrenceStatus::Partial {
                detail: Some("more rows".to_owned()),
            },
            Some("opaque-cursor"),
        );

        assert!(rendered.contains("## Occurrences · 1 on this page"));
        assert!(rendered.contains("status: partial · `more rows` · next: `opaque-cursor`"));

        let unavailable = render_occurrences(
            &[],
            &OccurrenceStatus::Unavailable {
                detail: "index is building".to_owned(),
            },
            None,
        );
        assert!(unavailable.contains("status: unavailable · `index is building`"));
    }
}
