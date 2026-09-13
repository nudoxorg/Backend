//! The terminal rendering of the presentation model.
//!
//! Identity first, always. A record's trail is one line and the exact
//! coordinate is the next, on its own, never wrapped and never clipped — a
//! reader copies it by selecting a line, not by hunting inside a sentence.
//! Everything the reader does not have to read is dim; everything that failed
//! is a [`crate::Fault`] in the shared three-line grammar.

use core::fmt::Write as _;

use super::{Lines, Style, Theme};
use crate::coverage::CoverageLine;
use crate::fault::Fault;
use crate::glyph::{KindGlyph, LanguageGlyph};
use crate::identity::{Identity, ProjectRef};
use crate::outline::{OutlineEntry, OutlineTree};
use crate::page::{MemberGroup, Page, Prose, RelationGroup, Source, Truncation};
use crate::product::ProductView;
use crate::record::{Record, RecordList};
use crate::shelf::{Readiness, Shelf, ShelfEntry};
use crate::signature::{Signature, TokenKind};
use crate::status::Status;

/// Renders one identity trail, with the project dropped when it is `within`.
#[must_use]
pub fn trail(identity: &Identity, within: Option<&ProjectRef>, theme: Theme) -> String {
    let rendered = identity.trail_within(within);
    let name = identity.name();
    if name.is_empty() || !theme.is_coloured() {
        return rendered;
    }
    match rendered.rsplit_once(crate::identity::TRAIL) {
        Some((head, leaf)) => format!(
            "{}{}{}",
            theme.paint(Style::Project, head),
            theme.paint(Style::Dim, crate::identity::TRAIL),
            theme.paint(Style::Name, leaf)
        ),
        None => theme.paint(Style::Name, &rendered),
    }
}

/// Renders one tokenized signature with each token painted by its kind.
#[must_use]
pub fn signature(signature: &Signature, theme: Theme) -> String {
    signature
        .tokens()
        .iter()
        .map(|token| {
            let style = match token.kind() {
                TokenKind::Keyword => Style::Keyword,
                TokenKind::Type => Style::Type,
                TokenKind::Name => Style::Name,
                TokenKind::Binding => Style::Binding,
                TokenKind::Lifetime => Style::Lifetime,
                TokenKind::Literal => Style::Literal,
                TokenKind::Punctuation => Style::Punctuation,
                TokenKind::Text => return token.text().to_owned(),
            };
            theme.paint(style, token.text())
        })
        .collect()
}

/// Renders one `~lanes` coverage statement.
#[must_use]
pub fn coverage(line: CoverageLine, theme: Theme) -> String {
    if !theme.is_coloured() {
        return line.render();
    }
    let rows = line.rows().map(crate::coverage::RowCount::get);
    let mut out = theme.paint(Style::Dim, "~lanes");
    for lane in line.lanes() {
        let mark = lane.mark(rows);
        let style = if lane.is_unavailable() {
            Style::Fault
        } else {
            Style::Ready
        };
        let _ = write!(
            out,
            " {}{}",
            theme.paint(Style::Dim, lane.name()),
            theme.paint(style, &mark)
        );
    }
    out
}

/// Renders one fault in the shared three-line grammar.
#[must_use]
pub fn fault(fault: &Fault, theme: Theme) -> String {
    let operand = fault.operand().render();
    let mut out = format!(
        "{} {}",
        theme.paint(Style::Fault, "✗"),
        theme.paint(Style::Fault, fault.slug().as_str())
    );
    if !operand.is_empty() {
        out.push(' ');
        out.push_str(&theme.paint(Style::Coordinate, &operand));
    }
    let _ = write!(out, "\n  {}", fault.cause().sentence());
    if let Some(affordance) = fault.affordance().shell() {
        let _ = write!(
            out,
            "\n  {} {}",
            theme.paint(Style::Dim, "→"),
            theme.paint(Style::Name, &affordance)
        );
    }
    out
}

