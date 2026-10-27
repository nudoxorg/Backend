//! The agent-facing rendering of the presentation model.
//!
//! Every rule here exists because the previous generation learned it the hard
//! way:
//!
//! * **One record per line.** Markdown table cells escape `\` and `|`, which
//!   silently corrupts exactly the two things an agent must copy verbatim —
//!   Windows paths and signatures with union or closure types. There are no
//!   tables in this renderer.
//! * **The coordinate on its own line.** An agent that has to extract an
//!   address out of a sentence extracts it wrongly.
//! * **A `~` signal line for coverage.** Cheap to emit, cheap to read, and it
//!   makes a thin answer visibly thin instead of quietly empty.
//! * **Evidence in fences.** A fenced block with the page's own language tag
//!   is the only place code appears, so nothing else needs escaping.
//! * **Counts, not dumps.** `oracles 0/18 ready · 18 no-manifest` is the same
//!   fact as eighteen records, at one seventieth of the tokens.

use core::fmt::Write as _;

use super::{ContinuationTarget, Lines, continuation_note};
use crate::coverage::CoverageLine;
use crate::drive::Answer;
use crate::fault::Fault;
use crate::glyph::KindGlyph;
use crate::identity::ProjectRef;
use crate::outline::{OutlineEntry, OutlineTree};
use crate::page::{MemberGroup, Page, Prose, RelationGroup, Source, Truncation};
use crate::product::{CursorTarget, IndexJobProjection, ProductView, semantic_history_details};
use crate::record::{Record, RecordList};
use crate::shelf::Shelf;
use crate::status::Status;
use crate::{Detail, MAX_PREVIEW_TEXT_BYTES, PageProjection, bounded_text};

/// Renders one `~lanes` coverage statement.
#[must_use]
pub fn coverage(line: CoverageLine) -> String {
    line.render()
}

/// Renders whichever answer a surface produced.
///
/// This is the single entry point an agent-facing surface calls. The CLI's
/// `--format markdown` and the MCP text block both reach it, which is what
/// makes them byte-identical rather than merely similar.
#[must_use]
pub fn answer(answer: &Answer) -> String {
    match answer {
        Answer::Page(value) => page(value),
        Answer::Records(value) => records(value, None),
        Answer::Shelf(value) => shelf(value),
        Answer::Outline(value) => outline(value),
        Answer::Status(value) => status(value),
        Answer::Product(value) => product(value),
    }
}

/// Renders an answer with the portable cursor issued by its caller.
#[must_use]
pub fn answer_with_cursor(
    value: &Answer,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    answer_with_detail(value, Detail::Standard, cursor, target)
}

/// Renders page fields selected by detail; other answer policies are unchanged.
#[must_use]
pub fn answer_with_detail(
    value: &Answer,
    detail: Detail,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    match value {
        Answer::Page(value) => page_projection(&PageProjection::new(value, detail)),
        Answer::Records(_) if !matches!(target, ContinuationTarget::CliOption) => {
            summary_with_cursor(value, cursor, target)
        }
        Answer::Records(value) => records_with_cursor(value, None, cursor, target),
        Answer::Product(value) => product_projection(value, Some(detail), Some(target)),
        Answer::Outline(value) => {
            let mut text = outline(value);
            if let Some(cursor) = cursor.filter(|_| value.continuation().is_some()) {
                text.push_str("\n\n");
                text.push_str(&continuation_note(target, cursor));
            }
            text
        }
        _ => answer(value),
    }
}

