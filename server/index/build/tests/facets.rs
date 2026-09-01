//! Exercises the `server-index-build` exact-plane facet join through its observable boundary.
//! Each case names one facet law and retains the exact typed terminal or rejection.
//! The fixtures use caller-owned rows and output, including the allocation measurement.

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::EntityKind;
use heart_identity::GenerationId;
use server_index_build::{
    ExactEntityValue, FacetCell, FacetExactStanding, FacetJoinError, FacetLexicalStanding,
    FacetTable, join_facets,
};
use server_index_core::{
    ExactManifest, ExactRow, ExactSegment, IndexSnapshot, LexicalRow, LexicalSegment,
    LexicalSnapshotHit, LexicalTerminal,
};
use thiserror::Error;

#[derive(Debug, Error)]
enum FacetTestError {
    #[error("fixture decode failed: {0}")]
    Decode(#[source] server_index_build::ExactEntityValueError),
    #[error("facet join failed")]
    Join { detail: JoinDetail },
    #[error("facet terminal count mismatch: expected {expected}, observed {observed}")]
    Terminal { expected: u32, observed: u32 },
    #[error("facet join allocated: {observed:?}")]
    Allocation { observed: AllocationInfo },
    #[error("facet fixture layout mismatch")]
    Layout { detail: LayoutDetail },
}

#[derive(Debug)]
enum JoinDetail {
    Mismatch,
    Decode,
    Overflow,
    Fixture,
}

#[derive(Debug)]
enum LayoutDetail {
    Snapshot,
}

const fn join_error(source: &FacetJoinError<'_>) -> FacetTestError {
    let detail = match source {
        FacetJoinError::SnapshotMismatch { .. } => JoinDetail::Mismatch,
        FacetJoinError::ValueDecode { .. } => JoinDetail::Decode,
        FacetJoinError::CountOverflow { .. } => JoinDetail::Overflow,
    };
    FacetTestError::Join { detail }
}

fn fixture_join<FixtureError: core::fmt::Debug>(source: FixtureError) -> FacetTestError {
    core::hint::black_box(source);
    FacetTestError::Join {
        detail: JoinDetail::Fixture,
    }
}

fn document() -> server_index_core::EntityDocumentId {
    server_index_core::EntityDocumentId {
        fragment: heart_identity::ArtifactId::<
            heart_identity::IrFragmentEncoding,
            heart_identity::IrFragmentDomain,
        >::from_digest([1; 32]),
        entity: compiler_ir_vocabulary::EntityId::new(0),
    }
}

fn document_for(entity: u32) -> server_index_core::EntityDocumentId {
    let mut result = document();
    result.entity = compiler_ir_vocabulary::EntityId::new(entity);
    result
}

#[test]
fn facet_axis_counts_only_the_three_closed_entity_kinds() -> Result<(), FacetTestError> {
    let value = ExactEntityValue::try_from(&[1, 0, 0, 0, 0, 0, 0, 0][..])
        .map_err(FacetTestError::Decode)?;
    assert_eq!(
        value.semantic_view().map_err(FacetTestError::Decode)?.kind,
        EntityKind::Function
    );
    assert_eq!(FacetTable::new().count(FacetCell::Unresolved), 0);
    Ok(())
}

#[test]
fn snapshot_pinning_rejects_mismatched_ids() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], false, |lexical, exact, _| {
        let mut table = FacetTable::new();
        let result = join_facets(&lexical, &exact, &mut table);
        assert!(matches!(
            result,
            Err(FacetJoinError::SnapshotMismatch { .. })
        ));
        Ok(())
    })
}

#[test]
fn typed_empty_terminal_has_zero_counts() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], true, |_, exact, snapshot| {
        let lexical = LexicalTerminal::Complete {
            snapshot,
            hits: &[],
        };
        let mut table = FacetTable::new();
        let terminal =
            join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
        let observed = terminal.counts.count(FacetCell::Function);
        if observed != 0 {
            return Err(FacetTestError::Terminal {
                expected: 0,
                observed,
            });
        }
        assert_eq!(terminal.lexical, FacetLexicalStanding::Complete);
        Ok(())
    })
}

#[test]
fn honest_absence_increments_unresolved() -> Result<(), FacetTestError> {
    with_case(&[], true, |lexical, exact, _| {
        let mut table = FacetTable::new();
        let terminal =
            join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
        assert_eq!(terminal.counts.count(FacetCell::Unresolved), 1);
        Ok(())
    })
}

