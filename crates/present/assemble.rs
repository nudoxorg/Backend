//! Assembly: engine replies in, presentation values out.
//!
//! This module is the keystone of surface parity. Every surface needs the same
//! derivation — a [`Document`] plus the rows around it becomes a [`Page`]; a
//! [`ViewSnapshot`] becomes a [`RecordList`] or a [`Shelf`]; an [`Outline`]
//! plus its flat rows becomes a named [`OutlineTree`]. If each surface did
//! that derivation itself, "the same corpus and basis produce
//! golden-equivalent CLI/MCP/desktop answers" would be a hope rather than a
//! property. Here it is a property: there is one function per derivation, and
//! CLI, MCP, and desktop call it.
//!
//! Every function is pure over already-admitted library values. Nothing here
//! decides *which* requests to issue — that is a surface's business — only
//! what the answers mean once they arrive.

use crate::coverage::CoverageLine;
use crate::fault::Fault;
use crate::glyph::{RelationDirection, RelationLabel, relation_label};
use crate::identity::{Identity, IdentityKey, KeyTag, LineNumber, PackagePath, ProjectRef};
use crate::language::Language;
use crate::outline::{OutlineEntry, OutlineTree, row_resolver};
use crate::page::{
    Member, MemberGroup, Page, Prose, Relation, RelationGroup, Source, SourceSite, Truncation,
};
use crate::record::{Record, RecordList};
use crate::shelf::{LanguageCount, Readiness, RowCount, Shelf, ShelfEntry};
use crate::signature::Signature;
use backend_library::{
    Coverage, Document, GraphRelation, Outline, PageContinuation, Row, RowId, RowState,
    SourceAvailability, SourceExcerpt, SymbolKey, ViewRoot, ViewSnapshot,
};
use std::collections::BTreeMap;

/// Builds one declaration page from a document reply and the rows around it.
///
/// `members` are the rows of the owning package's flat outline; `relations`
/// are the rows of the declaration's graph neighbourhood. Both are advisory:
/// a surface that could not fetch them passes an empty slice and records why
/// in `notes`, so a missing section is explained rather than read as "none".
#[must_use]
pub fn page_from_document(
    coordinate: &str,
    document: &Document,
    members: &[Row],
    relations: &[Row],
    notes: Vec<Fault>,
) -> Page {
    page_from_document_with_graph_relations(coordinate, document, members, relations, &[], notes)
}

/// Builds a declaration page while retaining typed edges selected by the
/// compiler graph authority. The untyped [`page_from_document`] entry point
/// remains the compatibility path for structural/fallback graph replies.
#[must_use]
pub fn page_from_document_with_graph_relations(
    coordinate: &str,
    document: &Document,
    members: &[Row],
    relations: &[Row],
    graph_relations: &[GraphRelation],
    notes: Vec<Fault>,
) -> Page {
    // A semantic coordinate is content-addressed and spells no path, so the
    // page reads its path and line from the document's captured site, as
    // `Record::from_row` does for the same row; otherwise every compiler
    // backed page would render `language: unknown`.
    let captured = document.location.captured();
    let identity = Identity::parse_with_key(coordinate, IdentityKey::Symbol(document.symbol))
        .with_captured_source(
            captured.map(backend_library::SourceLocation::path),
            captured.map(backend_library::SourceLocation::start_line),
        );
    // The page's own row. A structural row is keyed by
    // `symbol_key(coordinate)`, the key the document echoes; a compiler
    // backed row is keyed by its compiler identity instead, so it is found
    // by its exact coordinate. Its key is the one its members name as their
    // parent and its graph edges name as their end.
    let own = members.iter().find(|row| {
        row.id == RowId::Symbol(document.symbol)
            || (matches!(row.id, RowId::Symbol(_)) && row.label == coordinate)
    });
    let centre = match own.map(|row| row.id) {
        Some(RowId::Symbol(symbol)) => symbol,
        _ => document.symbol,
    };
    let language = page_language(&identity, members, centre);
    let source = source_from(&identity, &document.location, &document.excerpt);
    let kind = own.and_then(|row| row.kind);
    let signature = document
        .signature
        .as_ref()
        .map(|text| Signature::tokenize(text, language));
    let page = Page::new(identity, kind, source)
        .with_language(language)
        .with_prose(Prose::from_fragments(&document.fragments))
        .with_members(member_groups(members, centre))
        .with_relations(relation_groups(relations, centre, graph_relations))
        .with_notes(notes);
    match signature {
        Some(value) => page.with_signature(value),
        None => page,
    }
}

