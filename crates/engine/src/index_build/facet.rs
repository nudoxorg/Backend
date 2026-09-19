//! Allocation-free semantic facets composed from one pinned lexical and exact snapshot.
//!
//! The join is intentionally downstream of immutable exact values.  It does not reopen or cache
//! compiler IR: each lexical hit performs one exact lookup and one fixed-width typed decode, then
//! projects closed declaration, type-class, and graph-relation dimensions.

use core::mem::MaybeUninit;

use backend_semantic::index_core::{
    ExactDegradation, ExactManifest, ExactOperation, ExactResolution, ExactSegmentId,
    ExactTerminal, IndexSnapshotId, LexicalDegradation, LexicalSegmentId, LexicalSnapshotHit,
    LexicalTerminal,
};
use backend_semantic::ir::{EntityKind, LinkKind, TypeTag};
use thiserror::Error;

use crate::index_build::{
    ExactEntityValue, ExactEntityValueError, IndexedType, LinkKinds,
    fact::type_tag_code,
    initialized::{InitializationError, try_initialize},
};

/// One closed semantic facet dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetCell {
    /// Closed declaration kind.
    Kind(EntityKind),
    /// Coordinate-free top-level semantic type class.
    TypeClass(TypeTag),
    /// Outbound semantic graph relation kind.
    Relation(LinkKind),
    /// Lexical hit absent from or deleted in the exact plane.
    Unresolved,
}

/// Fixed-capacity facet counts with no heap owner or runtime vocabulary map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetTable {
    kinds: [u32; 15],
    type_classes: [u32; 41],
    relations: [u32; 11],
    unresolved: u32,
}

impl Default for FacetTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FacetTable {
    /// Creates an empty reusable table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            kinds: [0; 15],
            type_classes: [0; 41],
            relations: [0; 11],
            unresolved: 0,
        }
    }

    /// Returns the count in one closed cell.
    #[must_use]
    pub fn count(&self, cell: FacetCell) -> u32 {
        match cell {
            FacetCell::Kind(kind) => self
                .kinds
                .get(usize::from(u16::from(kind)))
                .copied()
                .unwrap_or(0),
            FacetCell::TypeClass(class) => self
                .type_classes
                .get(usize::from(type_tag_code(class)))
                .copied()
                .unwrap_or(0),
            FacetCell::Relation(kind) => LinkKinds::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .and_then(|index| self.relations.get(index))
                .copied()
                .unwrap_or(0),
            FacetCell::Unresolved => self.unresolved,
        }
    }

    fn clear(&mut self) {
        *self = Self::new();
    }

    fn slot(&mut self, cell: FacetCell) -> Option<&mut u32> {
        match cell {
            FacetCell::Kind(kind) => self.kinds.get_mut(usize::from(u16::from(kind))),
            FacetCell::TypeClass(class) => {
                self.type_classes.get_mut(usize::from(type_tag_code(class)))
            }
            FacetCell::Relation(kind) => LinkKinds::ALL
                .iter()
                .position(|candidate| *candidate == kind)
                .and_then(|index| self.relations.get_mut(index)),
            FacetCell::Unresolved => Some(&mut self.unresolved),
        }
    }

    fn increment<'value>(
        &mut self,
        cell: FacetCell,
        document: backend_semantic::index_core::EntityDocumentId,
    ) -> Result<(), FacetJoinError<'value>> {
        let Some(slot) = self.slot(cell) else {
            return Ok(());
        };
        *slot = slot
            .checked_add(1)
            .ok_or(FacetJoinError::CountOverflow { cell, document })?;
        Ok(())
    }
}

/// Lexical-side availability retained by a facet result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetLexicalStanding<'segments> {
    /// Every selected lexical segment was available through a healthy route.
    Complete,
    /// Some selected lexical segments were unavailable.
    Partial {
        /// Exact unavailable lexical segment identities.
        missing: &'segments [LexicalSegmentId],
    },
    /// The route degraded while retaining the pinned snapshot.
    Degraded {
        /// Exact unavailable lexical segment identities.
        missing: &'segments [LexicalSegmentId],
        /// Closed route degradation.
        reason: LexicalDegradation,
    },
}

