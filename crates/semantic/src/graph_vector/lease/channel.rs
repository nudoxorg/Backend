//! Defines lease channel behavior for `backend-semantic::graph_vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the lease channel invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Stack-owned graph-lease owner and endpoint protocol.

use core::{
    cell::Cell,
    marker::PhantomData,
    mem::{size_of, size_of_val},
    ops::Deref,
    pin::Pin,
    sync::atomic::Ordering,
    task::{Context, Poll},
};

use super::{
    cancellation::{Cancellation, CancellationReservation},
    contract::{
        GraphDegradation, GraphTerminal, LeaseCapacity, LeaseLoad, LeaseStateCell,
        StreamCapacityError,
    },
    storage::{EdgeRead, EdgeSlot, MAX_EDGES_PER_PARTITION, SharedLease, SlotPhase},
    wake::WakeCell,
};
use crate::graph_vector::{
    GraphAuthority, GraphEdge, GraphTraceEvent, MAX_PARTITIONS, MissingPartitions, PartitionId,
    TraceProbe,
};

mod produce;

/// One leased stream observation.
#[derive(Debug)]
pub enum GraphStreamEvent<'stream> {
    /// A borrowed immutable batch; dropping it returns that slot's credit.
    Batch(LeasedGraphBatch<'stream>),
    /// The one terminal fact.
    Terminal(GraphTerminal),
    /// The terminal was already observed, so no work or registration remains.
    Fused,
}

/// Stack-owned fixed storage for one graph request.
///
/// `split()` is one-shot. It returns a movable, non-`Sync` producer and a movable, non-`Sync`
/// consumer. A scoped thread can own the producer while one task polls the consumer; the stack
/// owner keeps permanent inline slots alive, so no `Arc` or reclamation scheme is required.
///
/// ```compile_fail
/// use backend_semantic::graph_vector::GraphLease;
/// fn require_clone_and_share<T: Clone + Sync>() {}
/// require_clone_and_share::<GraphLease<'static>>();
/// ```
pub struct GraphLease<'cancellation> {
    shared: SharedLease,
    cancellation: &'cancellation Cancellation,
    // Ownership moves into the stream at `split`. The owner can only release a reservation if
    // endpoint construction failed before that move; this prevents a stale owner drop from
    // clearing a slot subsequently reused by another lease.
    cancellation_slot: Cell<Option<CancellationReservation>>,
    endpoints_borrowed: Cell<bool>,
}

impl<'cancellation> GraphLease<'cancellation> {
    /// Validates all bounds before reserving fixed cancellation state.
    pub fn new(
        authority: GraphAuthority,
        selected: &[PartitionId],
        capacity: LeaseCapacity,
        cancellation: &'cancellation Cancellation,
        trace: &mut TraceProbe<'_>,
    ) -> Result<Self, StreamCapacityError> {
        let edge_capacity = usize::from(capacity.edges_per_partition);
        if edge_capacity > MAX_EDGES_PER_PARTITION {
            return Err(StreamCapacityError::InvalidItemCapacity {
                maximum: MAX_EDGES_PER_PARTITION,
                observed: edge_capacity,
            });
        }
        let required = edge_capacity * size_of::<GraphEdge>();
        if capacity.bytes_per_partition < required {
            return Err(StreamCapacityError::InvalidByteCapacity {
                required,
                observed: capacity.bytes_per_partition,
            });
        }
        if selected.len() > MAX_PARTITIONS {
            return Err(StreamCapacityError::PartitionCapacity {
                maximum: MAX_PARTITIONS,
                observed: selected.len(),
            });
        }
        for (index, partition) in selected.iter().copied().enumerate() {
            if let Some(first_index) = selected[..index]
                .iter()
                .position(|first| *first == partition)
            {
                return Err(StreamCapacityError::DuplicatePartition {
                    first_index,
                    index,
                    partition,
                });
            }
        }
        let cancellation_slot = cancellation.reserve()?;
        let mut selected_storage = [None; MAX_PARTITIONS];
        for (destination, partition) in selected_storage.iter_mut().zip(selected.iter().copied()) {
            *destination = Some(partition);
        }
        trace.record_with(|| GraphTraceEvent::Admitted {
            authority,
            partitions: selected.len() as u8,
        });
        Ok(Self {
            shared: SharedLease::new(authority, selected_storage, selected.len(), capacity),
            cancellation,
            cancellation_slot: Cell::new(Some(cancellation_slot)),
            endpoints_borrowed: Cell::new(false),
        })
    }

