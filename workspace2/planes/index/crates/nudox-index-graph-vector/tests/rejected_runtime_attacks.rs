use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use nudox_index_graph_vector::{
    AdmissionError, Cancellation, EdgeBatchStream, GraphAuthority, GraphEdge, GraphRow,
    GraphStreamEvent, GraphTerminal, GraphTraceEvent, MAX_PARTITIONS, Metric, ModelId, PartitionId,
    ProjectionId, TraceProbe, TraceRecorder, TrustfallGraph, VectorAuthority, VectorTerminal,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;
use std::{sync::Arc, task::Wake};

fn snapshot(byte: u8) -> IndexSnapshotId {
    IndexSnapshotId::from_canonical_bytes(&[byte; 32])
}

fn graph_authority(byte: u8) -> GraphAuthority {
    GraphAuthority::new(snapshot(byte), ProjectionId::new(7))
}

#[derive(Debug)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn cancellation_owns_and_reaches_pending_wake_registration() {
    let authority = graph_authority(1);
    let cancellation = Cancellation::new();
    let stream = EdgeBatchStream::new(authority, 2, size_of::<GraphEdge>() * 2);
    assert!(stream.is_ok());
    let Ok(mut stream) = stream else {
        return;
    };
    let wake_count = Arc::new(WakeCount(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&wake_count));
    let mut context = Context::from_waker(&waker);

    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &cancellation),
        Poll::Pending
    ));
    cancellation.cancel();
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &cancellation),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled {
            authority: observed,
        })) if observed == authority
    ));
    assert_eq!(stream.charged_items(), 0);
}

#[test]
fn insufficient_output_retains_the_entire_leased_batch_for_retry() {
    let authority = graph_authority(2);
    let cancellation = Cancellation::new();
    let stream = EdgeBatchStream::new(authority, 2, size_of::<GraphEdge>() * 2);
    assert!(stream.is_ok());
    let Ok(mut stream) = stream else {
        return;
    };
    let edges = [
        GraphEdge::new(
            authority,
            PartitionId::new(0),
            EntityId::new(1),
            EntityId::new(2),
        ),
        GraphEdge::new(
            authority,
            PartitionId::new(0),
            EntityId::new(1),
            EntityId::new(3),
        ),
    ];
    assert_eq!(stream.settle(PartitionId::new(0), &edges), Ok(()));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &cancellation),
        Poll::Ready(GraphStreamEvent::Batch(_))
    ));
    let Poll::Ready(GraphStreamEvent::Batch(mut batch)) =
        Pin::new(&mut stream).poll_batch(&mut context, &cancellation)
    else {
        return;
    };
    let placeholder = GraphEdge::new(
        authority,
        PartitionId::new(1),
        EntityId::new(9),
        EntityId::new(9),
    );
    let mut short = [placeholder; 1];
    let error = batch.copy_into(&mut short);
    assert!(error.is_err());
    let Err(error) = error else {
        return;
    };
    assert_eq!((error.required(), error.available()), (2, 1));
    assert_eq!(batch.len(), 2);
    assert_eq!(short, [placeholder; 1]);
    let mut complete = [placeholder; 2];
    assert_eq!(batch.copy_into(&mut complete), Ok(2));
    assert_eq!(complete, edges);
}

#[test]
fn graph_and_vector_terminals_retain_distinct_authority() {
    let graph = graph_authority(3);
    let vector = VectorAuthority::new(
        snapshot(3),
        ModelId::new([9; 16]),
        3,
        Metric::SquaredEuclidean,
    );
    assert_eq!(
        GraphTerminal::Complete { authority: graph }.authority(),
        graph
    );
    assert_eq!(
        VectorTerminal::Complete { authority: vector }.authority(),
        vector
    );
}

#[test]
fn hostile_row_bound_fires_before_duplicate_work() {
    let authority = graph_authority(4);
    let edge = [GraphEdge::new(
        authority,
        PartitionId::new(0),
        EntityId::new(1),
        EntityId::new(2),
    )];
    let duplicate = GraphRow::new(PartitionId::new(0), &edge);
    let rows = [duplicate; MAX_PARTITIONS + 1];
    assert_eq!(
        TrustfallGraph::try_new(authority, &rows),
        Err(AdmissionError::TooManyRows {
            maximum: MAX_PARTITIONS,
            observed: MAX_PARTITIONS + 1,
        })
    );
}

#[test]
fn wrong_snapshot_edges_never_enter_the_trustfall_view() {
    let requested = graph_authority(5);
    let stale = graph_authority(6);
    let edge = [GraphEdge::new(
        stale,
        PartitionId::new(0),
        EntityId::new(1),
        EntityId::new(2),
    )];
    let rows = [GraphRow::new(PartitionId::new(0), &edge)];
    assert_eq!(
        TrustfallGraph::try_new(requested, &rows),
        Err(AdmissionError::WrongGraphAuthority {
            row_index: 0,
            edge_index: 0,
            expected: requested,
            observed: stale,
        })
    );
}

#[test]
fn disabled_trace_has_no_queue_and_enabled_trace_is_drainable() {
    let authority = graph_authority(7);
    let mut disabled = TraceProbe::disabled();
    let mut built = false;
    disabled.record_with(|| {
        built = true;
        GraphTraceEvent::Admitted {
            authority,
            partitions: 1,
        }
    });
    assert!(!built);
    assert_eq!(disabled.retained_event_capacity(), 0);

    let mut recorder = TraceRecorder::new();
    let mut enabled = TraceProbe::enabled(&mut recorder);
    enabled.record_with(|| GraphTraceEvent::Admitted {
        authority,
        partitions: 1,
    });
    let mut output = [None; 2];
    assert_eq!(enabled.drain_into(&mut output), 1);
    assert_eq!(
        output[0],
        Some(GraphTraceEvent::Admitted {
            authority,
            partitions: 1,
        })
    );
}
