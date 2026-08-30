use core::{
    mem::{size_of, size_of_val},
    pin::Pin,
    sync::atomic::Ordering,
    task::{Context, Poll, Waker},
};

#[cfg(all(test, feature = "loom-model"))]
use loom::sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool};
#[cfg(not(all(test, feature = "loom-model")))]
use std::sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool};

use crate::{GraphAuthority, GraphEdge, GraphTraceEvent, MAX_PARTITIONS, PartitionId, TraceProbe};

const MAX_LEASED_EDGES: usize = 16;
const MAX_CANCEL_WAITERS: usize = MAX_PARTITIONS;

#[derive(Debug)]
struct CancelSlot {
    occupied: bool,
    waker: Option<Waker>,
}

impl CancelSlot {
    const fn vacant() -> Self {
        Self {
            occupied: false,
            waker: None,
        }
    }
}

/// Cancellation authority with one independently removable wake slot per admitted stream.
#[derive(Debug)]
pub struct Cancellation {
    cancelled: AtomicBool,
    waiters: Mutex<[CancelSlot; MAX_CANCEL_WAITERS]>,
}

impl Cancellation {
    /// Creates a clear cancellation authority with bounded wake storage.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            waiters: Mutex::new(core::array::from_fn(|_| CancelSlot::vacant())),
        }
    }

    /// Publishes cancellation and wakes every independently registered pending stream.
    pub fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut wakes: [Option<Waker>; MAX_CANCEL_WAITERS] = core::array::from_fn(|_| None);
        {
            let mut waiters = match self.waiters.lock() {
                Ok(waiters) => waiters,
                Err(poisoned) => poisoned.into_inner(),
            };
            for (destination, slot) in wakes.iter_mut().zip(waiters.iter_mut()) {
                *destination = slot.waker.take();
            }
        }
        for waker in wakes.into_iter().flatten() {
            waker.wake();
        }
    }

    fn reserve(&self) -> Result<usize, StreamCapacityError> {
        let mut waiters = match self.waiters.lock() {
            Ok(waiters) => waiters,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some((index, slot)) = waiters
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| !slot.occupied)
        else {
            return Err(StreamCapacityError::CancellationCapacity {
                maximum: MAX_CANCEL_WAITERS,
            });
        };
        slot.occupied = true;
        Ok(index)
    }

    fn register(&self, slot_index: usize, waker: &Waker) {
        let mut waiters = match self.waiters.lock() {
            Ok(waiters) => waiters,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(slot) = waiters.get_mut(slot_index) else {
            return;
        };
        if slot
            .waker
            .as_ref()
            .is_none_or(|current| !current.will_wake(waker))
        {
            slot.waker = Some(waker.clone());
        }
    }

    fn clear(&self, slot_index: usize) {
        let mut waiters = match self.waiters.lock() {
            Ok(waiters) => waiters,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(slot) = waiters.get_mut(slot_index) {
            slot.waker = None;
        }
    }

    fn release(&self, slot_index: usize) {
        let mut waiters = match self.waiters.lock() {
            Ok(waiters) => waiters,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(slot) = waiters.get_mut(slot_index) {
            slot.waker = None;
            slot.occupied = false;
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// Exact capacity or authority rejection before stream state is changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamCapacityError {
    /// Configured item capacity is invalid.
    InvalidItemCapacity {
        /// Maximum fixed storage supported by this stream.
        maximum: usize,
        /// Rejected requested capacity.
        observed: usize,
    },
    /// Configured byte capacity cannot hold the declared item reserve.
    InvalidByteCapacity {
        /// Bytes needed for the item reserve.
        required: usize,
        /// Rejected requested bytes.
        observed: usize,
    },
    /// No independent cancellation wake slot remained.
    CancellationCapacity {
        /// Maximum simultaneous streams sharing one cancellation authority.
        maximum: usize,
    },
    /// Selected partition fanout exceeded the fixed bound.
    PartitionCapacity {
        /// Maximum selected partitions.
        maximum: usize,
        /// Complete observed selected partitions.
        observed: usize,
    },
    /// One selected partition occurred twice.
    DuplicatePartition {
        /// Earlier selected position.
        first_index: usize,
        /// Later selected position.
        index: usize,
        /// Repeated partition.
        partition: PartitionId,
    },
    /// The completion slot still owns the preceding batch.
    BatchAlreadySettled,
    /// The stream has a queued terminal or has been cancelled/dropped.
    StreamClosed,
    /// Settled data exceeds the configured item reserve.
    ItemCapacity {
        /// Configured item reserve.
        maximum: usize,
        /// Complete observed item count.
        observed: usize,
    },
    /// Settled data exceeds the configured byte reserve.
    ByteCapacity {
        /// Configured byte reserve.
        maximum: usize,
        /// Complete observed byte charge.
        observed: usize,
    },
    /// A completion named a partition outside the admitted selection.
    UnselectedPartition {
        /// Rejected partition.
        observed: PartitionId,
    },
    /// One settled edge belongs to another stream authority.
    WrongAuthority {
        /// Rejected edge coordinate.
        edge_index: usize,
        /// Stream authority.
        expected: GraphAuthority,
        /// Rejected authority.
        observed: GraphAuthority,
    },
    /// One settled edge belongs to another partition.
    WrongPartition {
        /// Rejected edge coordinate.
        edge_index: usize,
        /// Completion partition.
        expected: PartitionId,
        /// Rejected edge partition.
        observed: PartitionId,
    },
    /// Exact missing-partition terminal exceeded the selected fanout.
    MissingPartitionCapacity {
        /// Maximum selected partitions.
        maximum: usize,
        /// Complete observed missing partitions.
        observed: usize,
    },
    /// One missing partition occurred twice.
    DuplicateMissingPartition {
        /// Earlier missing position.
        first_index: usize,
        /// Later missing position.
        index: usize,
        /// Repeated missing partition.
        partition: PartitionId,
    },
}

/// Exact short-output rejection; the lending batch remains unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InsufficientOutput {
    required: usize,
    available: usize,
}

impl InsufficientOutput {
    /// Returns the complete batch size.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the supplied destination size.
    #[must_use]
    pub const fn available(self) -> usize {
        self.available
    }
}

/// Terminal facts for the graph leased edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTerminal {
    /// All admitted partitions completed.
    Complete {
        /// Complete graph authority.
        authority: GraphAuthority,
    },
    /// Cancellation won and released every stream-owned charge.
    Cancelled {
        /// Complete graph authority.
        authority: GraphAuthority,
    },
    /// One or more requested partitions were exactly absent.
    Partial {
        /// Complete graph authority.
        authority: GraphAuthority,
        /// Exact absent partition coordinates in semantic order.
        missing: [Option<PartitionId>; MAX_PARTITIONS],
        /// Number of occupied entries in `missing`.
        missing_len: usize,
    },
}

impl GraphTerminal {
    /// Returns the authority retained by every terminal.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        match self {
            Self::Complete { authority }
            | Self::Cancelled { authority }
            | Self::Partial { authority, .. } => authority,
        }
    }

    /// Borrows the exact absent partition prefix, empty for complete or cancelled terminals.
    #[must_use]
    pub fn missing(&self) -> &[Option<PartitionId>] {
        match self {
            Self::Partial {
                missing,
                missing_len,
                ..
            } => &missing[..*missing_len],
            Self::Complete { .. } | Self::Cancelled { .. } => &[],
        }
    }
}

