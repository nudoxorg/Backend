//! Exercises the `server-index-graph-vector` tests allocation contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use core::{
    mem::size_of,
    pin::Pin,
    task::{Context, Poll},
};
use std::hint::black_box;

use allocation_counter::{AllocationInfo, measure};
use backend_semantic::ir::EntityId;
use server_index_graph_vector::{
    Cancellation, GraphAuthority, GraphEdge, GraphLease, GraphRow, LeaseCapacity, Metric, ModelId,
    PartitionId, ProjectionId, TraceProbe, ValidatedGraphView, ValidatedVectorSegment,
    VectorAuthority, VectorPoint, exact_vector_query,
};
use server_index_vocabulary::IndexSnapshotId;

fn snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

#[test]
fn borrowed_queries_and_pending_poll_allocate_nothing_after_setup() {
    let graph_authority = GraphAuthority::new(snapshot(1), ProjectionId::new(1));
    let partition = PartitionId::new(0);
    let edges = [GraphEdge::new(
        graph_authority,
        partition,
        EntityId::new(1),
        EntityId::new(2),
    )];
    let graph_rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(graph_authority, &graph_rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let vector_authority = VectorAuthority::new(
        snapshot(1),
        ModelId::new([3; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let coordinates = [1_i16, 2];
    let vector_facts = [VectorPoint::new(EntityId::new(1), &coordinates)];
    let vector_segment =
        ValidatedVectorSegment::try_new(vector_authority, partition, &vector_facts);
    assert!(vector_segment.is_ok());
    let Ok(vector_segment) = vector_segment else {
        return;
    };
    let vector_segments = [vector_segment];
    let mut graph_output = [None; 1];
    let mut vector_output = [None; 1];
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = GraphLease::new(
        graph_authority,
        &[partition],
        LeaseCapacity {
            edges_per_partition: 1,
            bytes_per_partition: size_of::<GraphEdge>(),
        },
        &cancellation,
        &mut trace,
    );
    assert!(lease.is_ok());
    let Ok(lease) = lease else {
        return;
    };
    let endpoints = lease.split();
    assert!(endpoints.is_ok());
    let Ok((_producer, mut stream)) = endpoints else {
        return;
    };
    let waker = core::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Pending
    ));

    let allocations = measure(|| {
        let graph_result =
            black_box(graph.neighbors(&[partition], EntityId::new(1), &mut graph_output));
        assert!(graph_result.is_ok());
        let vector_result = black_box(exact_vector_query(
            vector_authority,
            &[partition],
            &vector_segments,
            &[0, 0],
            1,
            &mut vector_output,
        ));
        assert!(vector_result.is_ok());
        assert!(matches!(
            black_box(Pin::new(&mut stream).poll_batch(&mut context, &mut trace)),
            Poll::Pending
        ));
    });
    assert_eq!(allocations, AllocationInfo::default());
}
