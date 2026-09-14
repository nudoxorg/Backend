//! Exercises the `backend-extension-trustfall` tests graph-query contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::ir::EntityId;
use core::{
    mem::size_of,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use server_index_graph_vector::{
    AdmissionError, Cancellation, GraphAuthority, GraphEdge, GraphLease, GraphRow,
    GraphStreamEvent, GraphTerminal, LeaseCapacity, PartitionId, ProjectionId, StreamCapacityError,
    TraceProbe, ValidatedGraphView,
};
use backend_extension_trustfall::server::{TrustfallGraph, TrustfallGraphError, TrustfallHit};
use server_index_vocabulary::IndexSnapshotId;

fn graph_authority() -> GraphAuthority {
    GraphAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"trustfall-pinned-graph-snapshot"),
        ProjectionId::new(9),
    )
}

#[derive(Debug)]
enum LeasedTrustfallError {
    Admission(AdmissionError),
    ExpectedBatch,
    Stream(StreamCapacityError),
    Trustfall(TrustfallGraphError),
}

impl core::fmt::Display for LeasedTrustfallError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Admission(cause) => write!(formatter, "graph admission failed: {cause:?}"),
            Self::ExpectedBatch => formatter.write_str("lease did not yield its admitted batch"),
            Self::Stream(cause) => write!(formatter, "graph lease failed: {cause:?}"),
            Self::Trustfall(cause) => write!(formatter, "Trustfall query failed: {cause}"),
        }
    }
}

impl std::error::Error for LeasedTrustfallError {}

#[test]
fn trustfall_borrows_only_a_batch_admitted_through_the_cancellable_lease()
-> Result<(), LeasedTrustfallError> {
    let authority = graph_authority();
    let partition = PartitionId::new(8);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = GraphLease::new(
        authority,
        &[partition],
        LeaseCapacity {
            edges_per_partition: 1,
            bytes_per_partition: size_of::<GraphEdge>(),
        },
        &cancellation,
        &mut trace,
    )
    .map_err(LeasedTrustfallError::Stream)?;
    let (mut producer, mut stream) = lease.split().map_err(LeasedTrustfallError::Stream)?;
    let edges = [GraphEdge::new(
        authority,
        partition,
        EntityId::new(5),
        EntityId::new(13),
    )];
    producer
        .settle(partition, &edges)
        .and_then(|()| producer.finish())
        .map_err(LeasedTrustfallError::Stream)?;
    let mut context = Context::from_waker(Waker::noop());

    {
        let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(batch)) = event else {
            return Err(LeasedTrustfallError::ExpectedBatch);
        };
        let rows = [GraphRow {
            partition,
            edges: batch.as_ref(),
        }];
        let view = ValidatedGraphView::try_new(authority, &rows)
            .map_err(LeasedTrustfallError::Admission)?;
        let graph = TrustfallGraph::new(&view);
        let mut output = [None];
        let terminal = graph
            .neighbors(EntityId::new(5), &mut output)
            .map_err(LeasedTrustfallError::Trustfall)?;
        assert_eq!(terminal.authority, authority);
        assert_eq!(terminal.written, 1);
        assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(13)));
    }

    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Complete { authority: observed }))
            if observed == authority
    ));
    Ok(())
}

#[test]
fn synchronous_trustfall_reads_only_the_pinned_validated_graph_view() {
    let authority = graph_authority();
    let first = PartitionId::new(1);
    let second = PartitionId::new(2);
    let first_edges = [
        GraphEdge::new(authority, first, EntityId::new(4), EntityId::new(7)),
        GraphEdge::new(authority, first, EntityId::new(4), EntityId::new(9)),
    ];
    let second_edges = [GraphEdge::new(
        authority,
        second,
        EntityId::new(4),
        EntityId::new(11),
    )];
    let rows = [
        GraphRow {
            partition: first,
            edges: &first_edges,
        },
        GraphRow {
            partition: second,
            edges: &second_edges,
        },
    ];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let mut output = [None; 3];

    let terminal = graph
        .neighbors(EntityId::new(4), &mut output)
        .expect("static Trustfall query must execute");

    assert_eq!(terminal.authority, authority);
    assert_eq!(terminal.written, 3);
    assert_eq!(
        output.map(|hit| hit.map(|hit| (hit.entity, hit.partition))),
        [
            Some((EntityId::new(7), first)),
            Some((EntityId::new(9), first)),
            Some((EntityId::new(11), second)),
        ]
    );
}

#[test]
fn synchronous_trustfall_preserves_source_isolation_and_full_entity_identity() {
    let authority = graph_authority();
    let partition = PartitionId::new(3);
    let high_source = EntityId::new(u32::MAX);
    let edges = [
        GraphEdge::new(authority, partition, high_source, EntityId::new(5)),
        GraphEdge::new(authority, partition, EntityId::new(4), EntityId::new(6)),
    ];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let mut output = [Some(TrustfallHit {
        authority,
        partition,
        entity: EntityId::new(0),
    })];

    let terminal = graph
        .neighbors(high_source, &mut output)
        .expect("full u32 source identity must survive Trustfall filters");

    assert_eq!(terminal.written, 1);
    assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(5)));
}

#[test]
fn insufficient_output_retains_caller_slots_before_trustfall_execution() {
    let authority = graph_authority();
    let partition = PartitionId::new(4);
    let edges = [
        GraphEdge::new(authority, partition, EntityId::new(8), EntityId::new(1)),
        GraphEdge::new(authority, partition, EntityId::new(8), EntityId::new(2)),
    ];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let sentinel = TrustfallHit {
        authority,
        partition,
        entity: EntityId::new(99),
    };
    let mut output = [Some(sentinel)];

    let rejected = graph
        .neighbors(EntityId::new(8), &mut output)
        .expect_err("capacity must be checked before a query can mutate output");

    assert_eq!(
        rejected,
        backend_extension_trustfall::server::TrustfallGraphError::InsufficientOutput {
            required: 2,
            available: 1,
        }
    );
    assert_eq!(output, [Some(sentinel)]);
}