fn page_language(identity: &Identity, rows: &[Row], symbol: SymbolKey) -> Language {
    let declared = identity.language();
    if declared != Language::Unknown {
        return declared;
    }
    rows.iter()
        .find(|row| row.id == RowId::Symbol(symbol))
        .map_or(Language::Unknown, |row| {
            Identity::parse(&row.label).language()
        })
}

fn source_from(
    identity: &Identity,
    availability: &SourceAvailability,
    excerpt: &SourceExcerpt,
) -> Source {
    let site = site_from(identity, availability);
    match (site, excerpt) {
        (Some(site), SourceExcerpt::Captured { text, extent }) => Source::Captured {
            lines: Source::number_lines(text, site.line()),
            site,
            truncation: Truncation::from_extent(*extent),
        },
        (Some(site), other) => {
            let operand = crate::fault::Operand::Coordinate(identity.coordinate().clone());
            Fault::excerpt(other, operand).map_or_else(
                || Source::Captured {
                    lines: Box::new([]),
                    site: site.clone(),
                    truncation: Truncation::Complete,
                },
                |fault| Source::Sited {
                    site: site.clone(),
                    fault,
                },
            )
        }
        (None, _) => {
            let operand = crate::fault::Operand::Coordinate(identity.coordinate().clone());
            Source::Absent {
                fault: Fault::source(availability, operand.clone())
                    .or_else(|| Fault::excerpt(excerpt, operand.clone()))
                    .unwrap_or_else(|| {
                        Fault::new(
                            crate::fault::FaultSlug::SourceUnavailable,
                            operand,
                            crate::fault::Cause::new(
                                crate::fault::CauseSlug::NotCaptured,
                                "the producer retained no source span for this declaration",
                            ),
                            crate::fault::Affordance::None,
                        )
                    }),
            }
        }
    }
}

fn site_from(identity: &Identity, availability: &SourceAvailability) -> Option<SourceSite> {
    if let Some(location) = availability.captured() {
        return LineNumber::new(location.start_line())
            .map(|line| SourceSite::new(PackagePath::new(location.path()), line));
    }
    match (identity.path(), identity.line()) {
        (Some(path), Some(line)) => Some(SourceSite::new(path.clone(), line)),
        _ => None,
    }
}

/// One row's identity, with the source site its producer captured when the
/// coordinate itself spells none (the same derivation `Record::from_row`
/// uses, so a member or relation names the path a search result names).
fn row_identity(row: &Row) -> Identity {
    let captured = row.source.captured();
    Identity::parse_with_key(&row.label, row.id.into()).with_captured_source(
        captured.map(backend_library::SourceLocation::path),
        captured.map(backend_library::SourceLocation::start_line),
    )
}

fn member_groups(rows: &[Row], parent: SymbolKey) -> Box<[MemberGroup]> {
    let members = rows
        .iter()
        .filter(|row| row.parent == Some(parent) && row.id != RowId::Symbol(parent))
        .map(|row| {
            let identity = row_identity(row);
            let language = identity.language();
            let member = Member::new(
                identity,
                row.kind,
                row.signature
                    .as_ref()
                    .map(|text| Signature::tokenize(text, language)),
            );
            match summary_of(row) {
                Some(text) => member.with_summary(text),
                None => member,
            }
        })
        .collect::<Vec<_>>();
    MemberGroup::group(members)
}

