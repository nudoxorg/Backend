//! Exercises the `backend-semantic::graph_vector` tests local-query-semantics contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::ir::EntityId;
use backend_semantic::graph_vector::{
    GraphAuthority, GraphEdge, GraphQueryError, GraphRow, Metric, ModelId, PartitionId,
    ProjectionId, ValidatedGraphView, ValidatedVectorSegment, VectorAuthority, VectorHit,
    VectorPoint, VectorQueryError, VectorQueryTerminal, exact_vector_query,
};
use backend_semantic::index_vocabulary::IndexSnapshotId;

fn snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

fn graph_authority(byte: u8) -> GraphAuthority {
    GraphAuthority::new(snapshot(byte), ProjectionId::new(3))
}

#[test]
fn borrowed_graph_query_is_snapshot_pinned_deterministic_and_exactly_partial() -> Result<(), String>
{
    let authority = graph_authority(1);
    let first_partition = PartitionId::new(0);
    let absent_partition = PartitionId::new(1);
    let edges = [
        GraphEdge::new(
            authority,
            first_partition,
            EntityId::new(4),
            EntityId::new(9),
        ),
        GraphEdge::new(
            authority,
            first_partition,
            EntityId::new(4),
            EntityId::new(7),
        ),
    ];
    let rows = [GraphRow {
        partition: first_partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(authority, &rows)
        .map_err(|error| format!("valid graph rejected: {error:?}"))?;
    let mut output = [None; 2];
    let outcome = graph
        .neighbors(
            &[first_partition, absent_partition],
            EntityId::new(4),
            &mut output,
        )
        .map_err(|error| format!("valid graph query rejected: {error:?}"))?;
    assert_eq!(outcome.written, 2);
    assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(7)));
    assert_eq!(output[1].map(|hit| hit.entity), Some(EntityId::new(9)));
    assert_eq!(output[0].map(|hit| hit.authority), Some(authority));
    match outcome.terminal {
        backend_semantic::graph_vector::GraphQueryTerminal::Partial {
            authority: observed,
            missing,
        } => {
            assert_eq!(observed, authority);
            assert_eq!(&*missing, &[absent_partition]);
        }
        backend_semantic::graph_vector::GraphQueryTerminal::Complete { .. } => {
            return Err("an unavailable partition produced a complete terminal".to_owned());
        }
    }
    Ok(())
}

