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
use crate::identity::{
    Identity, IdentityKey, IdentityShape, KeyTag, LineNumber, PackagePath, ProjectRef,
};
use crate::language::Language;
use crate::outline::{OutlineEntry, OutlineTree, row_resolver};
use crate::page::{
    Member, MemberGroup, Page, Prose, Relation, RelationGroup, Source, SourceSite, Truncation,
};
use crate::readiness::{PublishedFacts, gapped_lanes, project_readiness};
use crate::record::{CollapsedExternals, Record, RecordList};
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
        .as_deref()
        .and_then(|text| crate::signature::display_text_in(kind, language, text))
        .map(|text| Signature::tokenize(&text, language));
    let page = Page::new(identity, kind, source)
        .with_language(language)
        .with_prose(Prose::from_fragments(&document.fragments))
        .with_members(member_groups(members, centre))
        .with_relations(relation_groups(relations, centre, graph_relations, members))
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
                    .as_deref()
                    .and_then(|text| crate::signature::display_text_in(row.kind, language, text))
                    .map(|text| Signature::tokenize(&text, language)),
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
    members: &[Row],
) -> Box<[RelationGroup]> {
    // A member is already listed with its signature under its own heading;
    // listing it again as a related row would spend a second record on it.
    let listed_as_member = members
        .iter()
        .filter(|row| row.parent == Some(centre))
        .map(|row| row.id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut groups = BTreeMap::<RelationLabel, Vec<Relation>>::new();
    for row in rows.iter().filter(|row| {
        row.id != RowId::Symbol(centre)
            && !listed_as_member.contains(&row.id)
            && !backend_library::is_inert_external_placeholder(row)
    }) {
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
            matches.then_some(
                if edge.confidence >= backend_library::SemanticConfidence::Indexed {
                    relation_label(edge.relation, direction)
                } else {
                    RelationLabel::Unverified(edge.relation, direction)
                },
            )
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

/// Builds the page of one declaration's graph neighbours: the neighbourhood
/// of `center`, without `center` itself. The declaration a reader asked about
/// is not one of its own relations; a graph with nothing else says so.
#[must_use]
pub fn neighbour_list(center: &str, snapshot: &ViewSnapshot) -> RecordList {
    record_list_excluding(
        center,
        snapshot.root.rows(),
        snapshot.root.coverage(),
        snapshot.next.is_some(),
        Some(&Identity::parse(center)),
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
    record_list_excluding(query, rows, coverage, more, None)
}

/// [`record_list_from_rows`], without the declaration `excluding` names (in
/// either publication lane's spelling of it).
fn record_list_excluding(
    query: &str,
    rows: &[Row],
    coverage: &[Coverage],
    more: bool,
    excluding: Option<&Identity>,
) -> RecordList {
    let line = CoverageLine::new(coverage, u64::try_from(rows.len()).ok());
    // An external placeholder without facts names something elsewhere and
    // nothing else; a page lists it as one summary line, never as a record.
    // The exception is the row the query itself names (a graph centred on one
    // of them): it is what was asked for, so it stays a record.
    let (placeholders, declared): (Vec<&Row>, Vec<&Row>) = rows
        .iter()
        .partition(|row| backend_library::is_inert_external_placeholder(row) && row.label != query);
    let externals = CollapsedExternals::from_names(
        placeholders
            .iter()
            .filter_map(|row| backend_library::external_placeholder_display(&row.label)),
    );
    let mut records = declared
        .iter()
        .map(|row| {
            let record = Record::from_row(row);
            match summary_of(row) {
                Some(text) => record.with_summary(text),
                None => record,
            }
        })
        .collect::<Vec<_>>();
    // One record per declaration: when both publication lanes list the same
    // declaration, the structural record is the address and the compiler-backed
    // record only contributes the facts the structural one lacks.
    merge_lane_twins(&mut records);
    if let Some(excluded) = excluding {
        let excluded_site = site_of_identity(excluded);
        records.retain(|record| {
            record.identity().coordinate() != excluded.coordinate()
                && (excluded_site.is_none() || site_of(record) != excluded_site)
        });
    }
    // View rows are ordered by canonical identity for hashing and paging.
    // Scored search rows carry the owner's absolute rank independently of
    // that storage order. Restore it only in the readable projection; a
    // stable sort leaves unscored rows and equal scores in their input order.
    // An import that only brings a name in never outranks the declarations
    // sharing its rank.
    records.sort_by_key(|record| {
        (
            std::cmp::Reverse(record.score().map(crate::record::Score::get)),
            record.kind() == Some(backend_library::DeclarationKind::Import),
        )
    });
    RecordList::new(query, line, records)
        .with_more(more)
        .with_collapsed_externals(externals)
}

/// The declaration a record names: its project, file, line and name, which
/// both publication lanes agree on for one declaration.
fn site_of(record: &Record) -> Option<(String, String, u32, String)> {
    site_of_identity(record.identity())
}

fn site_of_identity(identity: &Identity) -> Option<(String, String, u32, String)> {
    if !matches!(
        identity.shape(),
        IdentityShape::Declaration | IdentityShape::Semantic
    ) {
        return None;
    }
    Some((
        identity.project()?.root().to_owned(),
        identity.path()?.as_str().to_owned(),
        identity.line()?.get(),
        identity.name().to_owned(),
    ))
}

/// Collapses each structural/compiler-backed pair of records for one
/// declaration into the structural record.
///
/// The pairing is deliberately as strict as the owner's: the same site
/// (project, file, line, name), exactly one record per lane, and an import
/// only with an import. Anything ambiguous stays listed.
fn merge_lane_twins(records: &mut Vec<Record>) {
    let mut sites = BTreeMap::<(String, String, u32, String), (Vec<usize>, Vec<usize>)>::new();
    for (at, record) in records.iter().enumerate() {
        let Some(site) = site_of(record) else {
            continue;
        };
        let lanes = sites.entry(site).or_default();
        if record.identity().shape() == IdentityShape::Semantic {
            lanes.1.push(at);
        } else {
            lanes.0.push(at);
        }
    }
    let mut absorbed = Vec::new();
    for (structural, semantic) in sites.values() {
        let ([structural], [semantic]) = (structural.as_slice(), semantic.as_slice()) else {
            continue;
        };
        let imports =
            |at: usize| records[at].kind() == Some(backend_library::DeclarationKind::Import);
        if imports(*structural) != imports(*semantic) {
            continue;
        }
        absorbed.push((*structural, *semantic));
    }
    let mut redundant = vec![false; records.len()];
    for (structural, semantic) in absorbed {
        let other = records[semantic].clone();
        records[structural].absorb(&other);
        redundant[semantic] = true;
    }
    let mut at = 0;
    records.retain(|_| {
        let keep = !redundant[at];
        at += 1;
        keep
    });
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
        .map(|row| shelf_entry(row, &counts, &facts, sole_project))
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
    if matches!(row.id, backend_library::RowId::Package(_)) {
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
        crate::outline::merge_lane_twins(roots),
        outline.extent,
    )
}

/// Returns the project a coordinate belongs to, for relative spellings.
#[must_use]
pub fn project_of(coordinate: &str) -> Option<ProjectRef> {
    Identity::parse(coordinate).project().cloned()
}