/// Exact-side availability retained by a facet result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetExactStanding<'segments> {
    /// Every consumed exact lookup was complete.
    Complete,
    /// Some consumed exact lookups had unavailable selected segments.
    Partial {
        /// Exact unavailable segment identities.
        missing: &'segments [ExactSegmentId],
    },
    /// The route degraded while retaining the pinned snapshot.
    Degraded {
        /// Exact unavailable segment identities.
        missing: &'segments [ExactSegmentId],
        /// Closed route degradation.
        reason: ExactDegradation,
    },
}

/// Exact semantic context recovered for one lexical hit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetHitFact {
    /// Closed declaration kind.
    pub kind: EntityKind,
    /// Compact or rich semantic type authority.
    pub semantic_type: IndexedType,
    /// Outbound relation kinds committed by the declaration.
    pub links: LinkKinds,
}

/// One lexical hit paired with its exact semantic context, if present.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetHit {
    /// Stable document identity resolved by both planes.
    pub document: backend_semantic::index_core::EntityDocumentId,
    /// Typed exact fact, or `None` for a deleted or absent exact row.
    pub fact: Option<FacetHitFact>,
}

/// Successful facet counts, per-hit context, and both plane standings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetTerminal<'table, 'lexical, 'exact, 'context> {
    /// Immutable snapshot identity shared by both input planes.
    pub snapshot: IndexSnapshotId,
    /// Caller-owned counts produced by this join.
    pub counts: &'table FacetTable,
    /// Per-hit context in lexical result order.
    pub context: &'context [FacetHit],
    /// Lexical availability retained from the input terminal.
    pub lexical: FacetLexicalStanding<'lexical>,
    /// Exact availability observed while resolving hits.
    pub exact: FacetExactStanding<'exact>,
}

/// Exact rejection from one snapshot-pinned facet join.
#[derive(Debug, Error)]
pub enum FacetJoinError<'value> {
    /// Lexical and exact planes were not pinned to the same snapshot.
    #[error("facet join snapshot mismatch: lexical {lexical:?}, exact {exact:?}")]
    SnapshotMismatch {
        /// Lexical terminal snapshot.
        lexical: IndexSnapshotId,
        /// Exact manifest snapshot.
        exact: IndexSnapshotId,
    },
    /// A present exact value was corrupt.
    #[error("exact value for document {document:?} could not be decoded")]
    ValueDecode {
        /// Rejected document identity.
        document: backend_semantic::index_core::EntityDocumentId,
        /// Complete rejected fixed-width bytes.
        value: &'value [u8],
        /// Exact typed decode cause.
        #[source]
        source: ExactEntityValueError,
    },
    /// One count exceeded its fixed representation.
    #[error("facet cell {cell:?} overflowed for document {document:?}")]
    CountOverflow {
        /// Overflowing closed cell.
        cell: FacetCell,
        /// Document whose admission overflowed the cell.
        document: backend_semantic::index_core::EntityDocumentId,
    },
    /// Caller context cannot retain one row per lexical hit.
    #[error("facet context holds {available} rows; {required} lexical hits need rows")]
    ContextTooSmall {
        /// Required context width.
        required: usize,
        /// Caller-provided context width.
        available: usize,
    },
}

