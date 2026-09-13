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

use super::Lines;
use crate::coverage::CoverageLine;
use crate::fault::Fault;
use crate::glyph::KindGlyph;
use crate::identity::ProjectRef;
use crate::outline::{OutlineEntry, OutlineTree};
use crate::page::{MemberGroup, Page, Prose, RelationGroup, Source, Truncation};
use crate::product::ProductView;
use crate::record::{Record, RecordList};
use crate::shelf::{Readiness, Shelf};
use crate::status::Status;

/// Renders one `~lanes` coverage statement.
#[must_use]
pub fn coverage(line: CoverageLine) -> String {
    line.render()
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
        let _ = write!(out, "\n→ `{call}`");
    }
    out
}

/// Renders one declaration page, identity first.
#[must_use]
pub fn page(page: &Page) -> String {
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
    push_prose(&mut lines, page.prose());
    push_members(&mut lines, page.members(), page.identity().project());
    push_relations(&mut lines, page.relations(), page.identity().project());
    push_source(&mut lines, page.source(), page.language().fence());
    push_notes(&mut lines, page.notes());
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

fn meta_line(page: &Page) -> String {
    let mut parts = Vec::with_capacity(4);
    if let Some(kind) = page.kind() {
        parts.push(kind.name().to_owned());
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
    let mut lines = Lines::new();
    lines.push(coverage(list.coverage()));
    if list.is_empty() {
        lines.push(empty_sentence(list));
        return lines.finish();
    }
    for record in list.records() {
        push_record(&mut lines, record, within);
    }
    if list.has_more() {
        lines.push("… more rows at this revision; raise `limit` to see them");
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

fn push_record(lines: &mut Lines, record: &Record, within: Option<&ProjectRef>) {
    let mut head = record.identity().trail_within(within);
    let mut tags = Vec::with_capacity(3);
    if let Some(kind) = record.kind() {
        tags.push(kind.name().to_owned());
    }
    tags.push(record.language().name().to_owned());
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
        lines.push("→ `{\"name\":\"backend.index\",\"arguments\":{}}`");
        return lines.finish();
    }
    for entry in shelf.entries() {
        let mut head = format!(
            "{} {}  ·  {}",
            entry.readiness().glyph(),
            entry.identity().name(),
            entry.readiness().name()
        );
        if let Readiness::Indexing { rows } = entry.readiness() {
            let _ = write!(head, " · {} row(s)", rows.get());
        }
        if entry.declarations().get() > 0 {
            let _ = write!(head, " · {} declaration(s)", entry.declarations().get());
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
    lines.push(format!(
        "{} project(s) · revision {}",
        shelf.entries().len(),
        shelf.revision()
    ));
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
    lines.finish()
}

fn push_outline_entry(
    lines: &mut Lines,
    entry: &OutlineEntry,
    depth: usize,
    within: Option<&ProjectRef>,
) {
    let indent = "  ".repeat(depth.min(16));
    let glyph = entry
        .kind()
        .map_or_else(|| "·".to_owned(), |kind| KindGlyph::new(kind).as_str().to_owned());
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
        "{} · {} row(s) · revision {} · source {} · sequence {}",
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
    let mut lines = Lines::new();
    lines.push(format!("# {}", view.heading()));
    if let Some(reason) = view.fault() {
        lines.push(fault(reason));
        return lines.finish();
    }
    if let Some(note) = view.note() {
        lines.push(note);
        return lines.finish();
    }
    if view.records().is_empty() {
        lines.push("no row at this revision");
        return lines.finish();
    }
    for record in view.records() {
        let mut head = record.title().to_owned();
        if !record.tags().is_empty() {
            let _ = write!(head, "  ·  {}", record.tags().join(" · "));
        }
        lines.push(head);
        if let Some(operand) = record.operand() {
            lines.push(format!("`{operand}`"));
        }
    }
    lines.push(format!("{} row(s)", view.records().len()));
    lines.finish()
}