fn relation_groups(
    rows: &[Row],
    centre: SymbolKey,
    graph_relations: &[GraphRelation],
) -> Box<[RelationGroup]> {
    let mut groups = BTreeMap::<RelationLabel, Vec<Relation>>::new();
    for row in rows.iter().filter(|row| row.id != RowId::Symbol(centre)) {
        let typed = graph_relations.iter().filter_map(|edge| {
            let (direction, matches) = match (edge.from, edge.to) {
                (RowId::Symbol(from), RowId::Symbol(_to))
                    if from == centre && edge.to == row.id =>
                {
                    (RelationDirection::Outgoing, true)
                }
                (RowId::Symbol(_from), RowId::Symbol(to))
                    if to == centre && edge.from == row.id =>
                {
                    (RelationDirection::Incoming, true)
                }
                _ => (RelationDirection::Outgoing, false),
            };
            matches.then_some(relation_label(edge.relation, direction))
        });
        let labels = typed.collect::<Vec<_>>();
        let labels = if labels.is_empty() {
            vec![RelationLabel::Related]
        } else {
            labels
        };
        for label in labels {
            groups
                .entry(label)
                .or_default()
                .push(Relation::new(row_identity(row), row.kind));
        }
    }
    if groups.is_empty() {
        return Box::new([]);
    }
    groups
        .into_iter()
        .map(|(label, relations)| RelationGroup::new(label, relations.into_boxed_slice()))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

/// Builds one result page from a bounded snapshot.
#[must_use]
pub fn record_list(query: &str, snapshot: &ViewSnapshot) -> RecordList {
    record_list_from_rows(
        query,
        snapshot.root.rows(),
        snapshot.root.coverage(),
        snapshot.next.is_some(),
    )
    .with_continuation(snapshot.next.map(PageContinuation::from_cursor))
}

/// Builds one result page from rows a surface already holds.
///
/// The desktop reads rows and coverage out of a reply without retaining the
/// snapshot they arrived in; it calls this so its result rows are derived by
/// the same function the CLI and MCP surfaces use rather than by a second
/// projection that could drift from it.
#[must_use]
pub fn record_list_from_rows(
    query: &str,
    rows: &[Row],
    coverage: &[Coverage],
    more: bool,
) -> RecordList {
    let line = CoverageLine::new(coverage, u64::try_from(rows.len()).ok());
    let records = rows
        .iter()
        .map(|row| {
            let record = Record::from_row(row);
            match summary_of(row) {
                Some(text) => record.with_summary(text),
                None => record,
            }
        })
        .collect::<Vec<_>>();
    RecordList::new(query, line, records).with_more(more)
}

fn summary_of(row: &Row) -> Option<String> {
    let text = row
        .document
        .iter()
        .find_map(|fragment| match fragment {
            backend_library::Fragment::Text(text) => Some(text.clone()),
            _ => None,
        })?;
    let first = text.lines().next().unwrap_or_default().trim().to_owned();
    (!first.is_empty() && first != row.label).then_some(first)
}

/// Builds the shelf from one packages snapshot.
#[must_use]
pub fn shelf_from_snapshot(snapshot: &ViewSnapshot, revision: KeyTag) -> Shelf {
    shelf_from_root(&snapshot.root, revision)
}

/// Builds the shelf from one immutable view root.
///
/// A project reaches the shelf two ways. Normally the owner publishes a
/// package row for it, and that row carries the readiness the engine actually
/// committed. But a root can also carry declarations whose project has no
/// package row yet — a freshly indexed folder whose package row has not landed
/// on this revision. Dropping those would make a window that holds thousands
/// of readable declarations render an empty shelf, so they are synthesised
/// here from the declarations themselves and marked ready, which is exactly
/// what they are: readable, with a known count.
#[must_use]
pub fn shelf_from_root(root: &ViewRoot, revision: KeyTag) -> Shelf {
    let rows = root.rows();
    let counts = language_counts(rows);
    let mut entries = rows
        .iter()
        .filter(|row| matches!(row.id, RowId::Package(_)))
        .map(|row| shelf_entry(row, &counts))
        .collect::<Vec<_>>();
    synthesise_loose(&mut entries, &counts);
    entries.sort_by(|left, right| left.identity().name().cmp(right.identity().name()));
    Shelf::new(revision, entries)
}

fn language_counts(rows: &[Row]) -> BTreeMap<String, BTreeMap<Language, u64>> {
    let mut counts: BTreeMap<String, BTreeMap<Language, u64>> = BTreeMap::new();
    for row in rows {
        let identity = Identity::parse(&row.label);
        let Some(project) = identity.project() else {
            continue;
        };
        if matches!(row.id, RowId::Symbol(_)) && identity.path().is_some() {
            *counts
                .entry(project.root().to_owned())
                .or_default()
                .entry(identity.language())
                .or_default() += 1;
        }
    }
    counts
}

fn synthesise_loose(
    entries: &mut Vec<ShelfEntry>,
    counts: &BTreeMap<String, BTreeMap<Language, u64>>,
) {
    for (root, per_language) in counts {
        let already = entries.iter().any(|entry| {
            entry
                .identity()
                .project()
                .is_some_and(|project| project.root() == root)
        });
        if already {
            continue;
        }
        let languages = per_language
            .iter()
            .map(|(language, count)| LanguageCount::new(*language, *count))
            .collect::<Vec<_>>();
        entries.push(
            ShelfEntry::new(Identity::parse(root), Readiness::Ready).with_languages(languages),
        );
    }
}

fn shelf_entry(row: &Row, counts: &BTreeMap<String, BTreeMap<Language, u64>>) -> ShelfEntry {
    let identity = Identity::parse_with_key(&row.label, row.id.into());
    let languages = identity
        .project()
        .and_then(|project| counts.get(project.root()))
        .map(|per_language| {
            per_language
                .iter()
                .map(|(language, count)| LanguageCount::new(*language, *count))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let published = languages
        .iter()
        .fold(0_u64, |total, count| {
            total.saturating_add(count.declarations().get())
        });
    ShelfEntry::new(identity, readiness_of(row, published)).with_languages(languages)
}

/// Reads one project's readiness from the row the owner published.
///
/// `published` is how many declarations of this project the *same* reply
/// carried, and it is used for the language tags and nothing else. It must not
/// decide readiness: the shelf reply carries package rows and no declarations,
/// so a count of zero means "this reply did not say", not "nothing is indexed".
/// Reading it as the latter is how a project with eighteen readable
/// declarations came to render as `○ polyglot requested` next to a `health`
/// line that said `ready · 18 row(s)`. The owner's own [`RowState`] is the
/// authority, and it is the only thing consulted here.
fn readiness_of(row: &Row, published: u64) -> Readiness {
    match row.state {
        RowState::Ready => Readiness::Ready,
        RowState::Loading => Readiness::Indexing {
            rows: RowCount::new(published),
        },
        RowState::Failed => Readiness::Failed {
            fault: Fault::new(
                crate::fault::FaultSlug::Rejected,
                crate::fault::Operand::Coordinate(crate::identity::Coordinate::new(&row.label)),
                crate::fault::Cause::new(
                    crate::fault::CauseSlug::Refused,
                    "the owner published this project row in a failed state",
                ),
                crate::fault::Affordance::Reindex {
                    path: row.label.clone(),
                },
            ),
        },
    }
}

/// Builds one named outline tree from an outline reply and its flat rows.
#[must_use]
pub fn outline_tree(package_label: &str, outline: &Outline, rows: &[Row]) -> OutlineTree {
    let mut resolver = row_resolver(rows);
    let roots = outline
        .roots()
        .map(|root| OutlineEntry::resolve(root, &mut resolver))
        .collect::<Vec<_>>();
    OutlineTree::new(
        Identity::parse_with_key(package_label, IdentityKey::Package(outline.package)),
        roots,
        outline.extent,
    )
}

/// Returns the project a coordinate belongs to, for relative spellings.
#[must_use]
pub fn project_of(coordinate: &str) -> Option<ProjectRef> {
    Identity::parse(coordinate).project().cloned()
}