/// One runtime-independent leased stream observation.
#[derive(Debug)]
pub enum GraphStreamEvent<'stream> {
    /// A complete settled batch lending the stream's exact owners.
    Batch(LeasedGraphBatch<'stream>),
    /// The exact terminal, observed once.
    Terminal(GraphTerminal),
    /// Polling after terminal performs no registration or work.
    Fused,
}

#[derive(Debug)]
struct CompletionState {
    authority: GraphAuthority,
    selected: [Option<PartitionId>; MAX_PARTITIONS],
    selected_len: usize,
    item_capacity: usize,
    byte_capacity: usize,
    edges: [Option<GraphEdge>; MAX_LEASED_EDGES],
    len: usize,
    charged_bytes: usize,
    ready: bool,
    closed: bool,
    terminal: Option<GraphTerminal>,
    terminal_emitted: bool,
    consumer_waiter: Option<Waker>,
    producer_waiter: Option<Waker>,
}

impl CompletionState {
    fn release_batch(&mut self) {
        for slot in self.edges.iter_mut().take(self.len) {
            *slot = None;
        }
        self.len = 0;
        self.charged_bytes = 0;
        self.ready = false;
        if let Some(waker) = self.producer_waiter.take() {
            waker.wake();
        }
    }

    fn selected_contains(&self, partition: PartitionId) -> bool {
        self.selected[..self.selected_len].contains(&Some(partition))
    }
}

