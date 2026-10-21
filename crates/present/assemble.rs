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
use crate::project_coverage::ProjectCoverageEvidence;
use crate::readiness::{PublishedFacts, gapped_lanes, project_readiness};
use crate::record::{Record, RecordList};
use crate::shelf::{LanguageCount, Readiness, Shelf, ShelfEntry};
use crate::signature::Signature;
use backend_library::{
    Coverage, Document, GraphRelation, HealthReport, Outline, PageContinuation, Row, RowId,
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
    // The admitted document carries the actual selected identity for both
    // structural and compiler-backed rows. A repeated coordinate cannot
    // replace that identity or select another row's members and graph edges.
    let centre = document.symbol;
    let own = members.iter().find(|row| row.id == RowId::Symbol(centre));
    let language = page_language(&identity, members, centre);
    let source = source_from(&identity, &document.location, &document.excerpt);
    let kind = own.and_then(|row| row.kind);
    let signature = document
        .signature
        .as_ref()
        .and_then(|text| {
            Signature::from_declaration(
                text,
                language,
                kind,
                identity.name(),
                &document.excerpt,
            )
        });
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
                    .and_then(|text| {
                        Signature::from_declaration(
                            text,
                            language,
                            row.kind,
                            identity.name(),
                            &row.excerpt,
                        )
                    }),
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
    .with_page_basis(snapshot.root.basis(), snapshot.root.frontier().sequence)
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
    let mut records = rows
        .iter()
        .map(|row| {
            let record = Record::from_row(row);
            match summary_of(row) {
                Some(text) => record.with_summary(text),
                None => record,
            }
        })
        .collect::<Vec<_>>();
    // View rows are ordered by canonical identity for hashing and paging.
    // Scored search rows carry the owner's absolute rank independently of
    // that storage order. Restore it only in the readable projection; a
    // stable sort leaves unscored rows and equal scores in their input order.
    records.sort_by_key(|record| std::cmp::Reverse(record.score().map(crate::record::Score::get)));
    RecordList::new(query, line, records).with_more(more)
}

fn summary_of(row: &Row) -> Option<String> {
    let text = row.document.iter().find_map(|fragment| match fragment {
        backend_library::Fragment::Text(text) => Some(text.clone()),
        _ => None,
    })?;
    let first = text.lines().next().unwrap_or_default().trim().to_owned();
    (!first.is_empty() && first != row.label).then_some(first)
}

/// Builds the shelf from one packages snapshot.
///
/// The snapshot carries the lane coverage of the revision it was read at, so
/// the shelf prints the same lane statement `status` does. It does not carry
/// the published row count; use [`shelf_from_snapshot_with_health`] when a
/// health report of the same revision is available.
#[must_use]
pub fn shelf_from_snapshot(snapshot: &ViewSnapshot, revision: KeyTag) -> Shelf {
    let facts = PublishedFacts::new(snapshot.root.coverage(), None);
    shelf_from_root_with_facts(&snapshot.root, revision, facts)
}

/// Builds the shelf from one packages snapshot and the health report read at
/// the same revision.
///
/// The report supplies the published row count and the lane coverage, so
/// `packages` and `status` read the very same facts: a project that is still
/// indexing is described with the count `status` prints rather than with a
/// count invented from a reply that carried only package rows. A packages
/// snapshot is stamped with the visible view's root, exactly as the report's
/// revision receipt is, so a report whose revision differs describes another
/// revision and is ignored: the lanes then come from the snapshot itself and the count is
/// shown as unread, never as zero.
#[must_use]
pub fn shelf_from_snapshot_with_health(
    snapshot: &ViewSnapshot,
    revision: KeyTag,
    health: &HealthReport,
) -> Shelf {
    let facts = if health.revision().root() == snapshot.root.basis().root {
        PublishedFacts::from_health(health)
    } else {
        PublishedFacts::new(snapshot.root.coverage(), None)
    };
    shelf_from_root_with_facts(&snapshot.root, revision, facts)
}

