use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use nudox_index_graph_vector::{
    AdmissionError, Cancellation, EdgeBatchProducer, EdgeBatchStream, GraphAuthority, GraphEdge,
    GraphRow, GraphStreamEvent, GraphTerminal, GraphTraceEvent, MAX_PARTITIONS, Metric, ModelId,
    PartitionId, ProjectionId, TraceProbe, TraceRecorder, ValidatedGraphView, VectorAuthority,
    VectorTerminal,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;
use std::{
    sync::{Arc, mpsc},
    task::Wake,
    thread,
    time::Duration,
};

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

#[derive(Debug)]
struct ReentrantProducerWake {
    producer: Arc<EdgeBatchProducer>,
    wake_count: AtomicUsize,
}

impl Wake for ReentrantProducerWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_count.fetch_add(1, Ordering::SeqCst);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(
            self.producer.poll_ready(&mut context),
            Poll::Pending
        ));
    }
}

#[derive(Debug)]
struct ReentrantCreditWake {
    producer: Arc<EdgeBatchProducer>,
    wake_count: AtomicUsize,
}

impl Wake for ReentrantCreditWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_count.fetch_add(1, Ordering::SeqCst);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(
            self.producer.poll_ready(&mut context),
            Poll::Ready(Ok(()))
        ));
    }
}

#[test]
fn cancellation_owns_and_reaches_pending_wake_registration() {
    let authority = graph_authority(1);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let channel = EdgeBatchStream::channel(
        authority,
        &[PartitionId::new(0)],
        2,
        size_of::<GraphEdge>() * 2,
        &cancellation,
        &mut trace,
    );
    assert!(channel.is_ok());
    let Ok((_producer, mut stream)) = channel else {
        return;
    };
    let wake_count = Arc::new(WakeCount(AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&wake_count));
    let mut context = Context::from_waker(&waker);

    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Pending
    ));
    cancellation.cancel();
    assert_eq!(wake_count.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled {
            authority: observed,
        })) if observed == authority
    ));
    assert_eq!(stream.charged_items(), 0);
}

#[test]
fn completion_wake_reenters_producer_after_unlocking_completion_state() {
    let authority = graph_authority(9);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let channel = EdgeBatchStream::channel(
        authority,
        &[PartitionId::new(0)],
        1,
        size_of::<GraphEdge>(),
        &cancellation,
        &mut trace,
    );
    assert!(channel.is_ok());
    let Ok((producer, mut stream)) = channel else {
        return;
    };
    let producer = Arc::new(producer);
    let wake = Arc::new(ReentrantProducerWake {
        producer: Arc::clone(&producer),
        wake_count: AtomicUsize::new(0),
    });
    let waker = Waker::from(Arc::clone(&wake));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Pending
    ));

    let (settled, settled_result) = mpsc::channel();
    let completion = Arc::clone(&producer);
    let settle = thread::spawn(move || {
        let result = completion.settle(PartitionId::new(0), &[]);
        let _ = settled.send(result);
    });
    let result = settled_result.recv_timeout(Duration::from_secs(1));
    assert!(
        matches!(result, Ok(Ok(()))),
        "settle did not complete: {result:?}"
    );
    assert!(settle.join().is_ok());
    assert_eq!(wake.wake_count.load(Ordering::SeqCst), 1);
}

#[test]
fn batch_release_wake_reenters_ready_producer_after_unlock() {
    let (completed, completion) = mpsc::channel();
    let worker = thread::spawn(move || {
        let authority = graph_authority(10);
        let cancellation = Cancellation::new();
        let mut trace = TraceProbe::disabled();
        let channel = EdgeBatchStream::channel(
            authority,
            &[PartitionId::new(0)],
            1,
            size_of::<GraphEdge>(),
            &cancellation,
            &mut trace,
        );
        assert!(channel.is_ok());
        let Ok((producer, mut stream)) = channel else {
            return;
        };
        let producer = Arc::new(producer);
        let edges = [GraphEdge::new(
            authority,
            PartitionId::new(0),
            EntityId::new(1),
            EntityId::new(2),
        )];
        assert_eq!(producer.settle(PartitionId::new(0), &edges), Ok(()));
        let wake = Arc::new(ReentrantCreditWake {
            producer: Arc::clone(&producer),
            wake_count: AtomicUsize::new(0),
        });
        let waker = Waker::from(Arc::clone(&wake));
        let mut context = Context::from_waker(&waker);
        assert!(matches!(producer.poll_ready(&mut context), Poll::Pending));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let Poll::Ready(GraphStreamEvent::Batch(mut batch)) =
            Pin::new(&mut stream).poll_batch(&mut context, &mut trace)
        else {
            return;
        };
        let mut output = [GraphEdge::new(
            authority,
            PartitionId::new(0),
            EntityId::new(0),
            EntityId::new(0),
        )];
        let copied = batch.copy_into(&mut output);
        let _ = completed.send((copied, wake.wake_count.load(Ordering::SeqCst)));
    });

    let result = completion.recv_timeout(Duration::from_secs(1));
    assert!(
        matches!(result, Ok((Ok(1), 1))),
        "batch release did not complete after a reentrant wake: {result:?}"
    );
    assert!(worker.join().is_ok());
}

