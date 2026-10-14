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

use super::{ContinuationTarget, Lines, continuation_note, next_page_line};
use crate::coverage::CoverageLine;
use crate::drive::Answer;
use crate::fault::Fault;
use crate::glyph::KindGlyph;
use crate::identity::ProjectRef;
use crate::outline::{OutlineEntry, OutlineTree};
use crate::listing::{handle, member_line};
use crate::page::{MemberGroup, Page, Prose, Source};
use crate::product::{CursorTarget, IndexJobProjection, ProductView, semantic_history_details};
use crate::record::{Record, RecordList};
use crate::shelf::Shelf;
use crate::status::Status;
use crate::{Detail, MAX_PREVIEW_TEXT_BYTES, PageProjection, ProseView, SourceView, bounded_text};

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
        Answer::Page(value) => {
            page_projection_with(&PageProjection::new(value, detail), cursor, target)
        }
        Answer::Records(_) if !matches!(target, ContinuationTarget::CliOption) => {
            summary_with_cursor(value, cursor, target)
        }
        Answer::Records(value) => records_with_cursor(value, None, cursor, target),
        Answer::Product(value) => product_with_cursor(value, detail, cursor, target),
        Answer::Outline(value) => outline_with_cursor(value, cursor, target),
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
        if let Some(header) = coverage_header(list) {
            lines.push(header);
        }
        lines.push(list.empty_explanation());
        if let Some(note) = summary_continuation(list, cursor, target) {
            lines.push(note);
        }
        return bounded_text(&lines.finish());
    }

    // A fully ready result says nothing about its lanes: every response
    // repeating "all lanes complete" is a line an agent pays for and never
    // acts on. A result that is thin, still indexing or partly unavailable
    // says so first.
    let coverage_line = coverage_header(list).unwrap_or_default();
    let total = list.records().len();
    let mut footer = summary_continuation(list, cursor, target);
    // Placeholders without facts are one summary line ahead of any
    // continuation instruction, so the preview parser keeps both intact.
    let externals = list
        .collapsed_externals()
        .map(crate::record::CollapsedExternals::render);
    let with_externals = |continuation: Option<String>| match (&externals, continuation) {
        (Some(line), Some(next)) => Some(format!("{line}\n{next}")),
        (Some(line), None) => Some(line.clone()),
        (None, next) => next,
    };
    footer = with_externals(footer);
    // Reserve the longest possible count line before adding records. Both
    // shown and omitted are at most `total`, so the final line cannot grow
    // beyond this reservation.
    let count_reservation = record_preview_count(total, total, total);
    let heading_bytes = coverage_line
        .len()
        .saturating_add(usize::from(!coverage_line.is_empty()))
        .saturating_add(count_reservation.len())
        .saturating_add(1);
    if footer.as_ref().is_some_and(|line| {
        line.len().saturating_add(1) > MAX_PREVIEW_TEXT_BYTES.saturating_sub(heading_bytes)
    }) {
        footer = with_externals(if list.has_more()
            && (list.continuation().is_some() || list.window().is_some())
        {
            Some("… more rows; the next-page cursor is `structuredContent.nextCursor`".to_owned())
        } else if list.has_more() {
            Some("… more rows at this revision; raise `limit` to see them".to_owned())
        } else {
            None
        });
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
    if !coverage_line.is_empty() {
        preview.push_str(&coverage_line);
        preview.push('\n');
    }
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
    if omitted == 0 {
        return format!("{total} row(s)");
    }
    format!(
        "{total} row(s) · showing {shown}; {omitted} omitted from this text, all rows in `structuredContent.records`"
    )
}