/// Concurrent completion owner for one bounded graph edge stream.
#[derive(Debug)]
pub struct EdgeBatchProducer {
    shared: Arc<Mutex<CompletionState>>,
}

impl EdgeBatchProducer {
    /// Polls for one free completion credit; consuming a batch wakes this registration.
    pub fn poll_ready(&self, context: &mut Context<'_>) -> Poll<Result<(), StreamCapacityError>> {
        let mut state = lock_state(&self.shared);
        if state.closed {
            return Poll::Ready(Err(StreamCapacityError::StreamClosed));
        }
        if !state.ready {
            return Poll::Ready(Ok(()));
        }
        if state
            .producer_waiter
            .as_ref()
            .is_none_or(|current| !current.will_wake(context.waker()))
        {
            state.producer_waiter = Some(context.waker().clone());
        }
        Poll::Pending
    }

    /// Transfers one complete provider batch into the fixed lease slot.
    pub fn settle(
        &self,
        partition: PartitionId,
        edges: &[GraphEdge],
    ) -> Result<(), StreamCapacityError> {
        let mut state = lock_state(&self.shared);
        if state.closed {
            return Err(StreamCapacityError::StreamClosed);
        }
        if state.ready || state.len != 0 {
            return Err(StreamCapacityError::BatchAlreadySettled);
        }
        if !state.selected_contains(partition) {
            return Err(StreamCapacityError::UnselectedPartition {
                observed: partition,
            });
        }
        if edges.len() > state.item_capacity {
            return Err(StreamCapacityError::ItemCapacity {
                maximum: state.item_capacity,
                observed: edges.len(),
            });
        }
        let observed_bytes = size_of_val(edges);
        if observed_bytes > state.byte_capacity {
            return Err(StreamCapacityError::ByteCapacity {
                maximum: state.byte_capacity,
                observed: observed_bytes,
            });
        }
        for (edge_index, edge) in edges.iter().copied().enumerate() {
            if edge.authority() != state.authority {
                return Err(StreamCapacityError::WrongAuthority {
                    edge_index,
                    expected: state.authority,
                    observed: edge.authority(),
                });
            }
            if edge.partition() != partition {
                return Err(StreamCapacityError::WrongPartition {
                    edge_index,
                    expected: partition,
                    observed: edge.partition(),
                });
            }
        }
        for (slot, edge) in state.edges.iter_mut().zip(edges.iter().copied()) {
            *slot = Some(edge);
        }
        state.len = edges.len();
        state.charged_bytes = observed_bytes;
        state.ready = true;
        if let Some(waker) = state.consumer_waiter.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Queues complete termination after every settled batch is consumed.
    pub fn finish(&self) -> Result<(), StreamCapacityError> {
        let mut state = lock_state(&self.shared);
        if state.closed {
            return Err(StreamCapacityError::StreamClosed);
        }
        state.closed = true;
        state.terminal = Some(GraphTerminal::Complete {
            authority: state.authority,
        });
        if let Some(waker) = state.consumer_waiter.take() {
            waker.wake();
        }
        Ok(())
    }

    /// Queues a terminal retaining every exactly absent selected partition.
    pub fn finish_partial(&self, missing: &[PartitionId]) -> Result<(), StreamCapacityError> {
        let mut state = lock_state(&self.shared);
        if state.closed {
            return Err(StreamCapacityError::StreamClosed);
        }
        if missing.len() > state.selected_len {
            return Err(StreamCapacityError::MissingPartitionCapacity {
                maximum: state.selected_len,
                observed: missing.len(),
            });
        }
        for (index, partition) in missing.iter().copied().enumerate() {
            if !state.selected_contains(partition) {
                return Err(StreamCapacityError::UnselectedPartition {
                    observed: partition,
                });
            }
            if let Some(first_index) = missing[..index]
                .iter()
                .position(|first| *first == partition)
            {
                return Err(StreamCapacityError::DuplicateMissingPartition {
                    first_index,
                    index,
                    partition,
                });
            }
        }
        let mut exact_missing = [None; MAX_PARTITIONS];
        for (slot, partition) in exact_missing.iter_mut().zip(missing.iter().copied()) {
            *slot = Some(partition);
        }
        state.closed = true;
        state.terminal = Some(GraphTerminal::Partial {
            authority: state.authority,
            missing: exact_missing,
            missing_len: missing.len(),
        });
        if let Some(waker) = state.consumer_waiter.take() {
            waker.wake();
        }
        Ok(())
    }
}

/// Bounded stream consumer with a unique cancellation wake registration.
#[derive(Debug)]
pub struct EdgeBatchStream<'cancellation> {
    shared: Arc<Mutex<CompletionState>>,
    cancellation: &'cancellation Cancellation,
    cancellation_slot: usize,
}