#[test]
fn insufficient_output_retains_the_entire_leased_batch_for_retry() {
    let authority = graph_authority(2);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let channel = EdgeBatchStream::channel(
        authority,
        &[PartitionId::new(0)],
        2,
        size_of::<GraphEdge>() * 2,
        &cancellation,
        &mut trace,
    );
    assert!(channel.is_ok());
    let Ok((producer, mut stream)) = channel else {
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
    assert_eq!(producer.settle(PartitionId::new(0), &edges), Ok(()));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Batch(_))
    ));
    let Poll::Ready(GraphStreamEvent::Batch(mut batch)) =
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace)
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
    let duplicate = GraphRow {
        partition: PartitionId::new(0),
        edges: &edge,
    };
    let rows = [duplicate; MAX_PARTITIONS + 1];
    assert_eq!(
        ValidatedGraphView::try_new(authority, &rows),
        Err(AdmissionError::TooManyRows {
            maximum: MAX_PARTITIONS,
            observed: MAX_PARTITIONS + 1,
        })
    );
}

#[test]
fn wrong_snapshot_edges_never_enter_the_validated_view() {
    let requested = graph_authority(5);
    let stale = graph_authority(6);
    let edge = [GraphEdge::new(
        stale,
        PartitionId::new(0),
        EntityId::new(1),
        EntityId::new(2),
    )];
    let rows = [GraphRow {
        partition: PartitionId::new(0),
        edges: &edge,
    }];
    assert_eq!(
        ValidatedGraphView::try_new(requested, &rows),
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
    let cancellation = Cancellation::new();
    let present = PartitionId::new(0);
    let missing = PartitionId::new(1);
    let channel = EdgeBatchStream::channel(
        authority,
        &[present, missing],
        1,
        size_of::<GraphEdge>(),
        &cancellation,
        &mut enabled,
    );
    assert!(channel.is_ok());
    let Ok((producer, mut stream)) = channel else {
        return;
    };
    let edges = [GraphEdge::new(
        authority,
        present,
        EntityId::new(1),
        EntityId::new(2),
    )];
    assert_eq!(producer.settle(present, &edges), Ok(()));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    {
        let event = Pin::new(&mut stream).poll_batch(&mut context, &mut enabled);
        let Poll::Ready(GraphStreamEvent::Batch(mut batch)) = event else {
            return;
        };
        let mut copied = [edges[0]];
        assert_eq!(batch.copy_into(&mut copied), Ok(1));
        assert_eq!(copied, edges);
    }
    assert_eq!(producer.finish_partial(&[missing]), Ok(()));
    let terminal = Pin::new(&mut stream).poll_batch(&mut context, &mut enabled);
    let Poll::Ready(GraphStreamEvent::Terminal(terminal)) = terminal else {
        return;
    };
    assert_eq!(terminal.authority(), authority);
    assert_eq!(terminal.missing(), &[Some(missing)]);

    let mut output = [None; 4];
    assert_eq!(enabled.drain_into(&mut output), 3);
    assert_eq!(
        output[0],
        Some(GraphTraceEvent::Admitted {
            authority,
            partitions: 2,
        })
    );
    assert_eq!(
        output[1],
        Some(GraphTraceEvent::BatchReady {
            authority,
            edges: 1,
        })
    );
    assert_eq!(
        output[2],
        Some(GraphTraceEvent::Terminal {
            authority,
            cancelled: false,
        })
    );
}

#[test]
fn terminal_absence_is_derived_from_delivered_partition_accounting() {
    let authority = graph_authority(8);
    let partition = PartitionId::new(2);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let channel =
        EdgeBatchStream::channel(authority, &[partition], 0, 0, &cancellation, &mut trace);
    assert!(channel.is_ok());
    let Ok((producer, mut stream)) = channel else {
        return;
    };
    assert_eq!(producer.finish(), Ok(()));
    let waker = Waker::noop();
    let mut context = Context::from_waker(waker);
    let terminal = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
    let Poll::Ready(GraphStreamEvent::Terminal(terminal)) = terminal else {
        return;
    };
    assert_eq!(terminal.missing(), &[Some(partition)]);

    let second_cancellation = Cancellation::new();
    let second = EdgeBatchStream::channel(
        authority,
        &[partition],
        0,
        0,
        &second_cancellation,
        &mut trace,
    );
    assert!(second.is_ok());
    let Ok((second_producer, mut second_stream)) = second else {
        return;
    };
    assert_eq!(second_producer.settle(partition, &[]), Ok(()));
    {
        let event = Pin::new(&mut second_stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(mut batch)) = event else {
            return;
        };
        assert_eq!(batch.copy_into(&mut []), Ok(0));
    }
    assert_eq!(
        second_producer.finish_partial(&[partition]),
        Err(
            nudox_index_graph_vector::StreamCapacityError::IncorrectMissingPartitions {
                expected: None,
                observed: Some(partition),
                index: 0,
            }
        )
    );
}