/// Joins lexical hits to typed exact semantic facts without allocation.
///
/// Capacity and snapshot identity are checked before the first context write.  The table is
/// cleared only after those admission checks succeed.
pub fn join_facets<'table, 'lexical, 'manifest, 'segment, 'context>(
    lexical: &LexicalTerminal<'lexical, '_, '_>,
    exact: &ExactManifest<'manifest, 'segment>,
    table: &'table mut FacetTable,
    context: &'context mut [MaybeUninit<FacetHit>],
) -> Result<FacetTerminal<'table, 'lexical, 'manifest, 'context>, FacetJoinError<'segment>> {
    let (snapshot, hits, lexical_standing) = lexical_parts(lexical);
    if snapshot != exact.snapshot {
        return Err(FacetJoinError::SnapshotMismatch {
            lexical: snapshot,
            exact: exact.snapshot,
        });
    }
    if context.len() < hits.len() {
        return Err(FacetJoinError::ContextTooSmall {
            required: hits.len(),
            available: context.len(),
        });
    }
    table.clear();
    let available = context.len();
    let region = context
        .get_mut(..hits.len())
        .ok_or(FacetJoinError::ContextTooSmall {
            required: hits.len(),
            available,
        })?;
    let mut exact_standing = FacetExactStanding::Complete;
    let context = try_initialize(region, hits.iter(), |hit| {
        let key: [u8; backend_semantic::index_core::ENTITY_DOCUMENT_ID_BYTES] = hit.document.into();
        let (resolution, observed) = exact_parts(exact.execute(ExactOperation::new(&key)));
        exact_standing = widen_exact_standing(exact_standing, observed);
        let fact = match resolution {
            ExactResolution::Present {
                value: value_bytes, ..
            } => {
                let value = ExactEntityValue::try_from(value_bytes).map_err(|source| {
                    FacetJoinError::ValueDecode {
                        document: hit.document,
                        value: value_bytes,
                        source,
                    }
                })?;
                let view = value
                    .semantic_view()
                    .map_err(|source| FacetJoinError::ValueDecode {
                        document: hit.document,
                        value: value_bytes,
                        source,
                    })?;
                Some(FacetHitFact {
                    kind: view.kind,
                    semantic_type: view.semantic_type,
                    links: view.links,
                })
            }
            ExactResolution::Deleted { .. } | ExactResolution::Absent => None,
        };
        match fact {
            Some(fact) => record_hit(table, fact, hit.document)?,
            None => table.increment(FacetCell::Unresolved, hit.document)?,
        }
        Ok(FacetHit {
            document: hit.document,
            fact,
        })
    })
    .map_err(|error| match error {
        InitializationError::Value(error) => error,
        InitializationError::Length { .. }
        | InitializationError::Exhausted { .. }
        | InitializationError::Surplus { .. } => FacetJoinError::ContextTooSmall {
            required: hits.len(),
            available,
        },
    })?
    .into_shared();
    Ok(FacetTerminal {
        snapshot,
        counts: table,
        context,
        lexical: lexical_standing,
        exact: exact_standing,
    })
}

fn record_hit<'value>(
    table: &mut FacetTable,
    fact: FacetHitFact,
    document: backend_semantic::index_core::EntityDocumentId,
) -> Result<(), FacetJoinError<'value>> {
    table.increment(FacetCell::Kind(fact.kind), document)?;
    if let IndexedType::Semantic(Some(semantic_type)) = fact.semantic_type {
        table.increment(FacetCell::TypeClass(semantic_type.class), document)?;
    }
    for kind in LinkKinds::ALL {
        if fact.links.contains(kind) {
            table.increment(FacetCell::Relation(kind), document)?;
        }
    }
    Ok(())
}

const fn widen_exact_standing<'segments>(
    current: FacetExactStanding<'segments>,
    observed: FacetExactStanding<'segments>,
) -> FacetExactStanding<'segments> {
    if matches!(current, FacetExactStanding::Degraded { .. }) {
        return current;
    }
    if matches!(observed, FacetExactStanding::Degraded { .. }) {
        return observed;
    }
    if matches!(current, FacetExactStanding::Partial { .. }) {
        return current;
    }
    observed
}

const fn exact_parts<'manifest, 'segment>(
    terminal: ExactTerminal<'manifest, 'segment>,
) -> (ExactResolution<'segment>, FacetExactStanding<'manifest>) {
    match terminal {
        ExactTerminal::Complete { resolution, .. } => (resolution, FacetExactStanding::Complete),
        ExactTerminal::Partial {
            resolution,
            missing,
            ..
        } => (resolution, FacetExactStanding::Partial { missing }),
        ExactTerminal::Degraded {
            resolution,
            missing,
            reason,
            ..
        } => (resolution, FacetExactStanding::Degraded { missing, reason }),
    }
}

