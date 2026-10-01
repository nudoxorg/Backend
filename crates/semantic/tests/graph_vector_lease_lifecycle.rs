//! Exercises the `backend-semantic::graph_vector` tests lease-lifecycle contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::ir::EntityId;
use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Waker},
};
use backend_semantic::graph_vector::{
    Cancellation, EdgeBatchStream, GraphAuthority, GraphEdge, GraphLease, GraphStreamEvent,
    GraphTerminal, LeaseCapacity, LeasedGraphBatch, PartitionId, ProjectionId, StreamCapacityError,
    TraceProbe,
};
use backend_semantic::index_vocabulary::IndexSnapshotId;

const CAPACITY_ONE: usize = 1;
const CAPACITY_TWO: usize = 2;

#[derive(Debug, Eq, PartialEq)]
enum TestFailure {
    Admission(StreamCapacityError),
    CapacityConversion {
        source: core::num::TryFromIntError,
    },
    ExpectedBatch {
        phase: TestPhase,
    },
    ExpectedTerminal {
        phase: TestPhase,
    },
    ExpectedRejection {
        phase: TestPhase,
    },
    BatchMismatch {
        phase: TestPhase,
        observed: Option<GraphEdge>,
        observed_len: usize,
    },
    ThreadPanicked {
        worker: Worker,
        report: PanicReport,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum PanicReport {
    Static(&'static str),
    Owned(String),
    NonText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestPhase {
    ReverseCompletion,
    DropConservation,
    TerminalFusion,
    ProducerDisconnect,
    Cancellation,
    ConcurrentCompletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Worker {
    First,
}

type TestResult = Result<(), TestFailure>;

fn authority(seed: u8) -> GraphAuthority {
    GraphAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[seed; 32]),
        ProjectionId::new(17),
    )
}

fn edge(graph: GraphAuthority, partition: PartitionId, source: u32, target: u32) -> GraphEdge {
    GraphEdge::new(
        graph,
        partition,
        EntityId::new(source),
        EntityId::new(target),
    )
}

fn lease<'cancellation>(
    graph: GraphAuthority,
    selected: &[PartitionId],
    item_capacity: usize,
    cancellation: &'cancellation Cancellation,
    trace: &mut TraceProbe<'_>,
) -> Result<GraphLease<'cancellation>, TestFailure> {
    let edges_per_partition =
        u8::try_from(item_capacity).map_err(|source| TestFailure::CapacityConversion { source })?;
    let capacity = LeaseCapacity {
        edges_per_partition,
        bytes_per_partition: item_capacity * size_of::<GraphEdge>(),
    };
    GraphLease::new(graph, selected, capacity, cancellation, trace).map_err(TestFailure::Admission)
}

fn next_batch<'stream, 'lease, 'cancellation>(
    stream: Pin<&'stream mut EdgeBatchStream<'lease, 'cancellation>>,
    context: &mut Context<'_>,
    trace: &mut TraceProbe<'_>,
    phase: TestPhase,
) -> Result<LeasedGraphBatch<'stream>, TestFailure> {
    match stream.poll_batch(context, trace) {
        Poll::Ready(GraphStreamEvent::Batch(batch)) => Ok(batch),
        Poll::Pending | Poll::Ready(_) => Err(TestFailure::ExpectedBatch { phase }),
    }
}

fn next_terminal<'stream, 'lease, 'cancellation>(
    stream: Pin<&'stream mut EdgeBatchStream<'lease, 'cancellation>>,
    context: &mut Context<'_>,
    trace: &mut TraceProbe<'_>,
    phase: TestPhase,
) -> Result<GraphTerminal, TestFailure> {
    match stream.poll_batch(context, trace) {
        Poll::Ready(GraphStreamEvent::Terminal(terminal)) => Ok(terminal),
        Poll::Pending | Poll::Ready(_) => Err(TestFailure::ExpectedTerminal { phase }),
    }
}

fn verify_single_edge(
    batch: &[GraphEdge],
    expected: GraphEdge,
    phase: TestPhase,
) -> Result<(), TestFailure> {
    if batch == [expected] {
        Ok(())
    } else {
        Err(TestFailure::BatchMismatch {
            phase,
            observed: batch.first().copied(),
            observed_len: batch.len(),
        })
    }
}