    /// Lends the unique producer and consumer endpoint for this stack-owned request.
    pub fn split(
        &self,
    ) -> Result<(EdgeBatchProducer<'_>, EdgeBatchStream<'_, 'cancellation>), StreamCapacityError>
    {
        if self.endpoints_borrowed.replace(true) {
            return Err(StreamCapacityError::EndpointsAlreadyBorrowed);
        }
        let Some(cancellation_slot) = self.cancellation_slot.take() else {
            return Err(StreamCapacityError::EndpointsAlreadyBorrowed);
        };
        Ok((
            EdgeBatchProducer {
                shared: &self.shared,
                delivered: [false; MAX_PARTITIONS],
                finished: false,
                unique: PhantomData,
            },
            EdgeBatchStream {
                shared: &self.shared,
                cancellation: self.cancellation,
                cancellation_slot: Some(cancellation_slot),
                next_partition: 0,
                terminal_emitted: false,
                unique: PhantomData,
            },
        ))
    }
}

impl Drop for GraphLease<'_> {
    fn drop(&mut self) {
        self.shared.close();
        if let Some(cancellation_slot) = self.cancellation_slot.take() {
            self.cancellation.release(cancellation_slot);
        }
    }
}

/// The one movable completion authority for a split [`GraphLease`].
///
/// ```compile_fail
/// use backend_semantic::graph_vector::EdgeBatchProducer;
/// fn require_clone_and_share<T: Clone + Sync>() {}
/// require_clone_and_share::<EdgeBatchProducer<'static>>();
/// ```
#[derive(Debug)]
pub struct EdgeBatchProducer<'lease> {
    shared: &'lease SharedLease,
    delivered: [bool; MAX_PARTITIONS],
    finished: bool,
    unique: PhantomData<Cell<()>>,
}

impl Drop for EdgeBatchProducer<'_> {
    fn drop(&mut self) {
        if !self.finished && !self.shared.closed.load(Ordering::Acquire) {
            let _ = self.shared.publish_terminal(GraphTerminal::Failed {
                authority: self.shared.authority,
                cause: StreamCapacityError::ProducerDisconnected,
            });
        }
    }
}

/// The one movable, pinned consumer endpoint for a split [`GraphLease`].
///
/// ```compile_fail
/// use backend_semantic::graph_vector::EdgeBatchStream;
/// fn require_clone_and_share<T: Clone + Sync>() {}
/// require_clone_and_share::<EdgeBatchStream<'static, 'static>>();
/// ```
#[derive(Debug)]
pub struct EdgeBatchStream<'lease, 'cancellation> {
    shared: &'lease SharedLease,
    cancellation: &'cancellation Cancellation,
    // This endpoint is the single release authority once `GraphLease::split` succeeds.
    cancellation_slot: Option<CancellationReservation>,
    next_partition: usize,
    terminal_emitted: bool,
    unique: PhantomData<Cell<()>>,
}

