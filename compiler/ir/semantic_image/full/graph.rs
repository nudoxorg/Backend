//! Canonical relations and authority-observed graph evidence.

use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::{
    Confidence, FactAvailability, Ir, Link, LinkId, LinkKind, LinkOccurrenceId, LinkTarget,
    OccurrenceAuthorityFacts, SourceSpan,
};

use super::super::{canonical::CanonicalFullPlan, fault::CoreSemanticImageFault};
use super::{GraphPlan, GraphPlanFault};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct RelationFastKey {
    from: [u8; 32],
    target_tag: u8,
    target: [u8; 32],
    kind: u8,
    confidence: u8,
    source_present: u8,
    source_file: u32,
    source_start: u32,
    source_end: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct OccurrenceFastKey {
    relation: u32,
    confidence: u8,
    source_availability: u8,
    source_present: u8,
    source_file: u32,
    source_start: u32,
    source_end: u32,
}

impl GraphPlan {
    pub(super) fn build(
        ir: &Ir,
        canonical: &CanonicalFullPlan<'_>,
    ) -> Result<Self, GraphPlanBuildError> {
        let relation_count = ir.storage_columns().graph.from.len();
        let mut keyed_relations = Vec::with_capacity(relation_count);
        for raw in 0..relation_count {
            let raw = u32::try_from(raw).map_err(|_| GraphPlanFault::GeometryOverflow {
                rows: relation_count,
            })?;
            let id = LinkId::new(raw);
            let relation = ir.link(id).ok_or(GraphPlanFault::MissingLink {
                link: id,
                count: u32::try_from(relation_count).map_err(|_| GraphPlanFault::GeometryOverflow {
                    rows: relation_count,
                })?,
            })?;
            keyed_relations.push((id, relation_fast_key(canonical, relation)?));
        }
        keyed_relations.sort_unstable_by(|(_, left), (_, right)| left.cmp(right));
        order_equal_relation_keys(ir, canonical, &mut keyed_relations)?;
        let mut relations = Vec::with_capacity(relation_count);
        let mut relation_remap = vec![0_u32; relation_count];
        for (canonical_row, (id, _)) in keyed_relations.into_iter().enumerate() {
            let canonical_row = u32::try_from(canonical_row).map_err(|_| {
                GraphPlanFault::GeometryOverflow { rows: relation_count }
            })?;
            relation_remap[id.index()] = canonical_row;
            relations.push(id);
        }

        let occurrence_count = ir.storage_columns().graph.occurrences.links.len();
        let mut keyed_occurrences = Vec::with_capacity(occurrence_count);
        for raw in 0..occurrence_count {
            let raw = u32::try_from(raw).map_err(|_| GraphPlanFault::GeometryOverflow {
                rows: occurrence_count,
            })?;
            let id = LinkOccurrenceId::new(raw);
            let occurrence = ir.link_occurrence(id).ok_or(GraphPlanFault::MissingOccurrence {
                occurrence: raw,
                count: u32::try_from(occurrence_count).map_err(|_| {
                    GraphPlanFault::GeometryOverflow { rows: occurrence_count }
                })?,
            })?;
            let authority = occurrence_authority(ir, id, occurrence_count)?;
            let expected = matches!(authority.source, FactAvailability::Captured);
            if expected != occurrence.source.is_some() {
                return Err(GraphPlanFault::OccurrenceAuthority {
                    occurrence: raw,
                    claimed: authority.source,
                    has_source: occurrence.source.is_some(),
                }
                .into());
            }
            let relation = relation_remap.get(occurrence.link.index()).copied().ok_or(
                GraphPlanFault::MissingLink {
                    link: occurrence.link,
                    count: u32::try_from(relation_count).map_err(|_| {
                        GraphPlanFault::GeometryOverflow { rows: relation_count }
                    })?,
                },
            )?;
            keyed_occurrences.push((raw, occurrence_fast_key(canonical, relation, occurrence, authority)?));
        }
        // Equal evidence rows remain distinct independent observations. Their
        // emitted projections are byte-identical, so equal-key permutation
        // cannot change a future canonical image and no raw occurrence map is
        // needed by any other full plane.
        keyed_occurrences.sort_unstable_by(|(_, left), (_, right)| left.cmp(right));
        let occurrences = keyed_occurrences
            .into_iter()
            .map(|(raw, _)| LinkOccurrenceId::new(raw))
            .collect();
        Ok(Self { relations, relation_remap, occurrences })
    }
}

#[derive(Debug)]
pub(super) enum GraphPlanBuildError {
    Core(CoreSemanticImageFault),
    Graph(GraphPlanFault),
}

impl From<CoreSemanticImageFault> for GraphPlanBuildError {
    fn from(value: CoreSemanticImageFault) -> Self { Self::Core(value) }
}
impl From<GraphPlanFault> for GraphPlanBuildError {
    fn from(value: GraphPlanFault) -> Self { Self::Graph(value) }
}

fn relation_fast_key(
    canonical: &CanonicalFullPlan<'_>,
    relation: Link,
) -> Result<RelationFastKey, CoreSemanticImageFault> {
    let from = declaration_identity_bytes(canonical.entity_identity(relation.from)?);
    let (target_tag, target) = match relation.target {
        LinkTarget::Local(entity) => (local_target_tag(), declaration_identity_bytes(canonical.entity_identity(entity)?)),
        LinkTarget::External(external) => (external_target_tag(), canonical.external_fingerprint(external)?),
    };
    let (source_present, source_file, source_start, source_end) = source_key(canonical, relation.source)?;
    Ok(RelationFastKey {
        from,
        target_tag,
        target,
        kind: link_kind_code(relation.kind),
        confidence: confidence_code(relation.confidence),
        source_present,
        source_file,
        source_start,
        source_end,
    })
}

fn occurrence_fast_key(
    canonical: &CanonicalFullPlan<'_>,
    relation: u32,
    occurrence: crate::LinkOccurrence,
    authority: OccurrenceAuthorityFacts,
) -> Result<OccurrenceFastKey, CoreSemanticImageFault> {
    let (source_present, source_file, source_start, source_end) = source_key(canonical, occurrence.source)?;
    Ok(OccurrenceFastKey {
        relation,
        confidence: confidence_code(occurrence.confidence),
        source_availability: availability_code(authority.source),
        source_present,
        source_file,
        source_start,
        source_end,
    })
}

fn order_equal_relation_keys(
    ir: &Ir,
    canonical: &CanonicalFullPlan<'_>,
    keyed: &mut [(LinkId, RelationFastKey)],
) -> Result<(), GraphPlanBuildError> {
    let mut start = 0_usize;
    while start < keyed.len() {
        let key = keyed[start].1;
        let mut end = start.checked_add(1).ok_or(GraphPlanFault::GeometryOverflow {
            rows: keyed.len(),
        })?;
        while end < keyed.len() && keyed[end].1 == key {
            end += 1;
        }
        for index in start.checked_add(1).ok_or(GraphPlanFault::GeometryOverflow {
            rows: keyed.len(),
        })?..end {
            let value = keyed[index];
            let mut insert = index;
            while insert > start {
                let previous = keyed[insert - 1];
                match compare_relation(ir, canonical, previous.0, value.0)? {
                    Ordering::Less => break,
                    Ordering::Equal => {
                        return Err(GraphPlanFault::DuplicateCanonicalRelation {
                            first: previous.0,
                            second: value.0,
                        }
                        .into());
                    }
                    Ordering::Greater => {
                        keyed[insert] = previous;
                        insert -= 1;
                    }
                }
            }
            keyed[insert] = value;
        }
        start = end;
    }
    Ok(())
}

fn compare_relation(
    ir: &Ir,
    canonical: &CanonicalFullPlan<'_>,
    left: LinkId,
    right: LinkId,
) -> Result<Ordering, GraphPlanBuildError> {
    let count = ir.storage_columns().graph.from.len();
    let left_relation = ir.link(left).ok_or(GraphPlanFault::MissingLink {
        link: left,
        count: u32::try_from(count).map_err(|_| GraphPlanFault::GeometryOverflow { rows: count })?,
    })?;
    let right_relation = ir.link(right).ok_or(GraphPlanFault::MissingLink {
        link: right,
        count: u32::try_from(count).map_err(|_| GraphPlanFault::GeometryOverflow { rows: count })?,
    })?;
    let mut ordering = canonical
        .entity_identity(left_relation.from)?
        .cmp(&canonical.entity_identity(right_relation.from)?);
    if ordering == Ordering::Equal {
        ordering = canonical.compare_link_target(left_relation.target, right_relation.target)?;
    }
    if ordering == Ordering::Equal {
        ordering = link_kind_code(left_relation.kind).cmp(&link_kind_code(right_relation.kind));
    }
    if ordering == Ordering::Equal {
        ordering = confidence_code(left_relation.confidence).cmp(&confidence_code(right_relation.confidence));
    }
    if ordering == Ordering::Equal {
        ordering = canonical.compare_source(left_relation.source, right_relation.source)?;
    }
    Ok(ordering)
}

fn occurrence_authority(
    ir: &Ir,
    id: LinkOccurrenceId,
    count: usize,
) -> Result<OccurrenceAuthorityFacts, GraphPlanFault> {
    ir.occurrence_authority_columns()
        .source
        .get(id.index())
        .copied()
        .map(|source| OccurrenceAuthorityFacts { source })
        .ok_or(GraphPlanFault::MissingOccurrence {
            occurrence: id.raw,
            count: u32::try_from(count).map_err(|_| GraphPlanFault::GeometryOverflow { rows: count })?,
        })
}

fn source_key(
    canonical: &CanonicalFullPlan<'_>,
    source: Option<SourceSpan>,
) -> Result<(u8, u32, u32, u32), CoreSemanticImageFault> {
    match source {
        None => Ok((absent_tag(), 0, 0, 0)),
        Some(source) => Ok((
            present_tag(),
            canonical.atom(source.file())?,
            source.start(),
            source.end(),
        )),
    }
}

fn declaration_identity_bytes(identity: crate::DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(identity.family.as_bytes());
    bytes[16..].copy_from_slice(identity.variant.as_bytes());
    bytes
}

const fn local_target_tag() -> u8 { 0 }
const fn external_target_tag() -> u8 { 1 }
const fn absent_tag() -> u8 { 0 }
const fn present_tag() -> u8 { 1 }
const fn availability_code(value: FactAvailability) -> u8 {
    match value {
        FactAvailability::Captured => 0,
        FactAvailability::Unavailable => 1,
    }
}
const fn confidence_code(value: Confidence) -> u8 {
    match value {
        Confidence::Syntactic => 0,
        Confidence::Heuristic => 1,
        Confidence::Indexed => 2,
        Confidence::Imported => 3,
        Confidence::Compiler => 4,
    }
}
const fn link_kind_code(value: LinkKind) -> u8 {
    match value {
        LinkKind::Calls => 0,
        LinkKind::MethodCall => 1,
        LinkKind::TypeReference => 2,
        LinkKind::Reads => 3,
        LinkKind::Writes => 4,
        LinkKind::Imports => 5,
        LinkKind::Implements => 6,
        LinkKind::Overrides => 7,
        LinkKind::Reexports => 8,
        LinkKind::Inherits => 9,
        LinkKind::Documents => 10,
    }
}
