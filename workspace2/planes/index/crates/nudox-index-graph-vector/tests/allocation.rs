use core::{
    mem::size_of,
    pin::Pin,
    task::{Context, Poll},
};
use std::hint::black_box;

use allocation_counter::{AllocationInfo, measure};
use nudox_index_graph_vector::{
    Cancellation, EdgeBatchStream, GraphAuthority, GraphEdge, GraphRow, Metric, ModelId,
    PartitionId, ProjectionId, TrustfallGraph, VectorAuthority, VectorFact, VectorRow,
    exact_vector_query,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

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
    let graph_rows = [GraphRow::new(partition, &edges)];
    let graph = TrustfallGraph::try_new(graph_authority, &graph_rows);
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
    let vector_facts = [VectorFact::new(
        vector_authority,
        partition,
        EntityId::new(1),
        &coordinates,
    )];
    let vector_rows = [VectorRow::new(partition, &vector_facts)];
    let mut graph_output = [None; 1];
    let mut vector_output = [None; 1];
    let stream = EdgeBatchStream::new(graph_authority, 1, size_of::<GraphEdge>());
    assert!(stream.is_ok());
    let Ok(mut stream) = stream else {
        return;
    };
    let cancellation = Cancellation::new();
    let waker = core::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &cancellation),
        Poll::Pending
    ));

    let allocations = measure(|| {
        let graph_result = black_box(graph.neighbors(
            &[partition],
            EntityId::new(1),
            &mut graph_output,
        ));
        assert!(graph_result.is_ok());
        let vector_result = black_box(exact_vector_query(
            vector_authority,
            &[partition],
            &vector_rows,
            &[0, 0],
            1,
            &mut vector_output,
        ));
        assert!(vector_result.is_ok());
        assert!(matches!(
            black_box(Pin::new(&mut stream).poll_batch(&mut context, &cancellation)),
            Poll::Pending
        ));
    });
    assert_eq!(allocations, AllocationInfo::default());
}