#[test]
fn graph_output_capacity_is_preflighted_without_mutation() {
    let authority = graph_authority(2);
    let partition = PartitionId::new(0);
    let edges = [
        GraphEdge::new(authority, partition, EntityId::new(1), EntityId::new(2)),
        GraphEdge::new(authority, partition, EntityId::new(1), EntityId::new(3)),
    ];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(authority, &rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let mut output = [None; 1];
    assert_eq!(
        graph.neighbors(&[partition], EntityId::new(1), &mut output),
        Err(GraphQueryError::InsufficientOutput {
            required: 2,
            available: 1,
        })
    );
    assert_eq!(output, [None]);
}

#[test]
fn local_vector_oracle_retains_model_metric_and_deterministic_ties() {
    let authority = VectorAuthority::new(
        snapshot(3),
        ModelId::new([5; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let partition = PartitionId::new(0);
    let coordinates_a = [0_i16, 1];
    let coordinates_b = [1_i16, 0];
    let facts = [
        VectorPoint::new(EntityId::new(7), &coordinates_b),
        VectorPoint::new(EntityId::new(9), &coordinates_a),
    ];
    let segment = ValidatedVectorSegment::try_new(authority, partition, &facts);
    assert!(segment.is_ok());
    let Ok(segment) = segment else {
        return;
    };
    let segments = [segment];
    let mut output = [None; 2];
    let outcome = exact_vector_query(authority, &[partition], &segments, &[0, 0], 2, &mut output);
    assert!(outcome.is_ok());
    let Ok(outcome) = outcome else {
        return;
    };
    assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(7)));
    assert_eq!(output[1].map(|hit| hit.entity), Some(EntityId::new(9)));
    assert_eq!(output[0].map(|hit| hit.authority), Some(authority));
    assert_eq!(outcome.written, 2);
    assert_eq!(
        outcome.terminal,
        backend_semantic::graph_vector::VectorQueryTerminal::Complete { authority }
    );
}

#[test]
fn vector_dimension_and_output_fail_before_ranking_or_mutation() {
    let authority = VectorAuthority::new(
        snapshot(4),
        ModelId::new([6; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let partition = PartitionId::new(0);
    let coordinates = [0_i16, 1];
    let facts = [VectorPoint::new(EntityId::new(1), &coordinates)];
    let segment = ValidatedVectorSegment::try_new(authority, partition, &facts);
    assert!(segment.is_ok());
    let Ok(segment) = segment else {
        return;
    };
    let segments = [segment];
    let mut output = [None; 1];
    assert_eq!(
        exact_vector_query(authority, &[partition], &segments, &[0], 1, &mut output),
        Err(VectorQueryError::QueryDimension {
            expected: 2,
            observed: 1,
        })
    );
    assert_eq!(output, [None]);
    assert_eq!(
        exact_vector_query(authority, &[partition], &segments, &[0, 0], 2, &mut output),
        Err(VectorQueryError::InsufficientOutput {
            required: 2,
            available: 1,
        })
    );
    assert_eq!(output, [None]);
}

#[test]
fn cross_authority_sealed_segment_rejects_before_overwriting_caller_output() {
    let authority = VectorAuthority::new(
        snapshot(5),
        ModelId::from([7; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let stale_authority = VectorAuthority {
        snapshot: snapshot(6),
        ..authority
    };
    let partition = PartitionId { raw: 0 };
    let points = [VectorPoint::new(EntityId::new(1), &[0_i16, 0])];
    let segment = ValidatedVectorSegment::try_new(stale_authority, partition, &points);
    assert!(segment.is_ok());
    let Ok(segment) = segment else {
        return;
    };
    let segments = [segment];
    let sentinel = VectorHit {
        authority,
        partition,
        entity: EntityId::new(99),
        score: -1,
    };
    let mut output = [Some(sentinel)];

    assert_eq!(
        exact_vector_query(authority, &[partition], &segments, &[0, 0], 1, &mut output),
        Err(VectorQueryError::WrongSegmentAuthority {
            segment_index: 0,
            expected: authority,
            observed: stale_authority,
        })
    );
    assert_eq!(output, [Some(sentinel)]);
}

#[test]
fn vector_query_distinguishes_available_empty_segments_and_partial_cardinality()
-> Result<(), String> {
    let authority = VectorAuthority::new(
        snapshot(6),
        ModelId::from([8; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let available = PartitionId::new(0);
    let empty = ValidatedVectorSegment::try_new(authority, available, &[])
        .map_err(|error| format!("empty segment rejected: {error:?}"))?;
    let segments = [empty];
    let mut output = [None; 1];
    assert_eq!(
        exact_vector_query(authority, &[available], &segments, &[0, 0], 1, &mut output),
        Ok(backend_semantic::graph_vector::VectorQueryOutcome {
            written: 0,
            terminal: VectorQueryTerminal::Complete { authority },
        })
    );

    for selected in [
        &[PartitionId::new(1)][..],
        &[PartitionId::new(1), PartitionId::new(2)][..],
        &[
            PartitionId::new(1),
            PartitionId::new(2),
            PartitionId::new(3),
        ][..],
        &[
            PartitionId::new(1),
            PartitionId::new(2),
            PartitionId::new(3),
            PartitionId::new(4),
        ][..],
    ] {
        let outcome = exact_vector_query(authority, selected, &[], &[0, 0], 1, &mut output)
            .map_err(|error| format!("missing segments rejected: {error:?}"))?;
        match outcome.terminal {
            VectorQueryTerminal::Partial { missing, .. } => assert_eq!(&*missing, selected),
            VectorQueryTerminal::Complete { .. } => {
                return Err("unavailable selected segments produced a complete terminal".to_owned());
            }
        }
    }
    Ok(())
}
