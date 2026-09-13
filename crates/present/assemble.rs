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
use crate::glyph::RelationLabel;
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
    Document, Outline, Row, RowId, RowState, SourceAvailability, SourceExcerpt, SymbolKey,
    ViewSnapshot,
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
    let identity = Identity::parse_with_key(coordinate, IdentityKey::Symbol(document.symbol));
    let language = page_language(&identity, members, document.symbol);
    let source = source_from(&identity, &document.location, &document.excerpt);
    let kind = members
        .iter()
        .find(|row| row.id == RowId::Symbol(document.symbol))
        .and_then(|row| row.kind);
    let signature = document
        .signature
        .as_ref()
        .map(|text| Signature::tokenize(text, language));
    let page = Page::new(identity, kind, source)
        .with_language(language)
        .with_prose(Prose::from_fragments(&document.fragments))
        .with_members(member_groups(members, document.symbol))
        .with_relations(relation_groups(relations, document.symbol))
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

fn member_groups(rows: &[Row], parent: SymbolKey) -> Box<[MemberGroup]> {
    let members = rows
        .iter()
        .filter(|row| row.parent == Some(parent) && row.id != RowId::Symbol(parent))
        .map(|row| {
            let identity = Identity::parse_with_key(&row.label, row.id.into());
            let language = identity.language();
            Member::new(
                identity,
                row.kind,
                row.signature
                    .as_ref()
                    .map(|text| Signature::tokenize(text, language)),
            )
        })
        .collect::<Vec<_>>();
    MemberGroup::group(members)
}

fn relation_groups(rows: &[Row], centre: SymbolKey) -> Box<[RelationGroup]> {
    let relations = rows
        .iter()
        .filter(|row| row.id != RowId::Symbol(centre))
        .map(|row| Relation::new(Identity::parse_with_key(&row.label, row.id.into()), row.kind))
        .collect::<Vec<_>>();
    if relations.is_empty() {
        return Box::new([]);
    }
    vec![RelationGroup::new(RelationLabel::Related, relations)].into_boxed_slice()
}

/// Builds one result page from a bounded snapshot.
#[must_use]
pub fn record_list(query: &str, snapshot: &ViewSnapshot) -> RecordList {
    let rows = snapshot.root.rows();
    let coverage = CoverageLine::new(
        snapshot.root.coverage(),
        u64::try_from(rows.len()).ok(),
    );
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
    RecordList::new(query, coverage, records).with_more(snapshot.next.is_some())
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
    let rows = snapshot.root.rows();
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
    let entries = rows
        .iter()
        .filter(|row| matches!(row.id, RowId::Package(_)))
        .map(|row| shelf_entry(row, &counts))
        .collect::<Vec<_>>();
    Shelf::new(revision, entries)
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

fn readiness_of(row: &Row, published: u64) -> Readiness {
    match row.state {
        RowState::Ready if published > 0 => Readiness::Ready,
        RowState::Ready => Readiness::Requested,
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