/// Renders a bounded text view of a structured Summary list.
/// Other answers retain their existing readable representation.
#[must_use]
pub fn summary_with_cursor(
    value: &Answer,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    if matches!(target, ContinuationTarget::CliOption) {
        return answer_with_cursor(value, cursor, target);
    }
    let Answer::Records(list) = value else {
        return answer_with_detail(value, Detail::Summary, cursor, target);
    };
    if list.is_empty() {
        let mut lines = Lines::new();
        lines.push(coverage(list.coverage()));
        lines.push(list.empty_explanation());
        if let Some(note) = summary_continuation(list, cursor, target) {
            lines.push(note);
        }
        return bounded_text(&lines.finish());
    }

    let coverage_line = coverage(list.coverage());
    let total = list.records().len();
    let mut footer = summary_continuation(list, cursor, target);
    // Reserve the longest possible count line before adding records. Both
    // shown and omitted are at most `total`, so the final line cannot grow
    // beyond this reservation.
    let count_reservation = record_preview_count(total, total, total);
    let heading_bytes = coverage_line
        .len()
        .saturating_add(1)
        .saturating_add(count_reservation.len())
        .saturating_add(1);
    if footer.as_ref().is_some_and(|line| {
        line.len().saturating_add(1) > MAX_PREVIEW_TEXT_BYTES.saturating_sub(heading_bytes)
    }) {
        footer = if list.has_more()
            && (list.continuation().is_some()
                || (cursor.is_some() && matches!(target, ContinuationTarget::McpInvocation(_))))
        {
            Some(match target {
                ContinuationTarget::McpInvocation(_) => {
                    "… more rows; copy `structuredContent.continuation_call` to continue".to_owned()
                }
                _ => "… more rows; use `structuredContent.nextCursor` to continue".to_owned(),
            })
        } else if list.has_more() {
            Some("… more rows at this revision; raise `limit` to see them".to_owned())
        } else {
            None
        };
    }
    let fixed_bytes = heading_bytes.saturating_add(
        footer
            .as_ref()
            .map_or(0, |line| line.len().saturating_add(1)),
    );
    let row_budget = MAX_PREVIEW_TEXT_BYTES.saturating_sub(fixed_bytes);
    let mut rendered_rows = String::new();
    let mut shown = 0;
    for record in list.records() {
        let Some(row) = mcp_summary_record(record) else {
            break;
        };
        if rendered_rows.len().saturating_add(row.len()) > row_budget {
            break;
        }
        rendered_rows.push_str(&row);
        shown += 1;
    }
    let omitted = total.saturating_sub(shown);
    let count_line = record_preview_count(total, shown, omitted);

    let mut preview = String::with_capacity(fixed_bytes.saturating_add(rendered_rows.len()));
    preview.push_str(&coverage_line);
    preview.push('\n');
    preview.push_str(&rendered_rows);
    preview.push_str(&count_line);
    preview.push('\n');
    if let Some(footer) = footer {
        preview.push_str(&footer);
        preview.push('\n');
    }
    debug_assert!(preview.len() <= MAX_PREVIEW_TEXT_BYTES);
    preview
}

/// Shrinks only the shared MCP record-preview dialect, retaining its coverage,
/// exact whole row operands, corrected counts and continuation instructions.
/// This is a text projection, never a source of row or cursor authority. Other
/// text and malformed preview claims return `None` for the caller's fallback.
/// A budget below the fixed coverage/count/footer size returns that zero-row
/// minimum intact; the final transport must admit or refuse it, never cut it.
#[must_use]
pub fn shorten_record_preview(text: &str, budget: usize) -> Option<String> {
    let preview = RecordPreviewText::parse(text)?;
    let reservation = record_preview_count(preview.total, preview.total, preview.total);
    let fixed = preview
        .header
        .len()
        .checked_add(reservation.len())?
        .checked_add(1)?
        .checked_add(preview.footer.len())?;
    let available = budget.saturating_sub(fixed);
    let mut row_bytes = 0_usize;
    let mut shown = 0;
    for row in &preview.rows {
        let next = row_bytes.checked_add(row.len())?;
        if next > available {
            break;
        }
        row_bytes = next;
        shown += 1;
    }
    let mut out = String::with_capacity(fixed.checked_add(row_bytes)?);
    out.push_str(preview.header);
    for row in &preview.rows[..shown] {
        out.push_str(row);
    }
    out.push_str(&record_preview_count(
        preview.total,
        shown,
        preview.total - shown,
    ));
    out.push('\n');
    out.push_str(preview.footer);
    debug_assert!(out.len() <= budget.max(fixed));
    Some(out)
}

fn record_preview_count(total: usize, shown: usize, omitted: usize) -> String {
    format!(
        "{total} row(s) on this page; showing {shown} exact coordinate(s); {omitted} omitted from text preview. Full page: `structuredContent.records`."
    )
}

/// A borrowed, fully validated layout of the renderer's own bounded row text.
struct RecordPreviewText<'a> {
    header: &'a str,
    rows: Vec<&'a str>,
    total: usize,
    footer: &'a str,
}