impl<'lease, 'cancellation> EdgeBatchStream<'lease, 'cancellation> {
    /// Polls selected partitions in semantic selection order without collecting provider batches.
    pub fn poll_batch<'stream>(
        self: Pin<&'stream mut Self>,
        context: &mut Context<'_>,
        trace: &mut TraceProbe<'_>,
    ) -> Poll<GraphStreamEvent<'stream>> {
        let stream = self.get_mut();
        if stream.terminal_emitted {
            return Poll::Ready(GraphStreamEvent::Fused);
        }
        loop {
            if stream.cancellation.is_cancelled() {
                stream.shared.close();
                stream.terminal_emitted = true;
                stream.release_cancellation();
                trace.record_with(|| GraphTraceEvent::Terminal {
                    authority: stream.shared.authority,
                    cancelled: true,
                });
                return Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled {
                    authority: stream.shared.authority,
                }));
            }
            if stream.next_partition == stream.shared.selected_len {
                match stream.shared.terminal() {
                    Ok(Some(terminal)) => return stream.terminal(terminal, trace),
                    Ok(None) if stream.register_pending(context) => return Poll::Pending,
                    Ok(None) => continue,
                    Err(cause) => return stream.failed(cause, trace),
                }
            }
            let slot = &stream.shared.slots[stream.next_partition];
            match slot.phase() {
                SlotPhase::Ready if slot.try_begin_read() => {
                    stream.next_partition += 1;
                    trace.record_with(|| GraphTraceEvent::BatchReady {
                        authority: stream.shared.authority,
                        edges: slot.length(),
                    });
                    return Poll::Ready(GraphStreamEvent::Batch(LeasedGraphBatch {
                        edges: slot.borrowed(),
                        _release: BatchRelease {
                            slot,
                            producer_wake: &stream.shared.producer_wake,
                        },
                    }));
                }
                SlotPhase::Ready | SlotPhase::Writing | SlotPhase::Vacant => {
                    if stream.shared.terminal_ready() {
                        stream.next_partition += 1;
                        continue;
                    }
                    if stream.register_pending(context) {
                        return Poll::Pending;
                    }
                }
                SlotPhase::Closed => {
                    return stream.failed(StreamCapacityError::StreamClosed, trace);
                }
                SlotPhase::Reading | SlotPhase::Corrupt => {
                    return stream.failed(
                        StreamCapacityError::CorruptState {
                            cell: LeaseStateCell::PartitionSlot,
                        },
                        trace,
                    );
                }
            }
        }
    }

    /// Observes retained credits as one coherent fact rather than tuple positions.
    #[must_use]
    pub fn load(&self) -> LeaseLoad {
        self.shared.load()
    }

    fn register_pending(&mut self, context: &mut Context<'_>) -> bool {
        self.shared.consumer_wake.register(context.waker());
        if let Some(cancellation_slot) = self.cancellation_slot.as_ref() {
            self.cancellation
                .register(cancellation_slot, context.waker());
        }
        let pending = !self.cancellation.is_cancelled()
            && !self.shared.terminal_ready()
            && (self.next_partition == self.shared.selected_len
                || self.shared.slots[self.next_partition].phase() == SlotPhase::Vacant);
        if !pending {
            self.shared.consumer_wake.take();
        }
        pending
    }

    fn terminal(
        &mut self,
        terminal: GraphTerminal,
        trace: &mut TraceProbe<'_>,
    ) -> Poll<GraphStreamEvent<'_>> {
        self.terminal_emitted = true;
        self.release_cancellation();
        trace.record_with(|| GraphTraceEvent::Terminal {
            authority: self.shared.authority,
            cancelled: false,
        });
        Poll::Ready(GraphStreamEvent::Terminal(terminal))
    }

    fn release_cancellation(&mut self) {
        if let Some(cancellation_slot) = self.cancellation_slot.take() {
            self.cancellation.release(cancellation_slot);
        }
    }

    fn failed(
        &mut self,
        cause: StreamCapacityError,
        trace: &mut TraceProbe<'_>,
    ) -> Poll<GraphStreamEvent<'_>> {
        self.shared.close();
        self.terminal(
            GraphTerminal::Failed {
                authority: self.shared.authority,
                cause,
            },
            trace,
        )
    }
}

impl Drop for EdgeBatchStream<'_, '_> {
    fn drop(&mut self) {
        self.shared.close();
        self.release_cancellation();
    }
}

/// Borrowed edge slice whose drop returns its exact slot to the producer.
///
/// A batch is deliberately `!Send`: its borrow guard must be dropped by the consumer task before
/// that task polls again and permits the slot to be reused. Copy or process the edges before
/// crossing an executor boundary.
///
/// ```compile_fail
/// use backend_semantic::graph_vector::LeasedGraphBatch;
/// fn require_send<Type: Send>() {}
/// require_send::<LeasedGraphBatch<'static>>();
/// ```
#[derive(Debug)]
pub struct LeasedGraphBatch<'lease> {
    // Declaration order is deliberate: the tracked read guard must end before the release phase
    // makes this slot available to a later producer.
    edges: EdgeRead<'lease>,
    _release: BatchRelease<'lease>,
}

impl Deref for LeasedGraphBatch<'_> {
    type Target = [GraphEdge];

    fn deref(&self) -> &Self::Target {
        &self.edges
    }
}

impl AsRef<[GraphEdge]> for LeasedGraphBatch<'_> {
    fn as_ref(&self) -> &[GraphEdge] {
        self
    }
}

#[derive(Debug)]
struct BatchRelease<'lease> {
    slot: &'lease EdgeSlot,
    producer_wake: &'lease WakeCell,
}

impl Drop for BatchRelease<'_> {
    fn drop(&mut self) {
        self.slot.release_read();
        self.producer_wake.wake();
    }
}

#[cfg(test)]
mod trait_contracts {
    use super::{EdgeBatchProducer, EdgeBatchStream, GraphLease};

    fn require_send<Type: Send>() {}

    #[test]
    fn scoped_endpoints_are_movable_without_shared_ownership() {
        require_send::<GraphLease<'static>>();
        require_send::<EdgeBatchProducer<'static>>();
        require_send::<EdgeBatchStream<'static, 'static>>();
    }
}