/// The lane statement a result leads with: absent when every lane is ready.
fn coverage_header(list: &RecordList) -> Option<String> {
    (!list.coverage().is_quiet()).then(|| coverage(list.coverage()))
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
        if text.len() > MAX_PREVIEW_TEXT_BYTES {
            return None;
        }
        // The lane statement is present only when a lane is not ready.
        let (header, mut rest) = if text.starts_with('~') {
            let (coverage, rest) = text.split_once('\n')?;
            (&text[..coverage.len() + 1], rest)
        } else {
            ("", text)
        };
        let mut rows = Vec::new();
        while rest.starts_with("• ") {
            // Work and borrowed row indexes are bounded by the shared preview
            // bytes; legacy Related may describe more than one query page.
            let (label, after_label) = rest.split_once('\n')?;
            let label_end = preview_code_span_end(label.strip_prefix("• ")?)? + "• ".len();
            if !label[label_end..].is_empty() && !label[label_end..].starts_with(" · ") {
                return None;
            }
            // A row is its label line and, for a declaration, its exact
            // coordinate on the next line. A placeholder has no coordinate.
            let (consumed, after_row) = match after_label.split_once('\n') {
                Some((coordinate, after)) if coordinate.starts_with("  `") => {
                    let operand = coordinate.strip_prefix("  ")?;
                    if preview_code_span_end(operand)? != operand.len() {
                        return None;
                    }
                    (rest.len() - after.len(), after)
                }
                _ => (rest.len() - after_label.len(), after_label),
            };
            rows.push(&rest[..consumed]);
            rest = after_row;
        }
        let (count, footer) = rest.split_once('\n')?;
        let (total, tail) = count.split_once(" row(s)")?;
        let total: usize = total.parse().ok()?;
        let (shown, omitted) = if tail.is_empty() {
            (total, 0)
        } else {
            let tail = tail.strip_prefix(" · showing ")?;
            let (shown, tail) = tail.split_once("; ")?;
            let omitted = tail.strip_suffix(
                " omitted from this text, all rows in `structuredContent.records`",
            )?;
            (shown.parse().ok()?, omitted.parse().ok()?)
        };
        if shown != rows.len() || usize::checked_add(shown, omitted)? != total {
            return None;
        }
        // The renderer emits either no footer, or an optional placeholder
        // summary line followed by an optional continuation line.
        if !footer.is_empty() {
            let lines = footer.strip_suffix('\n')?;
            let mut lines = lines.split('\n').peekable();
            if lines.peek().is_some_and(|line| line.starts_with("+ ")) {
                lines.next();
            }
            if let Some(line) = lines.next()
                && !(line.starts_with("next page:") || line.starts_with("… "))
            {
                return None;
            }
            if lines.next().is_some() {
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
    if let Some(window) = list.window()
        && window.has_more()
    {
        return Some(next_page_line(
            window.remaining(),
            window.kind().noun(),
            cursor,
            target,
        ));
    }
    if list.has_more() && list.continuation().is_some() {
        Some(cursor.map_or_else(
            || "… more rows; this rendering does not include the continuation token".to_owned(),
            |cursor| format!("… more rows; {}", continuation_note(target, cursor)),
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
    // An external placeholder's coordinate is a content address nobody can
    // read or retype; the name and the word `external` say what there is.
    let external = record.identity().shape() == crate::identity::IdentityShape::External;
    let mut tags = Vec::with_capacity(4);
    if let Some(kind) = record.kind() {
        tags.push(kind.name());
    }
    if record.language() != crate::language::Language::Unknown {
        tags.push(record.language().name());
    }
    if external {
        tags.push("external");
    }
    if record.state() != crate::record::RecordState::Ready {
        tags.push(record.state().name());
    }
    // Check the raw size before allocating the rendered row. The two code
    // spans add their delimiters and optional CommonMark padding below.
    let raw_bytes = name
        .len()
        .saturating_add(if external { 0 } else { coordinate.len() })
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
    let coordinate_ticks = if external {
        0
    } else {
        inline_code_delimiter(coordinate)?
    };

    let name_padding = code_span_needs_padding(name);
    let coordinate_padding = !external && code_span_needs_padding(coordinate);
    let mut row = String::with_capacity(
        raw_bytes
            .saturating_add(name_ticks.saturating_mul(2))
            .saturating_add(coordinate_ticks.saturating_mul(2))
            .saturating_add(if name_padding { 2 } else { 0 })
            .saturating_add(if coordinate_padding { 2 } else { 0 }),
    );
    row.push_str("• ");
    append_code_span(&mut row, name, name_ticks, name_padding);
    for tag in &tags {
        let _ = write!(&mut row, " · {tag}");
    }
    row.push('\n');
    if !external {
        row.push_str("  ");
        append_code_span(&mut row, coordinate, coordinate_ticks, coordinate_padding);
        row.push('\n');
    }
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
    } else if let crate::Affordance::RecoverOwner { .. } = fault.affordance()
        && let Some(shell) = fault.affordance().shell()
    {
        // An indented code block preserves even literal backticks and newlines
        // inside quoted path operands without inventing an MCP scope change.
        let _ = write!(
            out,
            "\n→ Run in a shell:\n\n    {}",
            shell.replace('\n', "\n    ")
        );
    }
    for candidate in fault.candidates() {
        let kind = candidate
            .kind()
            .map_or("declaration", backend_library::DeclarationKind::name);
        let _ = write!(out, "\n- {kind} {}", candidate.location());
        if let Some(call) = candidate.tool_call() {
            let _ = write!(out, " → `{}`", tool_call_text(&call));
        }
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
    page_projection_with(page, None, ContinuationTarget::CliOption)
}

/// Renders a page projection whose member list may continue, naming the exact
/// next call when the caller issued a cursor for it.
#[must_use]
pub fn page_projection_with(
    page: &PageProjection<'_>,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
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
    push_prose(&mut lines, &page.prose_view());
    push_members(&mut lines, page, cursor, target);
    push_relations(&mut lines, page);
    if let Some(view) = page.source_view() {
        push_source(&mut lines, &view, page.language().fence(), cursor, target);
    } else if let Some(source) = page.source().included() {
        push_unavailable_source(&mut lines, source);
    } else if let Some(reason) = page.source_fault() {
        lines.blank();
        lines.push(fault(reason));
    }
    push_notes(&mut lines, page.notes());
    lines.finish()
}

fn push_prose(lines: &mut Lines, prose: &ProseView) {
    if prose.blocks().is_empty() && prose.omitted_bytes() == 0 {
        return;
    }
    lines.blank();
    for block in prose.blocks() {
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
    if prose.omitted_bytes() > 0 {
        lines.push(format!(
            "… {} more byte(s) of documentation; request detail=full for all of it",
            prose.omitted_bytes()
        ));
    }
}

/// The heading of a member list: every member, or the window a response cut.
fn members_heading(page: &PageProjection<'_>) -> String {
    match page.member_window() {
        Some(window) if window.offset() > 0 || window.has_more() => format!(
            "## members {}-{} of {}",
            window.offset() + 1,
            window.offset() + window.shown(),
            window.total()
        ),
        Some(window) => format!("## members ({})", window.total()),
        None => "## members".to_owned(),
    }
}

/// How one group of members is counted: `(30)`, or `(10 of 56)` when the
/// window cut the group.
fn group_count(page: &PageProjection<'_>, group: &MemberGroup) -> String {
    let shown = group.members().len();
    match page.listing().map(|listing| listing.kind_total(group.kind())) {
        Some(total) if total > shown => format!("{shown} of {total}"),
        _ => shown.to_string(),
    }
}

fn push_members(
    lines: &mut Lines,
    page: &PageProjection<'_>,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) {
    let Some(groups) = page.members().included() else {
        return;
    };
    if groups.is_empty() {
        return;
    }
    lines.blank();
    lines.push(members_heading(page));
    for group in groups {
        let heading = format!(
            "{} {} ({})",
            KindGlyph::new(group.kind()),
            KindGlyph::plural(group.kind()),
            group_count(page, group)
        );
        if matches!(page.detail(), Detail::Summary) {
            let names = group
                .members()
                .iter()
                .map(|member| member.identity().name().to_owned())
                .collect::<Vec<_>>();
            lines.push(format!("{heading}: {}", names.join(", ")));
            continue;
        }
        lines.push(heading);
        for member in group.members() {
            lines.push(format!("  {}", member_line(member)));
        }
    }
    if let Some(window) = page.member_window()
        && window.has_more()
    {
        lines.push(next_page_line(
            window.remaining(),
            window.kind().noun(),
            cursor,
            target,
        ));
    }
}

fn push_relations(lines: &mut Lines, page: &PageProjection<'_>) {
    let views = page.relation_views();
    if views.is_empty() {
        return;
    }
    lines.blank();
    lines.push("## relations");
    let mut cut = false;
    for view in &views {
        let mut line = format!("{} ({})", view.label().as_str(), view.total());
        if !view.shown().is_empty() {
            let names = view
                .shown()
                .iter()
                .map(|relation| handle(relation.identity()))
                .collect::<Vec<_>>();
            let _ = write!(line, ": {}", names.join(", "));
        }
        let hidden = view.total().saturating_sub(view.shown().len());
        if hidden > 0 {
            cut = true;
            if !view.shown().is_empty() {
                let _ = write!(line, " … +{hidden} more");
            }
        }
        lines.push(line);
    }
    if cut {
        lines.push("every relation: `related`");
    }
}

fn push_source(
    lines: &mut Lines,
    view: &SourceView<'_>,
    fence: &str,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) {
    lines.blank();
    match view.window().filter(|window| window.is_cut()) {
        Some(window) => lines.push(format!(
            "## source lines {}-{} of {}",
            window.offset() + 1,
            window.offset() + window.shown(),
            window.total()
        )),
        None => lines.push("## source"),
    }
    lines.push(format!("```{fence}"));
    for line in view.lines() {
        lines.push(format!("{:>4} {}", line.number().get(), line.text()));
    }
    lines.push("```");
    if let Some(window) = view.window() {
        if window.has_more() {
            lines.push(next_page_line(
                window.remaining(),
                window.kind().noun(),
                cursor,
                target,
            ));
        }
    } else if view.omitted_lines() > 0 {
        lines.push(format!(
            "… {} more source line(s); read them all with `source`",
            view.omitted_lines()
        ));
    }
    if view.upstream_truncated() && view.window().is_none_or(|window| !window.has_more()) {
        lines.push("… source continues past the retained bound");
    }
}

fn push_unavailable_source(lines: &mut Lines, source: &Source) {
    if let Source::Sited { fault: reason, .. } | Source::Absent { fault: reason } = source {
        lines.blank();
        lines.push(fault(reason));
    }
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
    if let Some(header) = coverage_header(list) {
        lines.push(header);
    }
    if let Some(note) = summary_continuation(list, cursor, target) {
        lines.push(note);
    }
    if list.is_empty() {
        lines.push(list.empty_explanation());
        return lines.finish();
    }
    for record in list.records() {
        push_record(&mut lines, record, within);
    }
    if let Some(externals) = list.collapsed_externals() {
        lines.push(externals.render());
    }
    lines.finish()
}

fn push_record(lines: &mut Lines, record: &Record, within: Option<&ProjectRef>) {
    let mut head = record.identity().trail_within(within);
    let mut tags = Vec::with_capacity(3);
    if let Some(kind) = record.kind() {
        tags.push(kind.name().to_owned());
    }
    if record.language() != crate::language::Language::Unknown {
        tags.push(record.language().name().to_owned());
    }
    if record.identity().shape() == crate::identity::IdentityShape::External {
        tags.push("external".to_owned());
    }
    if record.state() != crate::record::RecordState::Ready {
        tags.push(record.state().name().to_owned());
    }
    if !tags.is_empty() {
        let _ = write!(head, "  ·  {}", tags.join(" · "));
    }
    lines.push(head);
    // An external placeholder's coordinate is a content address; its name and
    // the word `external` are all a reader can use.
    let external = record.identity().shape() == crate::identity::IdentityShape::External;
    let mut detail = if external {
        String::new()
    } else {
        format!("`{}`", record.identity().coordinate())
    };
    if let Some(signature) = record.signature() {
        if !detail.is_empty() {
            detail.push_str("  ");
        }
        detail.push_str(&crate::window::one_line(&signature.text(), 160));
    }
    if !detail.is_empty() {
        lines.push(detail);
    }
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
    outline_with_cursor(tree, None, ContinuationTarget::CliOption)
}

/// Renders one outline whose declarations may continue, naming the exact next
/// call when the caller issued a cursor for it.
#[must_use]
pub fn outline_with_cursor(
    tree: &OutlineTree,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    let mut lines = Lines::new();
    lines.push(format!("# {}", tree.package().trail_within(None)));
    lines.push(format!("`{}`", tree.package().coordinate()));
    lines.blank();
    for root in tree.roots() {
        push_outline_entry(&mut lines, root, 0);
    }
    let mut summary = match tree.window() {
        Some(window) => format!(
            "declarations {}-{} of {}",
            window.offset() + 1,
            window.offset() + window.shown(),
            window.total()
        ),
        None => format!(
            "{} declaration(s) · {}",
            tree.count(),
            tree.truncation().name()
        ),
    };
    if tree.unresolved() > 0 {
        let _ = write!(summary, " · {} unnamed", tree.unresolved());
    }
    lines.push(summary);
    if let Some(window) = tree.window()
        && window.has_more()
    {
        lines.push(next_page_line(
            window.remaining(),
            window.kind().noun(),
            cursor,
            target,
        ));
    }
    lines.finish()
}

fn push_outline_entry(lines: &mut Lines, entry: &OutlineEntry, depth: usize) {
    let indent = "  ".repeat(depth.min(16));
    let glyph = entry.kind().map_or_else(
        || "·".to_owned(),
        |kind| KindGlyph::new(kind).as_str().to_owned(),
    );
    // One name per entry: the handle already ends in it, and it is the
    // spelling every coordinate-taking command accepts back.
    let line = match entry.identity() {
        Some(identity) => format!("{indent}{glyph} {}", handle(identity)),
        None => format!("{indent}{glyph} {}", entry.name()),
    };
    lines.push(line);
    for child in entry.children() {
        push_outline_entry(lines, child, depth.saturating_add(1));
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
    lines.finish()
}

/// Renders one durable product answer: a heading, then one record per row.
#[must_use]
pub fn product(view: &ProductView) -> String {
    product_projection(view, None, None, ContinuationTarget::CliOption)
}

/// Renders the same receipt with a copyable MCP action retaining its detail.
#[must_use]
pub fn product_with_detail(view: &ProductView, detail: Detail) -> String {
    product_projection(view, Some(detail), None, ContinuationTarget::CliOption)
}

/// Renders a product answer whose rows may continue, naming the exact next
/// call when the caller issued a cursor for it.
#[must_use]
pub fn product_with_cursor(
    view: &ProductView,
    detail: Detail,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    product_projection(view, Some(detail), cursor, target)
}

fn product_projection(
    view: &ProductView,
    detail: Option<Detail>,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    let mut lines = Lines::new();
    lines.push(format!("# {}", view.heading()));
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
            index_search_page_footer(view, &mut lines);
            index_job_footer(view, detail, &mut lines);
            return lines.finish();
        }
    }
    if view.records().is_empty() {
        lines.push("no row at this revision");
        index_search_page_footer(view, &mut lines);
        index_job_footer(view, detail, &mut lines);
        return lines.finish();
    }
    for record in view.records() {
        let mut head = record.title().to_owned();
        let tags = record.readable_tags().collect::<Vec<_>>();
        if !tags.is_empty() {
            let _ = write!(head, "  ·  {}", tags.join(" · "));
        }
        lines.push(head);
        if !record.is_compact()
            && let Some(operand) = record.operand()
        {
            lines.push(format!("`{operand}`"));
        }
        if let Some(status) = record.history_status() {
            lines.push(semantic_history_details(status));
        }
    }
    match view.window() {
        Some(window) => {
            lines.push(format!(
                "rows {}-{} of {}",
                window.offset() + 1,
                window.offset() + window.shown(),
                window.total()
            ));
            if window.has_more() {
                lines.push(next_page_line(
                    window.remaining(),
                    window.kind().noun(),
                    cursor,
                    target,
                ));
            }
        }
        None => lines.push(format!("{} row(s)", view.records().len())),
    }
    index_search_page_footer(view, &mut lines);
    index_job_footer(view, detail, &mut lines);
    lines.finish()
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

fn index_search_page_footer(view: &ProductView, lines: &mut Lines) {
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
        let target = match page
            .cursor_projection()
            .map_or(CursorTarget::CliOption, |projection| projection.target())
        {
            CursorTarget::CliOption => ContinuationTarget::CliOption,
            CursorTarget::McpIndexSearchTool => ContinuationTarget::McpTool("backend.index_search"),
            CursorTarget::SurfaceCommand => ContinuationTarget::McpSurfaceCommand,
        };
        lines.push(continuation_note(target, cursor));
    }
}