/// Renders one result page: the coverage line, then two lines per record.
#[must_use]
pub fn records(list: &RecordList, within: Option<&ProjectRef>, theme: Theme) -> String {
    let mut lines = Lines::new();
    lines.push(coverage(list.coverage(), theme));
    if list.is_empty() {
        lines.push(empty_sentence(list));
        return lines.finish();
    }
    for record in list.records() {
        push_record(&mut lines, record, within, theme);
    }
    if list.has_more() {
        lines.push(theme.paint(
            Style::Dim,
            "… more rows at this revision; raise --limit or page with the continuation",
        ));
    }
    lines.finish()
}

fn empty_sentence(list: &RecordList) -> String {
    let coverage = list.coverage();
    if coverage.has_unavailable() {
        "no rows, and at least one lane answered nothing — read the marks above".to_owned()
    } else if coverage.readiness() == "indexing" {
        "no rows yet; indexing has not finished for this revision".to_owned()
    } else {
        format!("no declaration matches {:?} at this revision", list.query())
    }
}

fn push_record(lines: &mut Lines, record: &Record, within: Option<&ProjectRef>, theme: Theme) {
    let glyph = record.state().glyph();
    let mut head = format!(
        "{} {}",
        theme.paint(state_style(record), glyph),
        trail(record.identity(), within, theme)
    );
    let mut tags = Vec::with_capacity(3);
    if let Some(kind) = record.kind() {
        tags.push(format!("{} {}", KindGlyph::new(kind), kind.name()));
    }
    tags.push(LanguageGlyph::new(record.language()).as_str().to_owned());
    if record.state() != crate::record::RecordState::Ready {
        tags.push(record.state().name().to_owned());
    }
    let _ = write!(head, "  {}", theme.paint(Style::Dim, &tags.join(" · ")));
    lines.push(head);
    lines.push(format!(
        "  {}",
        theme.paint(Style::Coordinate, record.identity().coordinate().as_str())
    ));
    if let Some(value) = record.signature() {
        lines.push(format!("  {}", signature(value, theme)));
    }
    if let Some(summary) = record.summary() {
        let budget = theme.width().get().saturating_sub(4);
        lines.push(format!(
            "  {}",
            theme.paint(Style::Dim, &theme.clip(summary, budget))
        ));
    }
}

const fn state_style(record: &Record) -> Style {
    match record.state() {
        crate::record::RecordState::Ready => Style::Ready,
        crate::record::RecordState::Loading => Style::Working,
        crate::record::RecordState::Failed => Style::Fault,
    }
}

/// Renders one declaration page.
#[must_use]
pub fn page(page: &Page, theme: Theme) -> String {
    let mut lines = Lines::new();
    lines.push(trail(page.identity(), None, theme));
    lines.push(format!(
        "  {}",
        theme.paint(Style::Coordinate, page.identity().coordinate().as_str())
    ));
    lines.push(format!("  {}", theme.paint(Style::Dim, &meta_line(page))));
    if let Some(value) = page.signature() {
        lines.blank();
        lines.push(format!("  {}", signature(value, theme)));
    }
    push_prose(&mut lines, page.prose(), theme);
    push_members(&mut lines, page.members(), page.identity().project(), theme);
    push_relations(&mut lines, page.relations(), page.identity().project(), theme);
    push_source(&mut lines, page.source(), theme);
    push_notes(&mut lines, page.notes(), theme);
    lines.finish()
}

fn push_notes(lines: &mut Lines, notes: &[Fault], theme: Theme) {
    if notes.is_empty() {
        return;
    }
    lines.blank();
    for note in notes {
        lines.push(fault(note, theme));
    }
}

fn meta_line(page: &Page) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(kind) = page.kind() {
        parts.push(format!("{} {}", KindGlyph::new(kind), kind.name()));
    }
    parts.push(page.language().name().to_owned());
    if let Some(tag) = page.identity().key().tag() {
        parts.push(format!("key {tag}"));
    }
    if let Some(site) = page.source().site() {
        parts.push(format!("{}:{}", site.path(), site.line()));
    }
    parts.join(" · ")
}

fn push_prose(lines: &mut Lines, prose: &[Prose], theme: Theme) {
    if prose.is_empty() {
        return;
    }
    lines.blank();
    for block in prose {
        match block {
            Prose::Text(text) => lines.push(format!("  {text}")),
            Prose::Code(code) => {
                for line in code.lines() {
                    lines.push(format!("    {line}"));
                }
            }
            Prose::Link { label, .. } => {
                lines.push(format!("  {}", theme.paint(Style::Type, label)));
            }
        }
    }
}