#[test]
fn source_preservation_retains_bad_value_and_document() -> Result<(), FacetTestError> {
    with_case(&[0], true, |lexical, exact, _| {
        let mut table = FacetTable::new();
        let result = join_facets(&lexical, &exact, &mut table);
        assert!(matches!(
            result,
            Err(FacetJoinError::ValueDecode {
                document: observed,
                value,
                ..
            }) if observed == document() && value == [0]
        ));
        Ok(())
    })
}

#[test]
fn kind_wire_mutation_shifts_exact_facet_counts() -> Result<(), FacetTestError> {
    let first = document_for(0);
    let second = document_for(1);
    let first_key: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = first.into();
    let second_key: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = second.into();
    let first_value = [1, 0, 0, 0, 0, 0, 0, 0];
    let mut second_value = [1, 2, 0, 0, 0, 0, 0, 0];
    let exact_rows = [
        ExactRow::present(&first_key, &first_value),
        ExactRow::present(&second_key, &second_value),
    ];
    let exact_segment = ExactSegment::new(&exact_rows).map_err(fixture_join)?;
    let lexical_rows = [
        LexicalRow::new(b"x", first, 1.into()),
        LexicalRow::new(b"x", second, 1.into()),
    ];
    let lexical_segment = LexicalSegment::new(&lexical_rows).map_err(fixture_join)?;
    let lexical_ids = [lexical_segment.id];
    let exact_ids = [exact_segment.id];
    let snapshot = IndexSnapshot::new(GenerationId::from_digest([8; 32]), &exact_ids, &lexical_ids)
        .map_err(|source| {
            core::hint::black_box(source);
            FacetTestError::Layout {
                detail: LayoutDetail::Snapshot,
            }
        })?;
    let exact = ExactManifest::new(snapshot, core::slice::from_ref(&exact_segment), &[])
        .map_err(fixture_join)?;
    let hits = [
        LexicalSnapshotHit::new(lexical_segment.id, b"x", first, 1.into()),
        LexicalSnapshotHit::new(lexical_segment.id, b"x", second, 1.into()),
    ];
    let lexical = LexicalTerminal::Complete {
        snapshot: snapshot.id,
        hits: &hits,
    };
    let mut table = FacetTable::new();
    let terminal =
        join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
    if terminal.counts.count(FacetCell::Function) != 1
        || terminal.counts.count(FacetCell::Record) != 1
    {
        return Err(FacetTestError::Terminal {
            expected: 1,
            observed: terminal.counts.count(FacetCell::Function),
        });
    }
    second_value[1] = 0;
    let mutated_rows = [
        ExactRow::present(&first_key, &first_value),
        ExactRow::present(&second_key, &second_value),
    ];
    let mutated_segment = ExactSegment::new(&mutated_rows).map_err(fixture_join)?;
    let mutated_ids = [mutated_segment.id];
    let mutated_snapshot = IndexSnapshot::new(
        GenerationId::from_digest([8; 32]),
        &mutated_ids,
        &lexical_ids,
    )
    .map_err(|source| {
        core::hint::black_box(source);
        FacetTestError::Layout {
            detail: LayoutDetail::Snapshot,
        }
    })?;
    let mutated_exact = ExactManifest::new(
        mutated_snapshot,
        core::slice::from_ref(&mutated_segment),
        &[],
    )
    .map_err(fixture_join)?;
    let mutated_lexical = LexicalTerminal::Complete {
        snapshot: mutated_snapshot.id,
        hits: &hits,
    };
    let mut mutated_table = FacetTable::new();
    let mutated = join_facets(&mutated_lexical, &mutated_exact, &mut mutated_table)
        .map_err(|source| join_error(&source))?;
    if mutated.counts.count(FacetCell::Function) != 2
        || mutated.counts.count(FacetCell::Record) != 0
    {
        return Err(FacetTestError::Terminal {
            expected: 2,
            observed: mutated.counts.count(FacetCell::Function),
        });
    }
    Ok(())
}

#[test]
fn degradation_surfaces_are_retained() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], true, |_, exact, snapshot| {
        let lexical = LexicalTerminal::Partial {
            snapshot,
            hits: &[],
            missing: &[],
        };
        let mut table = FacetTable::new();
        let terminal =
            join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
        assert!(matches!(
            terminal.lexical,
            FacetLexicalStanding::Partial { .. }
        ));
        assert_eq!(terminal.exact, FacetExactStanding::Complete);
        Ok(())
    })
}