impl<'a> RecordPreviewText<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let (coverage, mut rest) = text.split_once('\n')?;
        if !coverage.starts_with('~') || text.len() > MAX_PREVIEW_TEXT_BYTES {
            return None;
        }
        let header = &text[..coverage.len() + 1];
        let mut rows = Vec::new();
        while rest.starts_with("• ") {
            // Work and borrowed row indexes are bounded by the shared preview
            // bytes; legacy Related may describe more than one query page.
            let (label, after_label) = rest.split_once('\n')?;
            let (coordinate, after_row) = after_label.split_once('\n')?;
            let label_end = preview_code_span_end(label.strip_prefix("• ")?)? + "• ".len();
            let operand = coordinate.strip_prefix("  ")?;
            if !label[label_end..].starts_with(" · ")
                || preview_code_span_end(operand)? != operand.len()
            {
                return None;
            }
            let consumed = rest.len() - after_row.len();
            rows.push(&rest[..consumed]);
            rest = after_row;
        }
        let (count, footer) = rest.split_once('\n')?;
        let (total, count) = count.split_once(" row(s) on this page; showing ")?;
        let (shown, count) = count.split_once(" exact coordinate(s); ")?;
        let omitted = count
            .strip_suffix(" omitted from text preview. Full page: `structuredContent.records`.")?;
        let total: usize = total.parse().ok()?;
        let shown: usize = shown.parse().ok()?;
        let omitted: usize = omitted.parse().ok()?;
        if shown != rows.len() || shown.checked_add(omitted)? != total {
            return None;
        }
        // The renderer emits either no footer or exactly one continuation line.
        if !footer.is_empty() {
            let line = footer.strip_suffix('\n')?;
            if line.contains('\n')
                || !(line.starts_with("Continue `") || line.starts_with("… more rows"))
            {
                return None;
            }
        }
        Some(Self {
            header,
            rows,
            total,
            footer,
        })
    }
}

fn preview_code_span_end(span: &str) -> Option<usize> {
    let delimiter = span.bytes().take_while(|byte| *byte == b'`').count();
    if delimiter == 0 {
        return None;
    }
    let bytes = span.as_bytes();
    let mut position = delimiter;
    while position < bytes.len() {
        if bytes[position] != b'`' {
            position += 1;
            continue;
        }
        let start = position;
        while bytes.get(position) == Some(&b'`') {
            position += 1;
        }
        if position - start == delimiter {
            return Some(position);
        }
    }
    None
}

fn summary_continuation(
    list: &RecordList,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> Option<String> {
    // A caller can supply the exact executable surface cursor separately
    // from the native page value. Rendering it never admits native authority.
    if list.has_more()
        && (list.continuation().is_some()
            || (cursor.is_some() && matches!(target, ContinuationTarget::McpInvocation(_))))
    {
        Some(cursor.map_or_else(
            || "… more rows; this rendering does not include the continuation token".to_owned(),
            |cursor| continuation_note(target, cursor),
        ))
    } else if list.has_more() {
        Some("… more rows at this revision; raise `limit` to see them".to_owned())
    } else {
        None
    }
}

fn mcp_summary_record(record: &Record) -> Option<String> {
    let name = record.identity().name();
    let coordinate = record.identity().coordinate().as_str();
    let mut tags = Vec::with_capacity(3);
    if let Some(kind) = record.kind() {
        tags.push(kind.name());
    }
    if record.is_external_reference() {
        tags.push("external reference");
    }
    if !record.is_external_reference() || record.language() != crate::Language::Unknown {
        tags.push(record.language().name());
    }
    if record.state() != crate::record::RecordState::Ready {
        tags.push(record.state().name());
    }
    // Check the raw size before allocating the rendered row. The two code
    // spans add their delimiters and optional CommonMark padding below.
    let raw_bytes = name
        .len()
        .saturating_add(coordinate.len())
        .saturating_add(tags.iter().map(|tag| tag.len()).sum::<usize>())
        .saturating_add(tags.len().saturating_mul(4))
        .saturating_add(10);
    if raw_bytes > MAX_PREVIEW_TEXT_BYTES {
        return None;
    }

    // Refuse identities that can add visual lines or reorder what a reader
    // sees. The typed payload still carries these exact values; this omission
    // affects only the human-readable projection.
    let name_ticks = inline_code_delimiter(name)?;
    let coordinate_ticks = inline_code_delimiter(coordinate)?;

    let name_padding = code_span_needs_padding(name);
    let coordinate_padding = code_span_needs_padding(coordinate);
    let mut row = String::with_capacity(
        raw_bytes
            .saturating_add(name_ticks.saturating_mul(2))
            .saturating_add(coordinate_ticks.saturating_mul(2))
            .saturating_add(if name_padding { 2 } else { 0 })
            .saturating_add(if coordinate_padding { 2 } else { 0 }),
    );
    row.push_str("• ");
    append_code_span(&mut row, name, name_ticks, name_padding);
    let _ = write!(&mut row, " · {}", tags[0]);
    for tag in tags.iter().skip(1) {
        let _ = write!(&mut row, " · {tag}");
    }
    row.push('\n');
    row.push_str("  ");
    append_code_span(&mut row, coordinate, coordinate_ticks, coordinate_padding);
    row.push('\n');
    if row.len() > MAX_PREVIEW_TEXT_BYTES {
        return None;
    }
    Some(row)
}

/// Returns the shortest backtick delimiter that can contain this value as a
/// CommonMark code span. Control and bidi formatting characters are omitted
/// from text so they cannot create forged rows or visually reorder operands.
fn inline_code_delimiter(value: &str) -> Option<usize> {
    if value.is_empty()
        || value.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\u{061c}'
                        | '\u{200e}'
                        | '\u{200f}'
                        | '\u{2028}'
                        | '\u{2029}'
                        | '\u{202a}'..='\u{202e}'
                        | '\u{2066}'..='\u{2069}'
                )
        })
    {
        return None;
    }

    let mut longest: usize = 0;
    let mut run = 0;
    for character in value.chars() {
        if character == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    Some(longest.saturating_add(1))
}

