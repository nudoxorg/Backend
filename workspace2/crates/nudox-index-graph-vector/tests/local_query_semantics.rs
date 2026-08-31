use nudox_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphQueryError, GraphRow, Metric, ModelId, PartitionId,
    ProjectionId, TrustfallGraph, VectorAuthority, VectorFact, VectorQueryError, VectorRow,
    exact_vector_query,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

fn snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

fn graph_authority(byte: u8) -> GraphAuthority {
    GraphAuthority::new(snapshot(byte), ProjectionId::new(3))
}

#[test]
fn borrowed_graph_query_is_snapshot_pinned_deterministic_and_exactly_partial() {
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
    let rows = [GraphRow::new(first_partition, &edges)];
    let graph = TrustfallGraph::try_new(authority, &rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let mut output = [None; 2];
    let outcome = graph.neighbors(
        &[first_partition, absent_partition],
        EntityId::new(4),
        &mut output,
    );
    assert!(outcome.is_ok());
    let Ok(outcome) = outcome else {
        return;
    };
    assert_eq!(outcome.written(), 2);
    assert_eq!(output[0].map(|hit| hit.entity()), Some(EntityId::new(7)));
    assert_eq!(output[1].map(|hit| hit.entity()), Some(EntityId::new(9)));
    assert_eq!(output[0].map(|hit| hit.authority()), Some(authority));
    assert_eq!(outcome.terminal().authority(), authority);
    assert!(outcome.terminal().is_partial());
    assert_eq!(outcome.terminal().missing_len(), 1);
    assert_eq!(outcome.terminal().missing_at(0), Some(absent_partition));
}

#[test]
fn graph_output_capacity_is_preflighted_without_mutation() {
    let authority = graph_authority(2);
    let partition = PartitionId::new(0);
    let edges = [
        GraphEdge::new(authority, partition, EntityId::new(1), EntityId::new(2)),
        GraphEdge::new(authority, partition, EntityId::new(1), EntityId::new(3)),
    ];
    let rows = [GraphRow::new(partition, &edges)];
    let graph = TrustfallGraph::try_new(authority, &rows);
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
        VectorFact::new(authority, partition, EntityId::new(9), &coordinates_a),
        VectorFact::new(authority, partition, EntityId::new(7), &coordinates_b),
    ];
    let rows = [VectorRow::new(partition, &facts)];
    let mut output = [None; 2];
    let outcome = exact_vector_query(authority, &[partition], &rows, &[0, 0], 2, &mut output);
    assert!(outcome.is_ok());
    let Ok(outcome) = outcome else {
        return;
    };
    assert_eq!(output[0].map(|hit| hit.entity()), Some(EntityId::new(7)));
    assert_eq!(output[1].map(|hit| hit.entity()), Some(EntityId::new(9)));
    assert_eq!(output[0].map(|hit| hit.authority()), Some(authority));
    assert_eq!(outcome.written(), 2);
    assert_eq!(outcome.terminal().authority(), authority);
    assert!(!outcome.terminal().is_partial());
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
    let facts = [VectorFact::new(
        authority,
        partition,
        EntityId::new(1),
        &coordinates,
    )];
    let rows = [VectorRow::new(partition, &facts)];
    let mut output = [None; 1];
    assert_eq!(
        exact_vector_query(authority, &[partition], &rows, &[0], 1, &mut output),
        Err(VectorQueryError::QueryDimension {
            expected: 2,
            observed: 1,
        })
    );
    assert_eq!(output, [None]);
    assert_eq!(
        exact_vector_query(authority, &[partition], &rows, &[0, 0], 2, &mut output),
        Err(VectorQueryError::InsufficientOutput {
            required: 2,
            available: 1,
        })
    );
    assert_eq!(output, [None]);
}
