//! Exercises the `server-index-graph-vector` tests rejected-runtime-attacks contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use compiler_ir::EntityId;
use core::{
    mem::size_of,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};
use server_index_graph_vector::{
    Cancellation, GraphAuthority, GraphDegradation, GraphEdge, GraphLease, GraphStreamEvent,
    GraphTerminal, LeaseCapacity, PartitionId, ProjectionId, StreamCapacityError, TraceProbe,
};
use server_index_vocabulary::IndexSnapshotId;
use std::{sync::Arc, task::Wake, thread};

fn authority(byte: u8) -> GraphAuthority {
    GraphAuthority::new(
        IndexSnapshotId::from_canonical_bytes(&[byte; 32]),
        ProjectionId::new(11),
    )
}

fn edge(authority: GraphAuthority, partition: PartitionId, source: u32, target: u32) -> GraphEdge {
    GraphEdge::new(
        authority,
        partition,
        EntityId::new(source),
        EntityId::new(target),
    )
}

fn capacity(edges: u8) -> LeaseCapacity {
    LeaseCapacity {
        edges_per_partition: edges,
        bytes_per_partition: usize::from(edges) * size_of::<GraphEdge>(),
    }
}

fn lease<'cancel>(
    authority: GraphAuthority,
    selected: &[PartitionId],
    cancellation: &'cancel Cancellation,
    trace: &mut TraceProbe<'_>,
) -> Result<GraphLease<'cancel>, StreamCapacityError> {
    GraphLease::new(authority, selected, capacity(1), cancellation, trace)
}

#[derive(Debug)]
struct WakeCount(AtomicUsize);

// A `Waker` must own its wake target across an executor boundary; this Arc is test scaffolding,
// not graph-lease ownership. The leased producer, consumer, slots, and cancellation registry are
// all stack-owned and allocation-free.
impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn reverse_provider_arrival_is_lent_in_selected_partition_order() -> Result<(), StreamCapacityError>
{
    let graph = authority(1);
    let first = PartitionId::new(2);
    let second = PartitionId::new(7);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[first, second], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
    let first_edge = [edge(graph, first, 1, 2)];
    let second_edge = [edge(graph, second, 3, 4)];
    let mut context = Context::from_waker(Waker::noop());

    producer.settle(second, &second_edge)?;
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Pending
    ));
    producer.settle(first, &first_edge)?;

    {
        let first_event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(first_batch)) = first_event else {
            return Err(StreamCapacityError::StreamClosed);
        };
        assert_eq!(first_batch.as_ref(), &first_edge);
        drop(first_batch);
    }

    {
        let second_event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(second_batch)) = second_event else {
            return Err(StreamCapacityError::StreamClosed);
        };
        assert_eq!(second_batch.as_ref(), &second_edge);
        drop(second_batch);
    }
    assert_eq!(stream.load().edges, 0);

    producer.finish()?;
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Complete { authority }))
            if authority == graph
    ));
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Fused)
    ));
    Ok(())
}

#[test]
fn scoped_worker_publishes_one_batch_without_arc_or_lock() -> Result<(), StreamCapacityError> {
    let graph = authority(2);
    let partition = PartitionId::new(1);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
    let source = [edge(graph, partition, 1, 2)];
    let produced = thread::scope(|scope| {
        let producer = scope.spawn(move || producer.settle(partition, &source));
        producer.join()
    });
    assert!(matches!(produced, Ok(Ok(()))));

    let mut context = Context::from_waker(Waker::noop());
    let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
    let Poll::Ready(GraphStreamEvent::Batch(batch)) = event else {
        return Err(StreamCapacityError::StreamClosed);
    };
    assert_eq!(batch.as_ref(), &source);
    drop(batch);
    Ok(())
}

#[test]
fn consumed_slot_never_reissues_its_partition_identity() -> Result<(), StreamCapacityError> {
    let graph = authority(3);
    let partition = PartitionId::new(4);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
    let source = [edge(graph, partition, 1, 2)];
    producer.settle(partition, &source)?;
    let mut context = Context::from_waker(Waker::noop());
    let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
    let Poll::Ready(GraphStreamEvent::Batch(batch)) = event else {
        return Err(StreamCapacityError::StreamClosed);
    };
    drop(batch);
    assert_eq!(
        producer.settle(partition, &source),
        Err(StreamCapacityError::PartitionAlreadySettled { partition })
    );
    Ok(())
}

#[test]
fn cancellation_wakes_pending_stream_and_returns_every_credit() -> Result<(), StreamCapacityError> {
    let graph = authority(4);
    let partition = PartitionId::new(5);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
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
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled { authority }))
            if authority == graph
    ));
    assert_eq!(stream.load().edges, 0);
    assert_eq!(
        producer.poll_ready(&mut context),
        Poll::Ready(Err(StreamCapacityError::StreamClosed))
    );
    Ok(())
}

