//! Falsifiers for the local exact-scan vector retrieval route.
//! Each test names one ordering, terminal, provenance, or resource law.
//! Fixtures remain borrowed from the sealed-boundary integration support.

#![allow(clippy::expect_used, reason = "fixture construction is checked setup")]

mod support;

use allocation_counter::measure;
use compiler_ir_vocabulary::EntityId;
use server_index_graph_vector::{
    Cancellation, Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority, VectorHit,
    VectorPoint, VectorQueryError,
};
use server_index_retrieval::{
    CancellationCause, RetrievalAbsence, RetrievalBoundary, RetrievalCoverage,
    RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
    VectorDegradation, VectorRoute,
};

use support::{SealedFixture, with_sealed_fixture};

#[test]
fn s1_vector_rejects_a_segment_outside_the_pinned_selection() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let coordinates = [2_i16, 0];
        let points = [VectorPoint::new(EntityId::new(7), &coordinates)];
        let segment =
            ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(3), &points)
                .expect("valid unselected segment");
        let sentinel = Some(hit(fixture, PartitionId::new(1), 99, 7));
        let mut output = [sentinel];
        let terminal = boundary.vector(VectorRoute::Healthy, &[segment], &[0, 0], 1, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::UnpinnedVectorSegment { position: 0, partition },
                ..
            } if partition == PartitionId::new(3)
        ));
        assert_eq!(output, [sentinel]);
    });
}

#[test]
fn s2_vector_cancelled_preflight_preserves_output() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        cancellation.cancel();
        let boundary = RetrievalBoundary::new(
            fixture.sealed,
            &cancellation,
            fixture.graph_authority,
            fixture.vector_authority,
            &selection,
        )
        .expect("valid boundary");
        let sentinel = Some(hit(fixture, PartitionId::new(1), 99, 7));
        let mut output = [sentinel];
        let terminal = boundary.vector(
            VectorRoute::Healthy,
            fixture.vector_segments,
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Cancelled {
                cause: CancellationCause::Preflight,
                ..
            }
        ));
        assert_eq!(output, [sentinel]);
    });
}

#[test]
fn s3_vector_query_dimension_retains_exact_operands() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let sentinel = Some(hit(fixture, PartitionId::new(1), 99, 7));
        let mut output = [sentinel];
        let terminal = boundary.vector(
            VectorRoute::Healthy,
            fixture.vector_segments,
            &[0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::VectorQuery(VectorQueryError::QueryDimension {
                    expected: 2,
                    observed: 1,
                }),
                ..
            }
        ));
        assert_eq!(output, [sentinel]);
    });
}

#[test]
fn s4_vector_partial_retains_exact_missing_partitions() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None];
        let terminal = boundary.vector(
            VectorRoute::Healthy,
            &fixture.vector_segments[..1],
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Partial {
                result: RetrievalResult::Vector { written: 1, .. },
                absence: RetrievalAbsence::Partitions(missing),
                ..
            } if missing.as_ref() == [PartitionId::new(2)]
        ));
    });
}

#[test]
fn s5_vector_degraded_retains_reason_and_coverage() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None];
        let terminal = boundary.vector(
            VectorRoute::Degraded(VectorDegradation::StaleRoute),
            fixture.vector_segments,
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Degraded {
                coverage: RetrievalCoverage::Complete,
                degradation: RetrievalDegradation::Vector(VectorDegradation::StaleRoute),
                ..
            }
        ));
    });
}

#[test]
fn f1_vector_missing_partitions_preserve_pinned_order() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None];
        let terminal = boundary.vector(VectorRoute::Healthy, &[], &[0, 0], 1, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Partial {
                absence: RetrievalAbsence::Partitions(missing),
                ..
            } if missing.as_ref() == [PartitionId::new(1), PartitionId::new(2)]
        ));
    });
}

#[test]
fn f2_vector_written_count_and_prefix_are_exact() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let sentinel = Some(hit(fixture, PartitionId::new(2), 99, 77));
        let mut output = [sentinel, sentinel];
        let terminal = boundary.vector(
            VectorRoute::Healthy,
            &fixture.vector_segments[..1],
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Partial {
                result: RetrievalResult::Vector { hits, written: 1 },
                ..
            } if hits.first().copied() == Some(Some(hit(fixture, PartitionId::new(1), 9, 1)))
                && hits.get(1).copied() == Some(sentinel)
        ));
    });
}

#[test]
fn f3_vector_degraded_partial_retains_missing_coverage() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None];
        let terminal = boundary.vector(
            VectorRoute::Degraded(VectorDegradation::PartitionSourceUnavailable),
            &fixture.vector_segments[..1],
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Degraded {
                coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
                degradation: RetrievalDegradation::Vector(
                    VectorDegradation::PartitionSourceUnavailable
                ),
                ..
            } if missing.as_ref() == [PartitionId::new(2)]
        ));
    });
}