impl<'cancellation> EdgeBatchStream<'cancellation> {
    /// Admits selected partitions and returns separate producer and consumer ownership.
    pub fn channel(
        authority: GraphAuthority,
        selected: &[PartitionId],
        item_capacity: usize,
        byte_capacity: usize,
        cancellation: &'cancellation Cancellation,
        trace: &mut TraceProbe<'_>,
    ) -> Result<(EdgeBatchProducer, Self), StreamCapacityError> {
        if item_capacity > MAX_LEASED_EDGES {
            return Err(StreamCapacityError::InvalidItemCapacity {
                maximum: MAX_LEASED_EDGES,
                observed: item_capacity,
            });
        }
        let required = item_capacity * size_of::<GraphEdge>();
        if byte_capacity < required {
            return Err(StreamCapacityError::InvalidByteCapacity {
                required,
                observed: byte_capacity,
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
        for (slot, partition) in selected_storage.iter_mut().zip(selected.iter().copied()) {
            *slot = Some(partition);
        }
        let shared = Arc::new(Mutex::new(CompletionState {
            authority,
            selected: selected_storage,
            selected_len: selected.len(),
            item_capacity,
            byte_capacity,
            edges: [None; MAX_LEASED_EDGES],
            len: 0,
            charged_bytes: 0,
            ready: false,
            closed: false,
            terminal: None,
            terminal_emitted: false,
            consumer_waiter: None,
            producer_waiter: None,
        }));
        trace.record_with(|| GraphTraceEvent::Admitted {
            authority,
            partitions: selected.len() as u8,
        });
        Ok((
            EdgeBatchProducer {
                shared: Arc::clone(&shared),
            },
            Self {
                shared,
                cancellation,
                cancellation_slot,
            },
        ))
    }

    /// Polls with register-before-pending and rechecks both cancellation and completion.
    pub fn poll_batch<'stream>(
        self: Pin<&'stream mut Self>,
        context: &mut Context<'_>,
        trace: &mut TraceProbe<'_>,
    ) -> Poll<GraphStreamEvent<'stream>> {
        let stream = self.get_mut();
        let mut state = lock_state(&stream.shared);
        if state.terminal_emitted {
            return Poll::Ready(GraphStreamEvent::Fused);
        }
        if stream.cancellation.is_cancelled() {
            state.release_batch();
            state.closed = true;
            state.terminal = None;
            state.terminal_emitted = true;
            stream.cancellation.clear(stream.cancellation_slot);
            let terminal = GraphTerminal::Cancelled {
                authority: state.authority,
            };
            trace.record_with(|| GraphTraceEvent::Terminal {
                authority: state.authority,
                cancelled: true,
            });
            return Poll::Ready(GraphStreamEvent::Terminal(terminal));
        }
        if state.ready {
            stream.cancellation.clear(stream.cancellation_slot);
            trace.record_with(|| GraphTraceEvent::BatchReady {
                authority: state.authority,
                edges: state.len as u8,
            });
            return Poll::Ready(GraphStreamEvent::Batch(LeasedGraphBatch { state }));
        }
        if let Some(terminal) = state.terminal.take() {
            state.terminal_emitted = true;
            stream.cancellation.clear(stream.cancellation_slot);
            trace.record_with(|| GraphTraceEvent::Terminal {
                authority: state.authority,
                cancelled: false,
            });
            return Poll::Ready(GraphStreamEvent::Terminal(terminal));
        }
        if state
            .consumer_waiter
            .as_ref()
            .is_none_or(|current| !current.will_wake(context.waker()))
        {
            state.consumer_waiter = Some(context.waker().clone());
        }
        stream
            .cancellation
            .register(stream.cancellation_slot, context.waker());
        if stream.cancellation.is_cancelled() {
            state.consumer_waiter = None;
            state.release_batch();
            state.closed = true;
            state.terminal_emitted = true;
            stream.cancellation.clear(stream.cancellation_slot);
            let terminal = GraphTerminal::Cancelled {
                authority: state.authority,
            };
            trace.record_with(|| GraphTraceEvent::Terminal {
                authority: state.authority,
                cancelled: true,
            });
            return Poll::Ready(GraphStreamEvent::Terminal(terminal));
        }
        Poll::Pending
    }

    /// Reports currently charged item slots.
    #[must_use]
    pub fn charged_items(&self) -> usize {
        lock_state(&self.shared).len
    }

    /// Reports currently charged bytes.
    #[must_use]
    pub fn charged_bytes(&self) -> usize {
        lock_state(&self.shared).charged_bytes
    }
}

impl Drop for EdgeBatchStream<'_> {
    fn drop(&mut self) {
        self.cancellation.release(self.cancellation_slot);
        let mut state = lock_state(&self.shared);
        state.closed = true;
        state.release_batch();
        state.consumer_waiter = None;
    }
}