/// Builds the shelf with package-bound semantic coverage from current owner
/// profile receipts. A receipt is used only for the exact view revision and
/// package row it was read against; all other lanes remain workspace-only.
pub(crate) fn shelf_from_snapshot_with_project_coverage(
    snapshot: &ViewSnapshot,
    revision: KeyTag,
    health: &HealthReport,
    project_coverage: &BTreeMap<backend_library::PackageKey, ProjectCoverageEvidence>,
) -> Shelf {
    let same_revision = health.revision().root() == snapshot.root.basis().root;
    let facts = if same_revision {
        PublishedFacts::from_health(health)
    } else {
        PublishedFacts::new(snapshot.root.coverage(), None)
    };
    let view_root = same_revision.then(|| health.revision().root());
    let source_root = same_revision.then(|| *health.basis().root.as_bytes());
    shelf_from_root_with_project_coverage(
        &snapshot.root,
        revision,
        facts,
        view_root,
        source_root,
        project_coverage,
    )
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
    shelf_from_root_with_facts(root, revision, PublishedFacts::unread())
}

/// Builds the shelf from one immutable view root and what the owner published
/// at the same revision.
#[must_use]
pub fn shelf_from_root_with_facts(
    root: &ViewRoot,
    revision: KeyTag,
    facts: PublishedFacts,
) -> Shelf {
    shelf_from_root_with_project_coverage(root, revision, facts, None, None, &BTreeMap::new())
}

pub(crate) fn shelf_from_root_with_project_coverage(
    root: &ViewRoot,
    revision: KeyTag,
    facts: PublishedFacts,
    view_root: Option<backend_library::ViewStateRoot>,
    source_root: Option<[u8; 32]>,
    project_coverage: &BTreeMap<backend_library::PackageKey, ProjectCoverageEvidence>,
) -> Shelf {
    let rows = root.rows();
    let counts = language_counts(rows);
    let sole_project = rows
        .iter()
        .filter(|row| matches!(row.id, RowId::Package(_)))
        .count()
        == 1;
    let mut entries = rows
        .iter()
        .filter(|row| matches!(row.id, RowId::Package(_)))
        .map(|row| {
            let evidence = match (row.id, view_root, source_root) {
                (RowId::Package(key), Some(view_root), Some(source_root)) if !sole_project => {
                    project_coverage.get(&key).filter(|evidence| {
                        backend_library::PackageReference::parse(row.label.clone()).is_ok_and(
                            |package| evidence.matches(view_root, source_root, key, &package),
                        )
                    })
                }
                _ => None,
            };
            shelf_entry(row, &counts, &facts, sole_project, evidence)
        })
        .collect::<Vec<_>>();
    synthesise_loose(&mut entries, &counts);
    entries.sort_by(|left, right| left.identity().name().cmp(right.identity().name()));
    Shelf::new(revision, entries).with_facts(facts)
}

fn language_counts(rows: &[Row]) -> BTreeMap<String, BTreeMap<Language, u64>> {
    let mut counts: BTreeMap<String, BTreeMap<Language, u64>> = BTreeMap::new();
    for row in rows {
        let captured = row.source.captured();
        let identity = Identity::parse(&row.label).with_captured_source(
            captured.map(backend_library::SourceLocation::path),
            captured.map(backend_library::SourceLocation::start_line),
        );
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

fn shelf_entry(
    row: &Row,
    counts: &BTreeMap<String, BTreeMap<Language, u64>>,
    facts: &PublishedFacts,
    sole_project: bool,
    project_coverage: Option<&ProjectCoverageEvidence>,
) -> ShelfEntry {
    let identity = package_identity(row);
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
    let readiness = project_readiness(row.state, &row.label, facts, sole_project);
    let entry = ShelfEntry::new(identity, readiness);
    // The lane statement and the row count describe the whole revision, so
    // they are attributed to a project only when it is the revision's only one.
    let entry = if sole_project {
        entry
            .with_published_rows(facts.rows())
            .with_partial_lanes(match row.state {
                backend_library::RowState::Ready => gapped_lanes(facts),
                backend_library::RowState::Loading | backend_library::RowState::Failed => {
                    Vec::new()
                }
            })
    } else if let Some(evidence) = project_coverage.filter(|evidence| evidence.semantic_partial()) {
        entry.with_partial_lanes(vec![backend_library::Lane::Semantic])
    } else {
        entry
    };
    if languages.is_empty() {
        // Package-only replies carry no declaration count evidence. Do not
        // turn the absence of symbol rows into an authoritative zero.
        entry
    } else {
        entry.with_languages(languages)
    }
}

fn package_identity(row: &Row) -> Identity {
    let identity = Identity::parse_with_key(&row.label, row.id.into());
    if matches!(row.id, RowId::Package(_)) {
        row.signature
            .as_deref()
            .map(|name| identity.clone().with_manifest_name(name))
            .unwrap_or(identity)
    } else {
        identity
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