fn code_span_needs_padding(value: &str) -> bool {
    let needs_padding = matches!(value.chars().next(), Some(' ' | '`'))
        || matches!(value.chars().next_back(), Some(' ' | '`'));
    needs_padding && value.chars().any(|character| character != ' ')
}

fn append_code_span(output: &mut String, value: &str, delimiter: usize, padding: bool) {
    output.extend(std::iter::repeat_n('`', delimiter));
    if padding {
        output.push(' ');
    }
    output.push_str(value);
    if padding {
        output.push(' ');
    }
    output.extend(std::iter::repeat_n('`', delimiter));
}

/// Renders one fault, with its affordance as the exact next tool call.
#[must_use]
pub fn fault(fault: &Fault) -> String {
    let operand = fault.operand().render();
    let mut out = if operand.is_empty() {
        format!("✗ {}", fault.slug())
    } else {
        format!("✗ {} `{operand}`", fault.slug())
    };
    let _ = write!(out, "\n{}", fault.cause().sentence());
    if let Some(call) = fault.affordance().tool_call() {
        let _ = write!(out, "\n→ `{}`", tool_call_text(&call));
    } else if matches!(
        fault.affordance(),
        crate::Affordance::RecoverOwner { .. }
            | crate::Affordance::ReadPackageGraphJson { .. }
            | crate::Affordance::RecoverStartup { .. }
    ) && let Some(shell) = fault.affordance().shell()
    {
        // An indented code block preserves even literal backticks and newlines
        // inside quoted path operands without inventing an MCP scope change.
        let location = if matches!(
            fault.affordance(),
            crate::Affordance::ReadPackageGraphJson { .. }
        ) {
            "Run on the machine serving this request to read the same input as complete JSON"
        } else {
            "Run in a shell"
        };
        let _ = write!(
            out,
            "\n→ {location}:\n\n    {}",
            shell.replace('\n', "\n    ")
        );
    }
    out
}

/// Spells one tool call as `{"name":…,"arguments":…}`, in MCP's own
/// `tools/call` field order.
///
/// `serde_json::Value` objects order their keys by whatever `serde_json`
/// features the build happened to unify (sorted by default, insertion order
/// once any crate enables `preserve_order`), so displaying the value directly
/// made these bytes depend on which packages shared the build.
fn tool_call_text(call: &serde_json::Value) -> String {
    let null = serde_json::Value::Null;
    let name = call.get("name").unwrap_or(&null);
    let arguments = call.get("arguments").unwrap_or(&null);
    format!("{{\"name\":{name},\"arguments\":{arguments}}}")
}

/// Renders one declaration page, identity first.
#[must_use]
pub fn page(page: &Page) -> String {
    page_projection(&PageProjection::new(page, Detail::Standard))
}