#[test]
fn item_capacity_one_and_two_reject_overflow_without_charging() -> TestResult {
    let graph = authority(41);
    let partition = PartitionId::new(3);
    let edges = [
        edge(graph, partition, 1, 2),
        edge(graph, partition, 2, 3),
        edge(graph, partition, 3, 4),
    ];
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();

    for item_capacity in [CAPACITY_ONE, CAPACITY_TWO] {
        let lease = lease(
            graph,
            &[partition],
            item_capacity,
            &cancellation,
            &mut trace,
        )?;
        let (mut producer, stream) = lease.split().map_err(TestFailure::Admission)?;
        let overfull = &edges[..item_capacity + CAPACITY_ONE];
        assert_eq!(
            producer.settle(partition, overfull),
            Err(StreamCapacityError::ItemCapacity {
                maximum: item_capacity,
                observed: overfull.len(),
            })
        );
        assert_eq!(stream.load().edges, 0);
        assert_eq!(stream.load().bytes, 0);
        assert_eq!(
            producer.poll_ready(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(Ok(()))
        );
        drop(stream);
    }
    Ok(())
}

#[test]
fn reverse_completion_preserves_partition_identity_and_missing_order() -> TestResult {
    let graph = authority(42);
    let first = PartitionId::new(1);
    let second = PartitionId::new(2);
    let third = PartitionId::new(3);
    let fourth = PartitionId::new(4);
    let selected = [first, second, third, fourth];
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &selected, CAPACITY_ONE, &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    let noop = Waker::noop();
    let mut context = Context::from_waker(noop);

    let fourth_edge = [edge(graph, fourth, 40, 41)];
    assert_eq!(producer.settle(fourth, &fourth_edge), Ok(()));
    let second_edge = [edge(graph, second, 20, 21)];
    assert_eq!(producer.settle(second, &second_edge), Ok(()));
    let first_edge = [edge(graph, first, 10, 11)];
    assert_eq!(producer.settle(first, &first_edge), Ok(()));
    assert_eq!(producer.finish(), Ok(()));

    let first_batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    assert_eq!(first_batch.as_ref(), first_edge);
    drop(first_batch);

    let second_batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    assert_eq!(second_batch.as_ref(), second_edge);
    drop(second_batch);

    let fourth_batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    assert_eq!(fourth_batch.as_ref(), fourth_edge);
    drop(fourth_batch);

    let terminal = next_terminal(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    let GraphTerminal::Partial { authority, missing } = terminal else {
        return Err(TestFailure::ExpectedTerminal {
            phase: TestPhase::ReverseCompletion,
        });
    };
    assert_eq!(authority, graph);
    assert_eq!(missing.as_ref(), &[third]);
    Ok(())
}

#[test]
fn dropping_ready_batch_returns_both_credits_and_allows_next_partition() -> TestResult {
    let graph = authority(43);
    let first = PartitionId::new(5);
    let second = PartitionId::new(6);
    let selected = [first, second];
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &selected, CAPACITY_TWO, &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    let first_edges = [edge(graph, first, 50, 51), edge(graph, first, 52, 53)];
    assert_eq!(producer.settle(first, &first_edges), Ok(()));
    let retained = stream.load();
    assert_eq!(retained.edges, first_edges.len());
    assert_eq!(retained.bytes, size_of::<GraphEdge>() * CAPACITY_TWO);
    let noop = Waker::noop();
    let mut context = Context::from_waker(noop);
    {
        let batch = next_batch(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::DropConservation,
        )?;
        assert_eq!(batch.len(), first_edges.len());
        drop(batch);
    }
    let released = stream.load();
    assert_eq!(released.edges, 0);
    assert_eq!(released.bytes, 0);
    assert_eq!(producer.poll_ready(&mut context), Poll::Ready(Ok(())));

    let second_edges = [edge(graph, second, 60, 61)];
    assert_eq!(producer.settle(second, &second_edges), Ok(()));
    let batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::DropConservation,
    )?;
    assert_eq!(batch.as_ref(), second_edges);
    drop(batch);
    assert_eq!(producer.finish(), Ok(()));
    assert_eq!(
        next_terminal(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::DropConservation,
        )?,
        GraphTerminal::Complete { authority: graph }
    );
    Ok(())
}

#[test]
fn cancellation_releases_settled_batch_and_fuses() -> TestResult {
    let graph = authority(44);
    let partition = PartitionId::new(7);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], CAPACITY_ONE, &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    let settled = [edge(graph, partition, 70, 71)];
    assert_eq!(producer.settle(partition, &settled), Ok(()));
    cancellation.cancel();
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(
        next_terminal(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::Cancellation,
        )?,
        GraphTerminal::Cancelled { authority: graph }
    );
    assert_eq!(stream.load().edges, 0);
    assert_eq!(stream.load().bytes, 0);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Fused)
    ));
    assert_eq!(producer.finish(), Err(StreamCapacityError::StreamClosed));
    Ok(())
}