/// Exclusive lending authority over one complete settled provider batch.
#[derive(Debug)]
pub struct LeasedGraphBatch<'stream> {
    state: MutexGuard<'stream, CompletionState>,
}

impl LeasedGraphBatch<'_> {
    /// Returns the complete retained batch size.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.len
    }

    /// Returns true only for an empty provider batch.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.len == 0
    }

    /// Copies only after proving the destination can retain every leased edge.
    pub fn copy_into(&mut self, output: &mut [GraphEdge]) -> Result<usize, InsufficientOutput> {
        let required = self.state.len;
        if output.len() < required {
            return Err(InsufficientOutput {
                required,
                available: output.len(),
            });
        }
        for (destination, source) in output
            .iter_mut()
            .zip(self.state.edges.iter())
            .take(required)
        {
            if let Some(edge) = source {
                *destination = *edge;
            }
        }
        self.state.release_batch();
        Ok(required)
    }
}

fn lock_state(shared: &Mutex<CompletionState>) -> MutexGuard<'_, CompletionState> {
    match shared.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod loom_tests {
    use core::{
        mem::size_of,
        pin::Pin,
        sync::atomic::Ordering,
        task::{Context, Poll, Waker},
    };
    use std::{sync::Arc as StandardArc, task::Wake};

    use loom::{
        sync::{Arc, atomic::AtomicUsize},
        thread,
    };
    use nudox_index_vocab::IndexSnapshotId;
    use nudox_ir_vocab::EntityId;

    use super::{Cancellation, EdgeBatchStream, GraphStreamEvent, GraphTerminal};
    use crate::{GraphAuthority, GraphEdge, PartitionId, ProjectionId, TraceProbe};

    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: StandardArc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &StandardArc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn authority() -> GraphAuthority {
        GraphAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"loom graph snapshot"),
            ProjectionId::new(1),
        )
    }

    #[test]
    fn concurrent_completion_wakes_consumer_and_releases_exact_credit() {
        loom::model(|| {
            let authority = authority();
            let cancellation = Arc::new(Cancellation::new());
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
            let wake_count = StandardArc::new(WakeCount(AtomicUsize::new(0)));
            let waker = Waker::from(StandardArc::clone(&wake_count));
            let mut context = Context::from_waker(&waker);
            assert!(matches!(
                Pin::new(&mut stream).poll_batch(&mut context, &mut trace),
                Poll::Pending
            ));
            let producer = Arc::new(producer);
            let completion = Arc::clone(&producer);
            let settle = thread::spawn(move || {
                completion.settle(
                    PartitionId::new(0),
                    &[GraphEdge::new(
                        authority,
                        PartitionId::new(0),
                        EntityId::new(1),
                        EntityId::new(2),
                    )],
                )
            });
            assert!(settle.join().is_ok());
            let producer_wake_count = StandardArc::new(WakeCount(AtomicUsize::new(0)));
            let producer_waker = Waker::from(StandardArc::clone(&producer_wake_count));
            let mut producer_context = Context::from_waker(&producer_waker);
            assert!(matches!(
                producer.poll_ready(&mut producer_context),
                Poll::Pending
            ));
            {
                let event = Pin::new(&mut stream).poll_batch(&mut context, &mut trace);
                let Poll::Ready(GraphStreamEvent::Batch(mut batch)) = event else {
                    return;
                };
                let mut output = [GraphEdge::new(
                    authority,
                    PartitionId::new(0),
                    EntityId::new(0),
                    EntityId::new(0),
                )];
                assert_eq!(batch.copy_into(&mut output), Ok(1));
            }
            assert_eq!(stream.charged_items(), 0);
            assert_eq!(producer_wake_count.0.load(Ordering::SeqCst), 1);
            assert!(matches!(
                producer.poll_ready(&mut producer_context),
                Poll::Ready(Ok(()))
            ));
            assert!(wake_count.0.load(Ordering::SeqCst) <= 1);
        });
    }

    #[test]
    fn shared_cancellation_wakes_every_stream_registration() {
        loom::model(|| {
            let authority = authority();
            let cancellation = Arc::new(Cancellation::new());
            let mut trace = TraceProbe::disabled();
            let first = EdgeBatchStream::channel(
                authority,
                &[PartitionId::new(0)],
                0,
                0,
                &cancellation,
                &mut trace,
            );
            let second = EdgeBatchStream::channel(
                authority,
                &[PartitionId::new(1)],
                0,
                0,
                &cancellation,
                &mut trace,
            );
            assert!(first.is_ok() && second.is_ok());
            let (Ok((_first_producer, mut first)), Ok((_second_producer, mut second))) =
                (first, second)
            else {
                return;
            };
            let first_count = StandardArc::new(WakeCount(AtomicUsize::new(0)));
            let second_count = StandardArc::new(WakeCount(AtomicUsize::new(0)));
            let first_waker = Waker::from(StandardArc::clone(&first_count));
            let second_waker = Waker::from(StandardArc::clone(&second_count));
            let mut first_context = Context::from_waker(&first_waker);
            let mut second_context = Context::from_waker(&second_waker);
            assert!(matches!(
                Pin::new(&mut first).poll_batch(&mut first_context, &mut trace),
                Poll::Pending
            ));
            assert!(matches!(
                Pin::new(&mut second).poll_batch(&mut second_context, &mut trace),
                Poll::Pending
            ));
            let cancellation_for_thread = Arc::clone(&cancellation);
            let cancel = thread::spawn(move || cancellation_for_thread.cancel());
            assert!(cancel.join().is_ok());
            assert_eq!(first_count.0.load(Ordering::SeqCst), 1);
            assert_eq!(second_count.0.load(Ordering::SeqCst), 1);
            assert!(matches!(
                Pin::new(&mut first).poll_batch(&mut first_context, &mut trace),
                Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled { .. }))
            ));
            assert!(matches!(
                Pin::new(&mut second).poll_batch(&mut second_context, &mut trace),
                Poll::Ready(GraphStreamEvent::Terminal(GraphTerminal::Cancelled { .. }))
            ));
        });
    }
}