fn push_members(
    lines: &mut Lines,
    groups: &[MemberGroup],
    within: Option<&ProjectRef>,
    theme: Theme,
) {
    if groups.is_empty() {
        return;
    }
    lines.blank();
    for group in groups {
        lines.push(theme.paint(
            Style::Heading,
            &format!(
                "  {} {} ({})",
                KindGlyph::new(group.kind()),
                KindGlyph::plural(group.kind()),
                group.members().len()
            ),
        ));
        for member in group.members() {
            let mut line = format!("    {}", trail(member.identity(), within, theme));
            if let Some(value) = member.signature() {
                let budget = theme.width().get().saturating_sub(8);
                let _ = write!(
                    line,
                    "  {}",
                    theme.paint(Style::Dim, &theme.clip(&value.text(), budget))
                );
            }
            lines.push(line);
        }
    }
}

fn push_relations(
    lines: &mut Lines,
    groups: &[RelationGroup],
    within: Option<&ProjectRef>,
    theme: Theme,
) {
    if groups.is_empty() {
        return;
    }
    lines.blank();
    for group in groups {
        lines.push(format!(
            "  {} {} ({})",
            theme.paint(Style::Dim, "→"),
            theme.paint(Style::Heading, group.label().as_str()),
            group.relations().len()
        ));
        for relation in group.relations() {
            lines.push(format!(
                "    {}",
                trail(relation.identity(), within, theme)
            ));
            lines.push(format!(
                "      {}",
                theme.paint(Style::Coordinate, relation.identity().coordinate().as_str())
            ));
        }
    }
}

fn push_source(lines: &mut Lines, source: &Source, theme: Theme) {
    lines.blank();
    match source {
        Source::Captured {
            lines: source_lines,
            truncation,
            ..
        } => {
            let gutter = source_lines
                .last()
                .map_or(1, |line| line.number().get().to_string().len());
            for line in source_lines {
                lines.push(format!(
                    "  {} {}",
                    theme.paint(
                        Style::Dim,
                        &format!("{:>gutter$}", line.number().get(), gutter = gutter)
                    ),
                    line.text()
                ));
            }
            if *truncation == Truncation::Truncated {
                lines.push(theme.paint(Style::Dim, "  … source continues past the retained bound"));
            }
        }
        Source::Sited { fault: reason, .. } | Source::Absent { fault: reason } => {
            lines.push(fault(reason, theme));
        }
    }
}

/// Renders the shelf: one project per row, readiness first.
#[must_use]
pub fn shelf(shelf: &Shelf, theme: Theme) -> String {
    let mut lines = Lines::new();
    if shelf.is_empty() {
        lines.push("no project is on the shelf at this revision");
        lines.push(theme.paint(Style::Dim, "  → backend index <PATH>"));
        return lines.finish();
    }
    for entry in shelf.entries() {
        push_shelf_entry(&mut lines, entry, theme);
    }
    lines.push(theme.paint(
        Style::Dim,
        &format!(
            "{} project(s) · revision {}",
            shelf.entries().len(),
            shelf.revision()
        ),
    ));
    lines.finish()
}

fn push_shelf_entry(lines: &mut Lines, entry: &ShelfEntry, theme: Theme) {
    let style = match entry.readiness() {
        Readiness::Ready => Style::Ready,
        Readiness::Indexing { .. } | Readiness::Requested => Style::Working,
        Readiness::Failed { .. } => Style::Fault,
    };
    let mut head = format!(
        "{} {}",
        theme.paint(style, entry.readiness().glyph()),
        theme.paint(Style::Name, entry.identity().name())
    );
    let mut tags = vec![entry.readiness().name().to_owned()];
    if let Readiness::Indexing { rows } = entry.readiness() {
        tags.push(format!("{} row(s)", rows.get()));
    }
    if entry.declarations().get() > 0 {
        tags.push(format!("{} declaration(s)", entry.declarations().get()));
    }
    for language in entry.languages() {
        tags.push(format!(
            "{} {}",
            LanguageGlyph::new(language.language()),
            language.declarations().get()
        ));
    }
    let _ = write!(head, "  {}", theme.paint(Style::Dim, &tags.join(" · ")));
    lines.push(head);
    lines.push(format!(
        "  {}",
        theme.paint(Style::Coordinate, entry.identity().coordinate().as_str())
    ));
    if let Some(reason) = entry.readiness().fault() {
        lines.push(fault(reason, theme));
    }
}