/// Renders a borrowed typed page projection without reinterpreting JSON.
#[must_use]
pub fn page_projection(page: &PageProjection<'_>) -> String {
    let mut lines = Lines::new();
    lines.push(format!("# {}", page.identity().trail_within(None)));
    lines.push(meta_line(page));
    lines.push(format!("`{}`", page.identity().coordinate()));
    if let Some(signature) = page.signature() {
        lines.blank();
        lines.push(format!("```{}", page.language().fence()));
        lines.push(signature.text());
        lines.push("```");
    }
    if let Some(prose) = page.prose().included() {
        push_prose(&mut lines, prose);
    }
    if let Some(members) = page.members().included() {
        push_members(&mut lines, members, page.identity().project());
    }
    if let Some(relations) = page.relations().included() {
        push_relations(&mut lines, relations, page.identity().project());
    }
    if let Some(source) = page.source().included() {
        push_source(&mut lines, source, page.language().fence());
    } else if let Some(reason) = page.source_fault() {
        lines.blank();
        lines.push(fault(reason));
    }
    push_notes(&mut lines, page.notes());
    if let Some(window) = page.section_page() {
        lines.blank();
        lines.push(window.description(page.identity().coordinate().as_str()));
    }
    lines.finish()
}

fn push_notes(lines: &mut Lines, notes: &[Fault]) {
    if notes.is_empty() {
        return;
    }
    lines.blank();
    lines.push("## notes");
    for note in notes {
        lines.push(fault(note));
    }
}

fn meta_line(page: &PageProjection<'_>) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(kind) = page.kind() {
        parts.push(kind.name().to_owned());
    }
    parts.push(page.language().name().to_owned());
    if let Some(tag) = page.identity().key().tag() {
        parts.push(format!("key {tag}"));
    }
    if let Some(site) = page.source_site() {
        parts.push(format!("{}:{}", site.path(), site.line()));
    }
    parts.join(" · ")
}

fn push_prose(lines: &mut Lines, prose: &[Prose]) {
    if prose.is_empty() {
        return;
    }
    lines.blank();
    for block in prose {
        match block {
            Prose::Text(text) => lines.push(text),
            Prose::Code(code) => {
                lines.push("```");
                lines.push(code);
                lines.push("```");
            }
            Prose::Link { label, .. } => lines.push(format!("see {label}")),
        }
    }
}

fn push_members(lines: &mut Lines, groups: &[MemberGroup], within: Option<&ProjectRef>) {
    if groups.is_empty() {
        return;
    }
    lines.blank();
    lines.push("## members");
    for group in groups {
        lines.push(format!(
            "{} {} ({})",
            KindGlyph::new(group.kind()),
            KindGlyph::plural(group.kind()),
            group.members().len()
        ));
        for member in group.members() {
            let mut line = format!("  {}", member.identity().trail_within(within));
            if let Some(signature) = member.signature() {
                let _ = write!(line, " — {}", signature.text());
            }
            lines.push(line);
        }
    }
}

fn push_relations(lines: &mut Lines, groups: &[RelationGroup], within: Option<&ProjectRef>) {
    if groups.is_empty() {
        return;
    }
    lines.blank();
    lines.push("## relations");
    for group in groups {
        lines.push(format!(
            "→ {} ({})",
            group.label().as_str(),
            group.relations().len()
        ));
        for relation in group.relations() {
            lines.push(format!(
                "  {}  `{}`",
                relation.identity().trail_within(within),
                relation.identity().coordinate()
            ));
        }
    }
}

fn push_source(lines: &mut Lines, source: &Source, fence: &str) {
    lines.blank();
    match source {
        Source::Captured {
            lines: source_lines,
            truncation,
            ..
        } => {
            lines.push("## source");
            lines.push(format!("```{fence}"));
            for line in source_lines {
                lines.push(format!("{:>4} {}", line.number().get(), line.text()));
            }
            lines.push("```");
            if *truncation == Truncation::Truncated {
                lines.push("… source continues past the retained bound");
            }
        }
        Source::Sited { fault: reason, .. } | Source::Absent { fault: reason } => {
            lines.push(fault(reason));
        }
    }
}

/// Renders one result page: the coverage line, then two lines per record.
#[must_use]
pub fn records(list: &RecordList, within: Option<&ProjectRef>) -> String {
    records_with_cursor(list, within, None, ContinuationTarget::CliOption)
}