#[test]
fn terminal_rejects_reuse_and_then_stays_fused() -> TestResult {
    let graph = authority(45);
    let partition = PartitionId::new(8);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], 0, &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    match lease.split() {
        Err(StreamCapacityError::EndpointsAlreadyBorrowed) => {}
        Ok(_) | Err(_) => {
            return Err(TestFailure::ExpectedRejection {
                phase: TestPhase::TerminalFusion,
            });
        }
    }
    assert_eq!(producer.finish(), Ok(()));
    let mut context = Context::from_waker(Waker::noop());
    let terminal = next_terminal(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::TerminalFusion,
    )?;
    let GraphTerminal::Partial { authority, missing } = terminal else {
        return Err(TestFailure::ExpectedTerminal {
            phase: TestPhase::TerminalFusion,
        });
    };
    assert_eq!(authority, graph);
    assert_eq!(missing.as_ref(), &[partition]);
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Fused)
    ));
    assert_eq!(producer.finish(), Err(StreamCapacityError::StreamClosed));
    assert_eq!(
        producer.poll_ready(&mut context),
        Poll::Ready(Err(StreamCapacityError::StreamClosed))
    );
    Ok(())
}

#[test]
fn dropping_producer_publishes_typed_failure_and_fuses() -> TestResult {
    let graph = authority(47);
    let partition = PartitionId::new(10);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], 0, &cancellation, &mut trace)?;
    let (producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    drop(producer);

    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(
        next_terminal(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::ProducerDisconnect,
        )?,
        GraphTerminal::Failed {
            authority: graph,
            cause: StreamCapacityError::ProducerDisconnected,
        }
    );
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Fused)
    ));
    Ok(())
}

#[test]
fn scoped_atomic_race_has_one_completion_winner_and_no_corrupt_batch() -> TestResult {
    let graph = authority(46);
    let partition = PartitionId::new(9);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], CAPACITY_ONE, &cancellation, &mut trace)?;
    let (producer, mut stream) = lease.split().map_err(TestFailure::Admission)?;
    let start = AtomicBool::new(false);
    let settled = AtomicBool::new(false);
    let first_edge = edge(graph, partition, 90, 91);
    let worker_result = std::thread::scope(|scope| {
        let worker_start = &start;
        let worker_settled = &settled;
        let worker = scope.spawn(move || {
            while !worker_start.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            let mut producer = producer;
            let settled_result = producer.settle(partition, &[first_edge]);
            worker_settled.store(true, Ordering::Release);
            let finished_result = settled_result.and_then(|()| producer.finish());
            (settled_result, finished_result)
        });

        start.store(true, Ordering::Release);
        let mut observed_batch = false;
        while !settled.load(Ordering::Acquire) && !observed_batch {
            match Pin::new(&mut stream)
                .poll_batch(&mut Context::from_waker(Waker::noop()), &mut trace)
            {
                Poll::Pending => std::thread::yield_now(),
                Poll::Ready(GraphStreamEvent::Batch(batch)) => {
                    verify_single_edge(
                        batch.as_ref(),
                        first_edge,
                        TestPhase::ConcurrentCompletion,
                    )?;
                    drop(batch);
                    observed_batch = true;
                }
                Poll::Ready(GraphStreamEvent::Terminal(_) | GraphStreamEvent::Fused) => {
                    return Err(TestFailure::ExpectedBatch {
                        phase: TestPhase::ConcurrentCompletion,
                    });
                }
            }
        }
        match worker.join() {
            Ok(result) => Ok((result, observed_batch)),
            Err(panic) => {
                let report = if let Some(message) = panic.downcast_ref::<&'static str>() {
                    PanicReport::Static(message)
                } else if let Some(message) = panic.downcast_ref::<String>() {
                    PanicReport::Owned(message.clone())
                } else {
                    PanicReport::NonText
                };
                Err(TestFailure::ThreadPanicked {
                    worker: Worker::First,
                    report,
                })
            }
        }
    })?;
    assert_eq!(worker_result.0.0, Ok(()));
    assert_eq!(worker_result.0.1, Ok(()));

    let mut context = Context::from_waker(Waker::noop());
    if !worker_result.1 {
        let batch = next_batch(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::ConcurrentCompletion,
        )?;
        verify_single_edge(batch.as_ref(), first_edge, TestPhase::ConcurrentCompletion)?;
        drop(batch);
    }
    let terminal = next_terminal(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ConcurrentCompletion,
    )?;
    assert_eq!(terminal, GraphTerminal::Complete { authority: graph });
    Ok(())
}