/// Renders one outline as an indented tree of names.
#[must_use]
pub fn outline(tree: &OutlineTree, theme: Theme) -> String {
    let mut lines = Lines::new();
    lines.push(trail(tree.package(), None, theme));
    lines.push(format!(
        "  {}",
        theme.paint(Style::Coordinate, tree.package().coordinate().as_str())
    ));
    lines.blank();
    for root in tree.roots() {
        push_outline_entry(&mut lines, root, 0, theme);
    }
    let mut summary = format!("{} declaration(s) · {}", tree.count(), tree.truncation().name());
    if tree.unresolved() > 0 {
        let _ = write!(summary, " · {} unnamed", tree.unresolved());
    }
    lines.push(theme.paint(Style::Dim, &summary));
    lines.finish()
}

fn push_outline_entry(lines: &mut Lines, entry: &OutlineEntry, depth: usize, theme: Theme) {
    let indent = "  ".repeat(depth.min(16));
    let glyph = entry
        .kind()
        .map_or_else(|| "·".to_owned(), |kind| KindGlyph::new(kind).as_str().to_owned());
    let name = entry.name();
    let painted = if entry.identity().is_some() {
        theme.paint(Style::Name, &name)
    } else {
        theme.paint(Style::Dim, &name)
    };
    let mut line = format!("{indent}{} {painted}", theme.paint(Style::Dim, &glyph));
    if let Some(identity) = entry.identity()
        && let Some(number) = identity.line()
    {
        let _ = write!(line, "  {}", theme.paint(Style::Dim, &format!(":{number}")));
    }
    lines.push(line);
    for child in entry.children() {
        push_outline_entry(lines, child, depth.saturating_add(1), theme);
    }
}

/// Renders the engine's whole state in a handful of lines.
#[must_use]
pub fn status(status: &Status, theme: Theme) -> String {
    let mut lines = Lines::new();
    let readiness = status.readiness();
    let style = match readiness {
        "ready" => Style::Ready,
        "unavailable" => Style::Fault,
        _ => Style::Working,
    };
    lines.push(format!(
        "{} {}",
        theme.paint(style, readiness),
        theme.paint(
            Style::Dim,
            &format!(
                "· {} row(s) · revision {} · source {} · sequence {}",
                status.rows(),
                status.revision(),
                status.source(),
                status.sequence()
            )
        )
    ));
    if let Some(project) = status.project() {
        lines.push(format!(
            "{} {}",
            theme.paint(Style::Dim, "project"),
            theme.paint(Style::Coordinate, project.root())
        ));
    }
    lines.push(coverage(status.coverage(), theme));
    lines.push(theme.paint(Style::Dim, &status.capabilities().render()));
    lines.finish()
}

/// Renders one durable product answer: a heading, then one record per row.
#[must_use]
pub fn product(view: &ProductView, theme: Theme) -> String {
    let mut lines = Lines::new();
    lines.push(theme.paint(Style::Heading, view.heading()));
    if let Some(reason) = view.fault() {
        lines.push(fault(reason, theme));
        return lines.finish();
    }
    if let Some(note) = view.note() {
        lines.push(format!("  {note}"));
        return lines.finish();
    }
    if view.records().is_empty() {
        lines.push(theme.paint(Style::Dim, "  no row at this revision"));
        return lines.finish();
    }
    for record in view.records() {
        let mut head = format!("{} {}", theme.paint(Style::Ready, "●"), theme.paint(Style::Name, record.title()));
        if !record.tags().is_empty() {
            let _ = write!(head, "  {}", theme.paint(Style::Dim, &record.tags().join(" · ")));
        }
        lines.push(head);
        if let Some(operand) = record.operand() {
            lines.push(format!("  {}", theme.paint(Style::Coordinate, operand)));
        }
    }
    lines.push(theme.paint(Style::Dim, &format!("{} row(s)", view.records().len())));
    lines.finish()
}