fn records_with_cursor(
    list: &RecordList,
    within: Option<&ProjectRef>,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    let mut lines = Lines::new();
    lines.push(coverage(list.coverage()));
    let cursor_note = if list.has_more() && list.continuation().is_some() {
        cursor.map(|cursor| continuation_note(target, cursor))
    } else {
        None
    };
    if let Some(note) = &cursor_note {
        lines.push(note);
    } else if list.has_more() && list.continuation().is_some() {
        lines.push("… more rows; this rendering does not include the continuation token");
    }
    if list.has_more() && list.continuation().is_none() {
        lines.push("… more rows at this revision; raise `limit` to see them");
    }
    if list.is_empty() {
        lines.push(list.empty_explanation());
        return lines.finish();
    }
    for record in list.records() {
        push_record(&mut lines, record, within);
    }
    lines.finish()
}

fn push_record(lines: &mut Lines, record: &Record, within: Option<&ProjectRef>) {
    let mut head = record.identity().trail_within(within);
    let mut tags = Vec::with_capacity(3);
    if let Some(kind) = record.kind() {
        tags.push(kind.name().to_owned());
    }
    if record.is_external_reference() {
        tags.push("external reference".to_owned());
    }
    if !record.is_external_reference() || record.language() != crate::Language::Unknown {
        tags.push(record.language().name().to_owned());
    }
    if record.state() != crate::record::RecordState::Ready {
        tags.push(record.state().name().to_owned());
    }
    let _ = write!(head, "  ·  {}", tags.join(" · "));
    lines.push(head);
    let mut detail = format!("`{}`", record.identity().coordinate());
    if let Some(signature) = record.signature() {
        let _ = write!(detail, "  {}", signature.text());
    }
    lines.push(detail);
}

/// Renders the shelf: one project per two lines.
#[must_use]
pub fn shelf(shelf: &Shelf) -> String {
    let mut lines = Lines::new();
    if shelf.is_empty() {
        lines.push("no project is on the shelf at this revision");
        lines.push("Call `backend.index` with `arguments.path` set to the repository's absolute path; the path is required.");
        return lines.finish();
    }
    for entry in shelf.entries() {
        let mut head = format!(
            "{} {}  ·  {}",
            entry.readiness().glyph(),
            entry.identity().name(),
            entry.readiness().name()
        );
        for tag in entry.count_tags() {
            let _ = write!(head, " · {tag}");
        }
        for language in entry.languages() {
            let _ = write!(
                head,
                " · {} {}",
                language.language().name(),
                language.declarations().get()
            );
        }
        lines.push(head);
        lines.push(format!("`{}`", entry.identity().coordinate()));
        if let Some(reason) = entry.readiness().fault() {
            lines.push(fault(reason));
        }
    }
    let mut footer = format!(
        "{} project(s) · revision {}",
        shelf.entries().len(),
        shelf.revision()
    );
    let statement = shelf.lane_statement();
    if let Some((words, _)) = &statement {
        let _ = write!(footer, " · {words}");
    }
    lines.push(footer);
    if let Some((_, line)) = statement {
        lines.push(coverage(line));
    }
    lines.finish()
}

/// Renders one outline as an indented tree of names.
#[must_use]
pub fn outline(tree: &OutlineTree) -> String {
    let mut lines = Lines::new();
    lines.push(format!("# {}", tree.package().trail_within(None)));
    lines.push(format!("`{}`", tree.package().coordinate()));
    lines.blank();
    for root in tree.roots() {
        push_outline_entry(&mut lines, root, 0, tree.package().project());
    }
    let mut summary = format!(
        "{} declaration(s) · {}",
        tree.count(),
        tree.truncation().name()
    );
    if tree.unresolved() > 0 {
        let _ = write!(summary, " · {} unnamed", tree.unresolved());
    }
    lines.push(summary);
    if let Some(page) = tree.page().filter(|page| page.continuation.is_some()) {
        lines.push(format!("More declarations remain beyond this bounded prefix. Continue outline with --limit {} and the next cursor.", page.limit));
    }
    lines.finish()
}

fn push_outline_entry(
    lines: &mut Lines,
    entry: &OutlineEntry,
    depth: usize,
    within: Option<&ProjectRef>,
) {
    let indent = "  ".repeat(depth.min(16));
    let glyph = entry.kind().map_or_else(
        || "·".to_owned(),
        |kind| KindGlyph::new(kind).as_str().to_owned(),
    );
    let mut line = format!("{indent}{glyph} {}", entry.name());
    if let Some(identity) = entry.identity() {
        let _ = write!(line, "  `{}`", identity.trail_within(within));
    }
    lines.push(line);
    for child in entry.children() {
        push_outline_entry(lines, child, depth.saturating_add(1), within);
    }
}

