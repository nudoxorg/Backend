use core::mem::{align_of, size_of};
use std::hint::black_box;

use allocation_counter::{AllocationInfo, measure};
use nudox_index_graph_vector::{
    Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority, VectorFact, VectorPoint,
    VectorQueryTerminal, VectorSegmentError, compact_vector_facts,
};
use nudox_index_vocab::{IndexSnapshotId, VectorSegmentId};
use nudox_ir_vocab::EntityId;

const MAX_SEGMENT_POINTS: usize = 16;

fn authority(seed: u8) -> VectorAuthority {
    VectorAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[seed; 32]),
        ModelId::new([seed.wrapping_add(1); 16]),
        2,
        Metric::SquaredEuclidean,
    )
}

static FIRST_COORDINATES: [i16; 2] = [1, 2];
static SECOND_COORDINATES: [i16; 2] = [3, 4];

fn points() -> [VectorPoint<'static>; 2] {
    [
        VectorPoint::new(EntityId::new(1), &FIRST_COORDINATES),
        VectorPoint::new(EntityId::new(2), &SECOND_COORDINATES),
    ]
}

#[test]
fn compact_point_and_segment_layout_beats_repeated_ingress_authority() {
    assert_eq!(size_of::<VectorFact<'static>>(), 80);
    assert_eq!(align_of::<VectorFact<'static>>(), 8);
    assert_eq!(size_of::<VectorPoint<'static>>(), 24);
    assert_eq!(align_of::<VectorPoint<'static>>(), 8);
    assert_eq!(size_of::<ValidatedVectorSegment<'static>>(), 104);
    assert_eq!(align_of::<ValidatedVectorSegment<'static>>(), 8);

    let old_point_lane = size_of::<VectorFact<'static>>() * MAX_SEGMENT_POINTS;
    let compact_point_lane = size_of::<VectorPoint<'static>>() * MAX_SEGMENT_POINTS;
    let old_segment_total = size_of::<ValidatedVectorSegment<'static>>() + old_point_lane;
    let compact_segment_total = size_of::<ValidatedVectorSegment<'static>>() + compact_point_lane;
    assert_eq!(old_point_lane, 1_280);
    assert_eq!(compact_point_lane, 384);
    assert_eq!(old_segment_total, 1_384);
    assert_eq!(compact_segment_total, 488);
    assert!(compact_segment_total < old_segment_total);
}

#[test]
fn canonical_identity_is_streamed_and_private_proof_lends_exact_points() -> Result<(), String> {
    let authority = authority(1);
    let partition = PartitionId::new(3);
    let points = points();
    let segment = ValidatedVectorSegment::try_new(authority, partition, &points);
    const GOLDEN: [u8; 32] = [
        0xc5, 0xe7, 0xa5, 0xb2, 0x11, 0x3b, 0x32, 0xc0, 0x3f, 0x64, 0xc1, 0xf2, 0x9a, 0x90, 0x15,
        0x68, 0xc5, 0x29, 0xe9, 0x35, 0x77, 0xb2, 0xbc, 0x84, 0x82, 0x4c, 0x62, 0x4c, 0xc4, 0x37,
        0xb4, 0x00,
    ];
    assert_eq!(
        segment.map(|segment| segment.id),
        Ok(VectorSegmentId::from_digest(GOLDEN))
    );
    let borrowed = ValidatedVectorSegment::try_new(authority, partition, &points)
        .map_err(|error| format!("valid points rejected: {error:?}"))?;
    assert_eq!(borrowed.authority, authority);
    assert_eq!(borrowed.partition, partition);
    assert_eq!(borrowed.as_ref(), points.as_slice());

    let permutation = [points[1], points[0]];
    assert_eq!(
        ValidatedVectorSegment::try_new(authority, partition, &permutation),
        Err(VectorSegmentError::OutOfOrder {
            index: 1,
            previous: EntityId::new(2),
            observed: EntityId::new(1),
        })
    );
    Ok(())
}

#[test]
fn duplicate_and_coordinate_mutations_cannot_reuse_identity() {
    let authority = authority(2);
    let partition = PartitionId::new(4);
    let points = points();
    assert_eq!(
        ValidatedVectorSegment::try_new(authority, partition, &[points[0], points[0]]),
        Err(VectorSegmentError::DuplicateEntity {
            first_index: 0,
            index: 1,
            entity: EntityId::new(1),
        })
    );

    static CHANGED_COORDINATES: [i16; 2] = [3, 5];
    let changed = [
        points[0],
        VectorPoint::new(EntityId::new(2), &CHANGED_COORDINATES),
    ];
    let original = ValidatedVectorSegment::try_new(authority, partition, &points);
    let changed = ValidatedVectorSegment::try_new(authority, partition, &changed);
    assert_ne!(
        original.map(|segment| segment.id),
        changed.map(|segment| segment.id)
    );
}

#[test]
fn raw_ingress_compacts_a_whole_batch_or_preserves_the_caller_buffer() {
    let expected_authority = authority(3);
    let partition = PartitionId::new(5);
    let raw_coordinates = [1_i16, 2];
    let sentinel = VectorPoint::new(EntityId::new(99), &raw_coordinates);

    let stale_authority = authority(4);
    let stale = VectorFact {
        authority: stale_authority,
        partition,
        entity: EntityId::new(2),
        coordinates: &raw_coordinates,
    };
    let accepted = VectorFact {
        authority: expected_authority,
        partition,
        entity: EntityId::new(1),
        coordinates: &raw_coordinates,
    };
    let mut output = [sentinel; 2];
    assert_eq!(
        compact_vector_facts(
            expected_authority,
            partition,
            &[accepted, stale],
            &mut output
        ),
        Err(VectorSegmentError::WrongAuthority {
            index: 1,
            expected: expected_authority,
            observed: stale_authority,
        })
    );
    assert_eq!(output, [sentinel; 2]);

    let wrong_partition = VectorFact {
        authority: expected_authority,
        partition: PartitionId::new(6),
        entity: EntityId::new(1),
        coordinates: &raw_coordinates,
    };
    assert_eq!(
        compact_vector_facts(
            expected_authority,
            partition,
            &[wrong_partition],
            &mut output
        ),
        Err(VectorSegmentError::WrongPartition {
            index: 0,
            expected: partition,
            observed: PartitionId::new(6),
        })
    );
    assert_eq!(output, [sentinel; 2]);

    let wrong_dimension = VectorFact {
        authority: expected_authority,
        partition,
        entity: EntityId::new(1),
        coordinates: &[0_i16],
    };
    assert_eq!(
        compact_vector_facts(
            expected_authority,
            partition,
            &[wrong_dimension],
            &mut output
        ),
        Err(VectorSegmentError::FactDimension {
            index: 0,
            expected: 2,
            observed: 1,
        })
    );
    assert_eq!(output, [sentinel; 2]);

    assert_eq!(
        compact_vector_facts(expected_authority, partition, &[accepted], &mut []),
        Err(VectorSegmentError::InsufficientPointOutput {
            required: 1,
            available: 0,
        })
    );

    let compacted = compact_vector_facts(expected_authority, partition, &[accepted], &mut output);
    assert_eq!(compacted, Ok(1),);
    assert_eq!(
        output[0],
        VectorPoint::new(EntityId::new(1), &raw_coordinates)
    );
    assert!(ValidatedVectorSegment::try_new(expected_authority, partition, &output[..1]).is_ok());
}

#[test]
fn empty_segment_is_valid_but_authority_and_partition_are_identity_inputs() {
    let segment_authority = authority(5);
    let first = ValidatedVectorSegment::try_new(segment_authority, PartitionId::new(0), &[]);
    let second = ValidatedVectorSegment::try_new(segment_authority, PartitionId::new(1), &[]);
    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_ne!(
        first.map(|segment| segment.id),
        second.map(|segment| segment.id)
    );
    let cross_authority = ValidatedVectorSegment::try_new(authority(7), PartitionId::new(0), &[]);
    assert_ne!(
        first.map(|segment| segment.id),
        cross_authority.map(|segment| segment.id)
    );

    let invalid = VectorAuthority::new(
        segment_authority.snapshot,
        segment_authority.model,
        0,
        segment_authority.metric,
    );
    assert_eq!(
        ValidatedVectorSegment::try_new(invalid, PartitionId::new(0), &[]),
        Err(VectorSegmentError::AuthorityDimension {
            maximum: 16,
            observed: 0,
        })
    );
}

#[test]
fn segment_construction_allocates_nothing_after_warmup() {
    let authority = authority(6);
    let partition = PartitionId::new(0);
    let points = points();
    assert!(ValidatedVectorSegment::try_new(authority, partition, &points).is_ok());
    let allocations = measure(|| {
        let result = black_box(ValidatedVectorSegment::try_new(
            authority, partition, &points,
        ));
        assert!(result.is_ok());
    });
    assert_eq!(allocations, AllocationInfo::default());
}

#[test]
fn segment_fact_bound_is_checked_before_order_or_hashing() {
    let authority = authority(8);
    let points = [VectorPoint::new(EntityId::new(1), &[0_i16, 0]); 17];
    assert_eq!(
        ValidatedVectorSegment::try_new(authority, PartitionId::new(0), &points),
        Err(VectorSegmentError::TooManyFacts {
            maximum: MAX_SEGMENT_POINTS,
            observed: 17,
        })
    );
}

#[test]
fn closed_vector_terminal_is_smaller_than_array_and_length_candidate() {
    #[derive(Clone, Copy)]
    struct LegacyVectorQueryTerminal {
        authority: VectorAuthority,
        missing: [Option<PartitionId>; 4],
        missing_len: usize,
    }

    let legacy = LegacyVectorQueryTerminal {
        authority: authority(0),
        missing: [None; 4],
        missing_len: 0,
    };
    assert_eq!(legacy.authority, authority(0));
    assert_eq!(legacy.missing, [None; 4]);
    assert_eq!(legacy.missing_len, 0);
    black_box(legacy);
    assert_eq!(size_of::<LegacyVectorQueryTerminal>(), 80);
    assert_eq!(align_of::<LegacyVectorQueryTerminal>(), 8);
    assert_eq!(size_of::<VectorQueryTerminal>(), 62);
    assert_eq!(align_of::<VectorQueryTerminal>(), 2);
}

#[test]
fn vector_terminal_reports_missing_coordinates_without_sentinel_indexing() {
    let authority = authority(7);
    let complete = VectorQueryTerminal::Complete { authority };
    assert!(!complete.is_partial());
    assert_eq!(complete.missing(), &[]);
}