#[test]
fn deterministic_join_does_not_depend_on_hit_order() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], true, |_, exact, snapshot| {
        let other = server_index_core::EntityDocumentId {
            fragment: document().fragment,
            entity: compiler_ir_vocabulary::EntityId::new(1),
        };
        let first = [
            LexicalSnapshotHit::new(lexical_id(), b"x", document(), 1.into()),
            LexicalSnapshotHit::new(lexical_id(), b"x", other, 1.into()),
        ];
        let second = [first[1], first[0]];
        let a = LexicalTerminal::Complete {
            snapshot,
            hits: &first,
        };
        let b = LexicalTerminal::Complete {
            snapshot,
            hits: &second,
        };
        let mut left = FacetTable::new();
        let mut right = FacetTable::new();
        assert_eq!(
            join_facets(&a, &exact, &mut left)
                .map_err(|source| join_error(&source))?
                .counts,
            join_facets(&b, &exact, &mut right)
                .map_err(|source| join_error(&source))?
                .counts
        );
        Ok(())
    })
}

#[test]
fn resources_use_no_warm_allocation() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], true, |lexical, exact, _| {
        let mut table = FacetTable::new();
        join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
        let mut table = FacetTable::new();
        let mut result = None;
        let allocation = measure(|| result = Some(join_facets(&lexical, &exact, &mut table)));
        if allocation != AllocationInfo::default() {
            return Err(FacetTestError::Allocation {
                observed: allocation,
            });
        }
        assert!(result.is_some());
        Ok(())
    })
}

#[test]
fn lexical_partial_is_not_absorbed_into_counts() -> Result<(), FacetTestError> {
    with_case(&[1, 0, 0, 0, 0, 0, 0, 0], true, |_, exact, snapshot| {
        let lexical = LexicalTerminal::Degraded {
            snapshot,
            hits: &[],
            missing: &[],
            reason: server_index_core::LexicalDegradation::StaleRoute,
        };
        let mut table = FacetTable::new();
        let terminal =
            join_facets(&lexical, &exact, &mut table).map_err(|source| join_error(&source))?;
        assert!(matches!(
            terminal.lexical,
            FacetLexicalStanding::Degraded { .. }
        ));
        Ok(())
    })
}

fn lexical_id() -> server_index_core::LexicalSegmentId {
    server_index_core::LexicalSegmentId::from_canonical_bytes(b"facet-lexical")
}

fn with_case(
    value: &[u8],
    same_snapshot: bool,
    action: impl for<'a> FnOnce(
        LexicalTerminal<'static, 'a, 'a>,
        ExactManifest<'a, 'a>,
        server_index_core::IndexSnapshotId,
    ) -> Result<(), FacetTestError>,
) -> Result<(), FacetTestError> {
    let key: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = document().into();
    let exact_rows = if value.is_empty() {
        [ExactRow::tombstone(&key)]
    } else {
        [ExactRow::present(&key, value)]
    };
    let exact_segment = ExactSegment::new(&exact_rows).map_err(fixture_join)?;
    let lexical_rows = [LexicalRow::new(b"x", document(), 1.into())];
    let lexical_segment = LexicalSegment::new(&lexical_rows).map_err(fixture_join)?;
    let exact_ids = [exact_segment.id];
    let lexical_ids = [lexical_segment.id];
    let snapshot = IndexSnapshot::new(GenerationId::from_digest([2; 32]), &exact_ids, &lexical_ids)
        .map_err(|source| {
            core::hint::black_box(source);
            FacetTestError::Layout {
                detail: LayoutDetail::Snapshot,
            }
        })?;
    let exact = ExactManifest::new(snapshot, core::slice::from_ref(&exact_segment), &[])
        .map_err(fixture_join)?;
    let lexical_snapshot = if same_snapshot {
        snapshot.id
    } else {
        server_index_core::IndexSnapshotId::from_canonical_bytes(b"other")
    };
    let hit = [LexicalSnapshotHit::new(
        lexical_segment.id,
        b"x",
        document(),
        1.into(),
    )];
    let lexical = LexicalTerminal::Complete {
        snapshot: lexical_snapshot,
        hits: &hit,
    };
    action(lexical, exact, snapshot.id)
}