/// Renders the engine's whole state in at most a handful of lines.
#[must_use]
pub fn status(status: &Status) -> String {
    let mut lines = Lines::new();
    lines.push(format!(
        "owner {} · publication {} · lanes {} · {} row(s) · revision {} · source {} · sequence {}",
        status.availability(),
        status.publication_state().name(),
        status.readiness(),
        status.rows(),
        status.revision(),
        status.source(),
        status.sequence()
    ));
    if let Some(project) = status.project() {
        lines.push(format!("project `{}`", project.root()));
    }
    lines.push(coverage(status.coverage()));
    lines.push(status.capabilities().render());
    if let Some(cause) = status.compiler_refusal() {
        lines.push("Observed failed compiler attempt (served coverage above is unchanged):");
        lines.push(fault(cause));
    }
    if let Some(observation) = status.source_selection() {
        lines.push(observation.render());
    }
    lines.finish()
}

/// Renders one durable product answer: a heading, then one record per row.
#[must_use]
pub fn product(view: &ProductView) -> String {
    product_projection(view, None, None)
}

/// Renders the same receipt with a copyable MCP action retaining its detail.
#[must_use]
pub fn product_with_detail(view: &ProductView, detail: Detail) -> String {
    product_projection(view, Some(detail), None)
}

fn product_projection(
    view: &ProductView,
    detail: Option<Detail>,
    target: Option<ContinuationTarget<'_>>,
) -> String {
    let mut lines = Lines::new();
    lines.push(format!("# {}", view.heading()));
    if detail == Some(Detail::Summary)
        && matches!(
            view.semantic_data(),
            Some(crate::ProductSemanticData::VersionsPage(_))
        )
    {
        lines.push(
            "For native source references, request --detail full (fresh discovery).".to_owned(),
        );
    }
    if let Some(reason) = view.fault() {
        lines.push(fault(reason));
        if view.records().is_empty() {
            index_job_footer(view, detail, &mut lines);
            return lines.finish();
        }
    }
    if let Some(note) = view.note() {
        lines.push(note);
        if view.records().is_empty() {
            semantic_source_footer(view, &mut lines, target);
            index_search_page_footer(view, &mut lines, target);
            index_job_footer(view, detail, &mut lines);
            return lines.finish();
        }
    }
    if view.records().is_empty() {
        lines.push(if view.is_references_result() {
            "no reference records captured at this revision"
        } else {
            "no row at this revision"
        });
        index_search_page_footer(view, &mut lines, target);
        index_job_footer(view, detail, &mut lines);
        return lines.finish();
    }
    let readable_records = view.readable_records();
    let coverage_continuation = match view.compiler_outcomes() {
        Some(backend_library::PackageCompilerOutcomes::Coverage { page, .. }) => page
            .next
            .as_ref()
            .and_then(|read| serde_json::to_string(&read).ok()),
        _ => None,
    };
    let mut shown_coverage_continuation = false;
    for record in readable_records.as_ref() {
        let mut head = record.title().to_owned();
        if !record.tags().is_empty() {
            let _ = write!(head, "  ·  {}", record.tags().join(" · "));
        }
        lines.push(head);
        if let Some(operand) = record.operand() {
            let repeated_coverage_continuation = coverage_continuation
                .as_deref()
                .is_some_and(|continuation| continuation == operand);
            if !repeated_coverage_continuation || !shown_coverage_continuation {
                lines.push(format!("`{operand}`"));
                shown_coverage_continuation |= repeated_coverage_continuation;
            }
        }
        if let Some(status) = record.history_status() {
            lines.push(semantic_history_details(status));
        }
    }
    lines.push(format!("{} row(s)", view.records().len()));
    if let Some(page) = view.package_graph_page()
        && matches!(
            &page.terminal,
            backend_library::PackageGraphPageTerminal::More(_)
        )
        && let Some(ContinuationTarget::McpInvocation(invocation)) = target
    {
        lines.push(invocation.continuation_note());
    }
    semantic_source_footer(view, &mut lines, target);
    if matches!(target, None | Some(ContinuationTarget::CliOption))
        && let Some(command) =
            view.package_graph_cli_continuation(detail.unwrap_or(Detail::Standard))
    {
        lines.push(format!("Continue: `{command}`"));
    }
    if matches!(target, None | Some(ContinuationTarget::CliOption))
        && let Some(command) = view.semantic_versions_cli_continuation(detail.unwrap_or(Detail::Standard)) {
        lines.push(format!("Continue: `{command}`"));
    }
    index_search_page_footer(view, &mut lines, target);
    index_job_footer(view, detail, &mut lines);
    lines.finish()
}