const fn lexical_parts<'manifest, 'output, 'bytes>(
    terminal: &LexicalTerminal<'manifest, 'output, 'bytes>,
) -> (
    IndexSnapshotId,
    &'output [LexicalSnapshotHit<'bytes>],
    FacetLexicalStanding<'manifest>,
) {
    match terminal {
        LexicalTerminal::Complete { snapshot, hits } => {
            (*snapshot, hits, FacetLexicalStanding::Complete)
        }
        LexicalTerminal::Partial {
            snapshot,
            hits,
            missing,
        } => (*snapshot, hits, FacetLexicalStanding::Partial { missing }),
        LexicalTerminal::Degraded {
            snapshot,
            hits,
            missing,
            reason,
        } => (
            *snapshot,
            hits,
            FacetLexicalStanding::Degraded {
                missing,
                reason: *reason,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{FacetCell, FacetHit, FacetTable, join_facets};
    use crate::index_build::{ExactEntityValue, IndexedType, LinkKinds, SemanticTypeFact};
    use backend_semantic::index_core::{
        EntityArtifactIdentity, EntityDocumentId, ExactManifest, ExactRow, ExactSegment,
        IndexSnapshot, LexicalManifest, LexicalOperation, LexicalRow, LexicalScore, LexicalSegment,
        LexicalSnapshotHit, LexicalTopK,
    };
    use backend_semantic::ir::{EntityId, EntityKind, LinkKind, TypeId, TypeTag};
    use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};
    use core::mem::MaybeUninit;

    #[test]
    fn one_snapshot_join_exposes_rich_type_and_relation_facets_without_allocation() {
        let document = document(7);
        let links = LinkKinds::NONE
            .insert(LinkKind::Calls)
            .insert(LinkKind::TypeReference)
            .insert(LinkKind::Documents);
        let value = ExactEntityValue::from_facts(
            EntityKind::Function,
            IndexedType::Semantic(Some(SemanticTypeFact {
                coordinate: TypeId::new(19),
                class: TypeTag::Conditional,
            })),
            links,
        );
        let key: [u8; backend_semantic::index_core::ENTITY_DOCUMENT_ID_BYTES] = document.into();
        let exact_rows = [ExactRow::present(&key, value.as_ref())];
        let lexical_rows = [LexicalRow::new(b"derive", document, LexicalScore::from(9))];
        let exact = ExactSegment::new(&exact_rows).expect("canonical exact fixture");
        let lexical = LexicalSegment::new(&lexical_rows).expect("canonical lexical fixture");
        let exact_ids = [exact.id];
        let lexical_ids = [lexical.id];
        let snapshot = IndexSnapshot::new(generation(), &exact_ids, &lexical_ids)
            .expect("bounded shared snapshot");
        let exact_segments = [exact];
        let lexical_segments = [lexical];
        let exact_manifest =
            ExactManifest::new(snapshot, &exact_segments, &[]).expect("exact manifest");
        let lexical_manifest =
            LexicalManifest::new(snapshot, &lexical_segments, &[]).expect("lexical manifest");
        let mut seen = [None];
        let mut lexical_output = [LexicalSnapshotHit::new(
            lexical.id,
            b"",
            document,
            LexicalScore::from(0),
        )];
        let lexical_terminal = lexical_manifest
            .execute(
                LexicalOperation::new(b"derive"),
                LexicalTopK::new(1).expect("bounded top-k"),
                &mut seen,
                &mut lexical_output,
            )
            .expect("lexical lookup");
        let mut table = FacetTable::new();
        let mut context = [MaybeUninit::<FacetHit>::uninit()];
        let terminal = join_facets(&lexical_terminal, &exact_manifest, &mut table, &mut context)
            .expect("typed facet join");

        assert_eq!(terminal.snapshot, snapshot.id);
        assert_eq!(terminal.context[0].document, document);
        assert_eq!(
            terminal.context[0].fact.map(|fact| fact.semantic_type),
            Some(IndexedType::Semantic(Some(SemanticTypeFact {
                coordinate: TypeId::new(19),
                class: TypeTag::Conditional,
            })))
        );
        assert_eq!(
            terminal.counts.count(FacetCell::Kind(EntityKind::Function)),
            1
        );
        assert_eq!(
            terminal
                .counts
                .count(FacetCell::TypeClass(TypeTag::Conditional)),
            1
        );
        for kind in [
            LinkKind::Calls,
            LinkKind::TypeReference,
            LinkKind::Documents,
        ] {
            assert_eq!(terminal.counts.count(FacetCell::Relation(kind)), 1);
        }
        assert_eq!(
            terminal.counts.count(FacetCell::Relation(LinkKind::Reads)),
            0
        );
        assert_eq!(terminal.counts.count(FacetCell::Unresolved), 0);
    }

    fn document(entity: u32) -> EntityDocumentId {
        EntityDocumentId {
            artifact: EntityArtifactIdentity::Compact(ArtifactId::<
                IrFragmentEncoding,
                IrFragmentDomain,
            >::from_encoded_bytes(
                b"semantic-facet-fixture"
            )),
            entity: EntityId::new(entity),
        }
    }

    fn generation() -> GenerationId {
        GenerationId::from_canonical_bytes(b"semantic-facet-generation")
    }
}
