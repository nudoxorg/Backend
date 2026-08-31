use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use nudox_index_graph_vector::{
    Cancellation, EdgeBatchProducer, EdgeBatchStream, GraphAuthority, GraphEdge, GraphStreamEvent,
    GraphTerminal, LeasedGraphBatch, PartitionId, ProjectionId, StreamCapacityError, TraceProbe,
};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

const CAPACITY_ONE: usize = 1;
const CAPACITY_TWO: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestPhase {
    Capacity,
    ReverseCompletion,
    DropConservation,
    TerminalFusion,
    Cancellation,
    ConcurrentCompletion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventKind {
    Pending,
    Batch,
    Complete,
    Partial,
    Cancelled,
    Fused,
}

#[derive(Debug, Eq, PartialEq)]
enum TestFailure {
    Admission(StreamCapacityError),
    UnexpectedEvent {
        phase: TestPhase,
        expected: EventKind,
        observed: EventKind,
    },
    ThreadPanicked {
        worker: Worker,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Worker {
    First,
    Second,
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

fn channel<'cancellation>(
    graph: GraphAuthority,
    selected: &[PartitionId],
    item_capacity: usize,
    cancellation: &'cancellation Cancellation,
    trace: &mut TraceProbe<'_>,
) -> Result<(EdgeBatchProducer, EdgeBatchStream<'cancellation>), TestFailure> {
    let byte_capacity = item_capacity * size_of::<GraphEdge>();
    EdgeBatchStream::channel(
        graph,
        selected,
        item_capacity,
        byte_capacity,
        cancellation,
        trace,
    )
    .map_err(TestFailure::Admission)
}

fn event_kind(event: &GraphStreamEvent<'_>) -> EventKind {
    match event {
        GraphStreamEvent::Batch(_) => EventKind::Batch,
        GraphStreamEvent::Terminal(GraphTerminal::Complete { .. }) => EventKind::Complete,
        GraphStreamEvent::Terminal(GraphTerminal::Partial { .. }) => EventKind::Partial,
        GraphStreamEvent::Terminal(GraphTerminal::Cancelled { .. }) => EventKind::Cancelled,
        GraphStreamEvent::Fused => EventKind::Fused,
    }
}

fn next_batch<'stream, 'cancellation>(
    stream: Pin<&'stream mut EdgeBatchStream<'cancellation>>,
    context: &mut Context<'_>,
    trace: &mut TraceProbe<'_>,
    phase: TestPhase,
) -> Result<LeasedGraphBatch<'stream>, TestFailure> {
    match stream.poll_batch(context, trace) {
        Poll::Ready(GraphStreamEvent::Batch(batch)) => Ok(batch),
        Poll::Pending => Err(TestFailure::UnexpectedEvent {
            phase,
            expected: EventKind::Batch,
            observed: EventKind::Pending,
        }),
        Poll::Ready(event) => Err(TestFailure::UnexpectedEvent {
            phase,
            expected: EventKind::Batch,
            observed: event_kind(&event),
        }),
    }
}

fn next_terminal<'cancellation>(
    stream: Pin<&mut EdgeBatchStream<'cancellation>>,
    context: &mut Context<'_>,
    trace: &mut TraceProbe<'_>,
    phase: TestPhase,
) -> Result<GraphTerminal, TestFailure> {
    match stream.poll_batch(context, trace) {
        Poll::Ready(GraphStreamEvent::Terminal(terminal)) => Ok(terminal),
        Poll::Pending => Err(TestFailure::UnexpectedEvent {
            phase,
            expected: EventKind::Partial,
            observed: EventKind::Pending,
        }),
        Poll::Ready(event) => Err(TestFailure::UnexpectedEvent {
            phase,
            expected: EventKind::Partial,
            observed: event_kind(&event),
        }),
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
        let (producer, stream) = channel(
            graph,
            &[partition],
            item_capacity,
            &cancellation,
            &mut trace,
        )?;
        let overfull = &edges[..item_capacity + CAPACITY_ONE];
        assert_eq!(
            producer.settle(partition, overfull),
            Err(StreamCapacityError::ItemCapacity {
                maximum: item_capacity,
                observed: overfull.len(),
            })
        );
        assert_eq!(stream.charged_items(), 0);
        assert_eq!(stream.charged_bytes(), 0);
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
    let (producer, mut stream) =
        channel(graph, &selected, CAPACITY_ONE, &cancellation, &mut trace)?;
    let noop = Waker::noop();
    let mut context = Context::from_waker(noop);

    let fourth_edge = [edge(graph, fourth, 40, 41)];
    assert_eq!(producer.settle(fourth, &fourth_edge), Ok(()));
    let mut fourth_batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    let mut fourth_output = [fourth_edge[0]];
    assert_eq!(
        fourth_batch.copy_into(&mut fourth_output),
        Ok(fourth_edge.len())
    );
    assert_eq!(fourth_output, fourth_edge);

    let second_edge = [edge(graph, second, 20, 21)];
    assert_eq!(producer.settle(second, &second_edge), Ok(()));
    let mut second_batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    let mut second_output = [second_edge[0]];
    assert_eq!(
        second_batch.copy_into(&mut second_output),
        Ok(second_edge.len())
    );
    assert_eq!(second_output, second_edge);

    assert_eq!(producer.finish(), Ok(()));
    let terminal = next_terminal(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ReverseCompletion,
    )?;
    assert_eq!(
        terminal,
        GraphTerminal::Partial {
            authority: graph,
            missing: [Some(first), Some(third), None, None],
            missing_len: CAPACITY_TWO,
        }
    );
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
    let (producer, mut stream) =
        channel(graph, &selected, CAPACITY_TWO, &cancellation, &mut trace)?;
    let first_edges = [edge(graph, first, 50, 51), edge(graph, first, 52, 53)];
    assert_eq!(producer.settle(first, &first_edges), Ok(()));
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
        assert_eq!(
            stream.charged_bytes(),
            size_of::<GraphEdge>() * CAPACITY_TWO
        );
        drop(batch);
    }
    assert_eq!(stream.charged_items(), 0);
    assert_eq!(stream.charged_bytes(), 0);
    assert_eq!(producer.poll_ready(&mut context), Poll::Ready(Ok(())));

    let second_edges = [edge(graph, second, 60, 61)];
    assert_eq!(producer.settle(second, &second_edges), Ok(()));
    let mut batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::DropConservation,
    )?;
    let mut output = [second_edges[0]; CAPACITY_ONE];
    assert_eq!(batch.copy_into(&mut output), Ok(second_edges.len()));
    assert_eq!(output, second_edges);
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
    let (producer, mut stream) =
        channel(graph, &[partition], CAPACITY_ONE, &cancellation, &mut trace)?;
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
    assert_eq!(stream.charged_items(), 0);
    assert_eq!(stream.charged_bytes(), 0);
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
    let (producer, mut stream) = channel(graph, &[partition], 0, &cancellation, &mut trace)?;
    assert_eq!(producer.finish(), Ok(()));
    let mut context = Context::from_waker(Waker::noop());
    assert_eq!(
        next_terminal(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::TerminalFusion,
        )?,
        GraphTerminal::Partial {
            authority: graph,
            missing: [Some(partition), None, None, None],
            missing_len: CAPACITY_ONE,
        }
    );
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
fn scoped_atomic_race_has_one_completion_winner_and_no_corrupt_batch() -> TestResult {
    let graph = authority(46);
    let partition = PartitionId::new(9);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let (producer, mut stream) =
        channel(graph, &[partition], CAPACITY_ONE, &cancellation, &mut trace)?;
    let arrived = AtomicUsize::new(0);
    let start = AtomicBool::new(false);
    let first_edge = edge(graph, partition, 90, 91);
    let second_edge = edge(graph, partition, 92, 93);

    let (first_result, second_result) = std::thread::scope(|scope| {
        let first_producer = &producer;
        let first_arrived = &arrived;
        let first_start = &start;
        let first = scope.spawn(move || {
            first_arrived.fetch_add(CAPACITY_ONE, Ordering::AcqRel);
            while !first_start.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            first_producer.settle(partition, &[first_edge])
        });

        let second_producer = &producer;
        let second_arrived = &arrived;
        let second_start = &start;
        let second = scope.spawn(move || {
            second_arrived.fetch_add(CAPACITY_ONE, Ordering::AcqRel);
            while !second_start.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            second_producer.settle(partition, &[second_edge])
        });

        while arrived.load(Ordering::Acquire) != CAPACITY_TWO {
            std::thread::yield_now();
        }
        start.store(true, Ordering::Release);
        let first_result = first.join().map_err(|_| TestFailure::ThreadPanicked {
            worker: Worker::First,
        })?;
        let second_result = second.join().map_err(|_| TestFailure::ThreadPanicked {
            worker: Worker::Second,
        })?;
        Ok::<_, TestFailure>((first_result, second_result))
    })?;

    let results = [first_result, second_result];
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        CAPACITY_ONE
    );
    assert!(results.iter().all(|result| {
        result.is_ok()
            || matches!(
                result,
                Err(StreamCapacityError::BatchAlreadySettled)
                    | Err(StreamCapacityError::PartitionAlreadySettled { partition: observed })
                    if *observed == partition
            )
    }));

    let mut context = Context::from_waker(Waker::noop());
    let mut batch = next_batch(
        Pin::new(&mut stream),
        &mut context,
        &mut trace,
        TestPhase::ConcurrentCompletion,
    )?;
    let placeholder = edge(graph, partition, 0, 0);
    let mut output = [placeholder];
    assert_eq!(batch.copy_into(&mut output), Ok(CAPACITY_ONE));
    assert!(output[0] == first_edge || output[0] == second_edge);
    assert_eq!(producer.finish(), Ok(()));
    assert_eq!(
        next_terminal(
            Pin::new(&mut stream),
            &mut context,
            &mut trace,
            TestPhase::ConcurrentCompletion,
        )?,
        GraphTerminal::Complete { authority: graph }
    );
    Ok(())
}