fn semantic_source_footer(
    view: &ProductView,
    lines: &mut Lines,
    target: Option<ContinuationTarget<'_>>,
) {
    if matches!(view.semantic_data(), Some(crate::ProductSemanticData::Source(chunk)) if chunk.next.is_some())
        && let Some(ContinuationTarget::McpInvocation(invocation)) = target
    {
        lines.push(invocation.continuation_note());
    }
}

fn index_job_footer(view: &ProductView, detail: Option<Detail>, lines: &mut Lines) {
    let poll = match detail {
        Some(detail) => crate::IndexPollDto::for_detail(view, detail),
        None => crate::IndexPollDto::new(view),
    };
    if let Some(crate::IndexPollDto { call: next, .. }) = poll {
        if let Some(job @ IndexJobProjection::Cancellation(_)) = view.index_job()
            && let Some(ticket) = job.ticket_json()
        {
            lines.push(format!(
                "cancellation was requested; cancellation ticket: `{ticket}`"
            ));
        }
        lines.push(format!("Operation in flight; poll with `{next}`"));
        return;
    }
    let Some(job) = view.index_job() else {
        return;
    };
    let guidance = match job {
        // Only the shared policy above may offer a next observation.
        IndexJobProjection::Started(backend_library::IndexStartResult::Started { .. })
        | IndexJobProjection::Progress(backend_library::IndexJobObservation::Pending(_))
        | IndexJobProjection::Cancellation(backend_library::IndexCancelReceipt {
            status: backend_library::IndexCancelStatus::Requested,
            ..
        }) => return,
        IndexJobProjection::Started(backend_library::IndexStartResult::Terminal(_))
        | IndexJobProjection::Terminal(_) => "the owner returned a terminal index receipt",
        IndexJobProjection::Progress(backend_library::IndexJobObservation::Terminal(_)) => {
            "the owner returned a terminal index receipt"
        }
        IndexJobProjection::Progress(backend_library::IndexJobObservation::Unknown {
            ticket,
            current_owner_epoch,
        }) if ticket.owner_epoch() != *current_owner_epoch => {
            "this ticket belongs to a prior owner process and cannot be resumed here"
        }
        IndexJobProjection::Progress(backend_library::IndexJobObservation::Unknown { .. }) => {
            "this ticket is no longer active or retained by the current owner"
        }
        IndexJobProjection::Cancellation(backend_library::IndexCancelReceipt {
            status: backend_library::IndexCancelStatus::Terminal(_),
            ..
        }) => "the owner returned a terminal index receipt",
        IndexJobProjection::Cancellation(backend_library::IndexCancelReceipt {
            status: backend_library::IndexCancelStatus::Unknown,
            ..
        }) => "no active or retained terminal job matched the cancellation ticket",
    };
    lines.push(guidance);
    if let IndexJobProjection::Cancellation(receipt) = job {
        lines.push(format!(
            "cancellation ticket: `{}`",
            serde_json::json!(&receipt.ticket).to_string()
        ));
    }
}

fn index_search_page_footer(
    view: &ProductView,
    lines: &mut Lines,
    reader_target: Option<ContinuationTarget<'_>>,
) {
    let Some(page) = view.index_search_page() else {
        return;
    };
    let snapshot = page
        .snapshot()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    lines.push(format!(
        "snapshot `{snapshot}` · freshness evaluated at {} · {:?}",
        page.evaluated_at_millis(),
        page.result_count()
    ));
    if let Some(cursor) = page.next_cursor() {
        let target = reader_target
            .filter(|target| matches!(target, ContinuationTarget::McpInvocation(_)))
            .unwrap_or_else(|| {
                match page
                    .cursor_projection()
                    .map_or(CursorTarget::CliOption, |projection| projection.target())
                {
                    CursorTarget::CliOption => ContinuationTarget::CliOption,
                    CursorTarget::McpIndexSearchTool => {
                        ContinuationTarget::McpTool("backend.index_search")
                    }
                    CursorTarget::SurfaceCommand => ContinuationTarget::McpSurfaceCommand,
                }
            });
        lines.push(continuation_note(target, cursor));
    }
}