#[test]
fn s6_s7_vector_rank_is_deterministic_and_retains_provenance() {
    with_sealed_fixture(|fixture| {
        let coordinates = [1_i16, 0];
        let first_points = [VectorPoint::new(EntityId::new(9), &coordinates)];
        let second_points = [VectorPoint::new(EntityId::new(3), &coordinates)];
        let segments = [
            ValidatedVectorSegment::try_new(
                fixture.vector_authority,
                PartitionId::new(1),
                &first_points,
            )
            .expect("valid first segment"),
            ValidatedVectorSegment::try_new(
                fixture.vector_authority,
                PartitionId::new(2),
                &second_points,
            )
            .expect("valid second segment"),
        ];
        let selection = segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None, None];
        let terminal = boundary.vector(VectorRoute::Healthy, &segments, &[0, 0], 2, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Complete {
                result: RetrievalResult::Vector { hits, written: 2 },
                ..
            } if hits.first().copied() == Some(Some(hit(fixture, PartitionId::new(2), 3, 1)))
                && hits.get(1).copied() == Some(Some(hit(fixture, PartitionId::new(1), 9, 1)))
        ));
    });
}

#[test]
fn f5_vector_rejects_a_foreign_segment_authority() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let foreign = VectorAuthority::new(
            fixture.vector_authority.snapshot,
            ModelId::new([0x72; 16]),
            fixture.vector_authority.dimension,
            Metric::SquaredEuclidean,
        );
        let coordinates = [1_i16, 0];
        let points = [VectorPoint::new(EntityId::new(9), &coordinates)];
        let segment = ValidatedVectorSegment::try_new(foreign, PartitionId::new(1), &points)
            .expect("valid foreign segment");
        let sentinel = Some(hit(fixture, PartitionId::new(1), 99, 77));
        let mut output = [sentinel];
        let terminal = boundary.vector(VectorRoute::Healthy, &[segment], &[0, 0], 1, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::VectorQuery(
                    VectorQueryError::WrongSegmentAuthority { observed, .. }
                ),
                ..
            } if observed == foreign
        ));
        assert_eq!(output, [sentinel]);
    });
}

#[test]
fn f6_vector_same_entity_ties_break_by_partition() {
    with_sealed_fixture(|fixture| {
        let coordinates = [1_i16, 0];
        let points = [VectorPoint::new(EntityId::new(9), &coordinates)];
        let segments = [
            ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(1), &points)
                .expect("valid first segment"),
            ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(2), &points)
                .expect("valid second segment"),
        ];
        let selection = segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None, None];
        let terminal = boundary.vector(VectorRoute::Healthy, &segments, &[0, 0], 2, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Complete {
                result: RetrievalResult::Vector { hits, written: 2 },
                ..
            } if hits.first().copied() == Some(Some(hit(fixture, PartitionId::new(1), 9, 1)))
                && hits.get(1).copied() == Some(Some(hit(fixture, PartitionId::new(2), 9, 1)))
        ));
    });
}

#[test]
fn f6_vector_ties_break_by_partition_regardless_of_segment_order() {
    with_sealed_fixture(|fixture| {
        let coordinates = [1_i16, 0];
        let points = [VectorPoint::new(EntityId::new(9), &coordinates)];
        let segments = [
            ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(2), &points)
                .expect("valid reversed first segment"),
            ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(1), &points)
                .expect("valid reversed second segment"),
        ];
        let selection = [segments[1].descriptor(), segments[0].descriptor()];
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let mut output = [None, None];
        let terminal = boundary.vector(VectorRoute::Healthy, &segments, &[0, 0], 2, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Complete {
                result: RetrievalResult::Vector { hits, written: 2 },
                ..
            } if hits.first().copied() == Some(Some(hit(fixture, PartitionId::new(1), 9, 1)))
                && hits.get(1).copied() == Some(Some(hit(fixture, PartitionId::new(2), 9, 1)))
        ));
    });
}

#[test]
fn s8_vector_route_allocates_nothing_on_nonempty_scan() {
    with_sealed_fixture(|fixture| {
        let selection = fixture.vector_segments.map(|segment| segment.descriptor());
        let cancellation = Cancellation::new();
        let boundary = boundary(fixture, &cancellation, &selection);
        let allocations = measure(|| {
            let mut output = [None, None];
            let terminal = boundary.vector(
                VectorRoute::Healthy,
                fixture.vector_segments,
                &[0, 0],
                2,
                &mut output,
            );
            assert!(matches!(
                terminal,
                RetrievalOperationTerminal::Complete { .. }
            ));
        });
        assert_eq!(allocations.count_total, 0);
    });
}

fn boundary<'fixture>(
    fixture: &'fixture SealedFixture<'fixture>,
    cancellation: &'fixture Cancellation,
    selection: &'fixture [server_index_graph_vector::VectorSegmentDescriptor],
) -> RetrievalBoundary<'fixture, 'fixture, 'fixture, 'fixture, Box<[u8]>> {
    RetrievalBoundary::new(
        fixture.sealed,
        cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        selection,
    )
    .expect("valid boundary")
}

const fn hit(
    fixture: &SealedFixture<'_>,
    partition: PartitionId,
    entity: u32,
    score: i64,
) -> VectorHit {
    VectorHit {
        authority: fixture.vector_authority,
        partition,
        entity: EntityId::new(entity),
        score,
    }
}