#[test]
fn disconnected_producer_has_one_typed_terminal() -> Result<(), StreamCapacityError> {
    let graph = authority(5);
    let partition = PartitionId::new(6);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[partition], &cancellation, &mut trace)?;
    let (producer, mut stream) = lease.split()?;
    drop(producer);
    let mut context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Failed {
            authority,
            cause: StreamCapacityError::ProducerDisconnected,
        })) if authority == graph
    ));
    assert!(matches!(
        Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
        Poll::Ready(GraphStreamEvent::Fused)
    ));
    Ok(())
}

#[test]
fn exact_partial_declaration_cannot_forge_absence() -> Result<(), StreamCapacityError> {
    let graph = authority(6);
    let present = PartitionId::new(0);
    let missing = PartitionId::new(1);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[present, missing], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
    producer.settle(present, &[edge(graph, present, 1, 2)])?;
    assert_eq!(
        producer.finish_partial(&[present]),
        Err(StreamCapacityError::IncorrectMissingPartitions {
            expected: Some(missing),
            observed: Some(present),
            index: 0,
        })
    );
    producer.finish_partial(&[missing])?;
    let mut context = Context::from_waker(Waker::noop());
    {
        let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(batch)) = event else {
            return Err(StreamCapacityError::StreamClosed);
        };
        drop(batch);
    }
    let terminal = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
    let Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Partial {
        authority: observed_authority,
        missing: observed_missing,
    })) = terminal
    else {
        return Err(StreamCapacityError::StreamClosed);
    };
    assert_eq!(observed_authority, graph);
    assert_eq!(observed_missing.as_ref(), &[missing]);
    Ok(())
}

#[test]
fn degraded_terminal_retains_typed_cause_and_exact_missing_partition()
-> Result<(), StreamCapacityError> {
    let graph = authority(10);
    let present = PartitionId::new(0);
    let missing = PartitionId::new(1);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[present, missing], &cancellation, &mut trace)?;
    let (mut producer, mut stream) = lease.split()?;
    producer.settle(present, &[edge(graph, present, 1, 2)])?;
    producer.finish_degraded(&[missing], GraphDegradation::PartitionSourceUnavailable)?;
    let mut context = Context::from_waker(Waker::noop());
    {
        let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
        let Poll::Ready(GraphStreamEvent::Batch(batch)) = event else {
            return Err(StreamCapacityError::StreamClosed);
        };
        drop(batch);
    }
    let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
    let Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::DegradedPartial {
        authority: observed_authority,
        missing: observed_missing,
        reason,
    })) = event
    else {
        return Err(StreamCapacityError::StreamClosed);
    };
    assert_eq!(observed_authority, graph);
    assert_eq!(observed_missing.as_ref(), &[missing]);
    assert_eq!(reason, GraphDegradation::PartitionSourceUnavailable);
    Ok(())
}

#[test]
fn one_lease_has_one_endpoint_pair() -> Result<(), StreamCapacityError> {
    let graph = authority(7);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = lease(graph, &[], &cancellation, &mut trace)?;
    let endpoints = lease.split()?;
    assert!(matches!(
        lease.split(),
        Err(StreamCapacityError::EndpointsAlreadyBorrowed)
    ));
    drop(endpoints);
    Ok(())
}

#[test]
fn stale_lease_drop_cannot_release_a_reused_cancellation_registration()
-> Result<(), StreamCapacityError> {
    let graph = authority(8);
    let first = PartitionId::new(1);
    let second = PartitionId::new(2);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let first_lease = lease(graph, &[first], &cancellation, &mut trace)?;
    let (first_producer, mut first_stream) = first_lease.split()?;
    drop(first_producer);
    let mut no_op_context = Context::from_waker(Waker::noop());
    assert!(matches!(
        Pin::new(&mut first_stream).poll_batch(&mut no_op_context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Failed {
            cause: StreamCapacityError::ProducerDisconnected,
            ..
        }))
    ));
    drop(first_stream);

    let second_lease = lease(graph, &[second], &cancellation, &mut trace)?;
    let (_second_producer, mut second_stream) = second_lease.split()?;
    let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
    let wake = Waker::from(Arc::clone(&wakes));
    let mut waiting_context = Context::from_waker(&wake);
    assert!(matches!(
        Pin::new(&mut second_stream).poll_batch(&mut waiting_context, &mut trace),
        Poll::Pending
    ));

    // `first_lease` has transferred its registration at split, so its late owner drop cannot
    // clear or take the Waker that now belongs to `second_stream`.
    drop(first_lease);
    cancellation.cancel();
    assert_eq!(wakes.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        Pin::new(&mut second_stream).poll_batch(&mut waiting_context, &mut trace),
        Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled { authority }))
            if authority == graph
    ));
    Ok(())
}
