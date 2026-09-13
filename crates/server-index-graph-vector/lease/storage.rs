//! Defines lease storage behavior for `server-index-graph-vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the lease storage invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Inline SPSC slots and terminal storage; this is the only raw graph-edge memory boundary.

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::{
    marker::PhantomData,
    mem::{MaybeUninit, size_of},
    ops::Deref,
};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::{
    cell::{self, UnsafeCell},
    contract::{GraphTerminal, LeaseCapacity, LeaseLoad, LeaseStateCell, StreamCapacityError},
    wake::WakeCell,
};
use crate::{GraphAuthority, GraphEdge, MAX_PARTITIONS, PartitionId};

pub(super) const MAX_EDGES_PER_PARTITION: usize = 16;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SlotPhase {
    Vacant = 0,
    Writing = 1,
    Ready = 2,
    Reading = 3,
    Closed = 4,
    Corrupt,
}

impl SlotPhase {
    fn observe(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Vacant as u8 => Self::Vacant,
            raw if raw == Self::Writing as u8 => Self::Writing,
            raw if raw == Self::Ready as u8 => Self::Ready,
            raw if raw == Self::Reading as u8 => Self::Reading,
            raw if raw == Self::Closed as u8 => Self::Closed,
            _ => Self::Corrupt,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalState {
    Open = 0,
    Writing = 1,
    Ready = 2,
    Corrupt,
}

impl TerminalState {
    fn observe(raw: u8) -> Self {
        match raw {
            raw if raw == Self::Open as u8 => Self::Open,
            raw if raw == Self::Writing as u8 => Self::Writing,
            raw if raw == Self::Ready as u8 => Self::Ready,
            _ => Self::Corrupt,
        }
    }
}

#[derive(Debug)]
pub(super) struct EdgeSlot {
    phase: AtomicU8,
    length: AtomicU8,
    edges: UnsafeCell<[MaybeUninit<GraphEdge>; MAX_EDGES_PER_PARTITION]>,
}

impl EdgeSlot {
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(SlotPhase::Vacant as u8),
            length: AtomicU8::new(0),
            edges: UnsafeCell::new(core::array::from_fn(|_| MaybeUninit::uninit())),
        }
    }

    pub(super) fn phase(&self) -> SlotPhase {
        SlotPhase::observe(self.phase.load(Ordering::Acquire))
    }

    pub(super) fn try_begin_write(&self) -> bool {
        self.phase
            .compare_exchange(
                SlotPhase::Vacant as u8,
                SlotPhase::Writing as u8,
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn publish(&self, edges: &[GraphEdge]) -> bool {
        if edges.len() > MAX_EDGES_PER_PARTITION {
            return false;
        }
        // `Writing` grants this unique producer the only mutable payload access. A consumer reads
        // only after the release publication below; `GraphEdge` is Copy, so a closed unpublished
        // slot has no destructor.
        cell::write(&self.edges, |storage| {
            // SAFETY: the Writing phase grants this callback the unique mutable access to the
            // initialized array storage, and the pointer never escapes the callback.
            let storage = unsafe { &mut *storage };
            for (destination, edge) in storage.iter_mut().zip(edges.iter().copied()) {
                destination.write(edge);
            }
        });
        self.length.store(edges.len() as u8, Ordering::Relaxed);
        self.phase
            .compare_exchange(
                SlotPhase::Writing as u8,
                SlotPhase::Ready as u8,
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn try_begin_read(&self) -> bool {
        self.phase
            .compare_exchange(
                SlotPhase::Ready as u8,
                SlotPhase::Reading as u8,
                Ordering::Acquire,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn length(&self) -> u8 {
        self.length.load(Ordering::Relaxed)
    }

    pub(super) fn release_read(&self) {
        self.length.store(0, Ordering::Relaxed);
        self.phase.store(SlotPhase::Vacant as u8, Ordering::Release);
    }

    fn close(&self) {
        let mut observed = self.phase();
        while matches!(
            observed,
            SlotPhase::Vacant | SlotPhase::Writing | SlotPhase::Ready
        ) {
            match self.phase.compare_exchange_weak(
                observed as u8,
                SlotPhase::Closed as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => observed = SlotPhase::observe(actual),
            }
        }
    }

    pub(super) fn borrowed(&self) -> EdgeRead<'_> {
        EdgeRead {
            payload: ReadGuard::new(&self.edges),
            length: usize::from(self.length.load(Ordering::Relaxed)),
            not_send: PhantomData,
        }
    }
}

/// Tracked immutable access to the initialized prefix of one ready edge slot.
#[derive(Debug)]
pub(super) struct EdgeRead<'slot> {
    payload: ReadGuard<'slot, [MaybeUninit<GraphEdge>; MAX_EDGES_PER_PARTITION]>,
    length: usize,
    // This borrow guard is intentionally confined to the task that owns the consumer endpoint.
    // The slot must be released by that same task before the next poll may reuse it.
    not_send: PhantomData<*mut ()>,
}

/// Immutable payload access retained for exactly as long as a borrowed batch is alive.
#[derive(Debug)]
struct ReadGuard<'cell, Payload: ?Sized> {
    #[cfg(all(test, feature = "loom-model"))]
    pointer: loom::cell::ConstPtr<Payload>,
    #[cfg(not(all(test, feature = "loom-model")))]
    pointer: *const Payload,
    lifetime: PhantomData<&'cell Payload>,
}

impl<'cell, Payload: ?Sized> ReadGuard<'cell, Payload> {
    /// Starts a tracked immutable access whose guard lifetime is tied to the source cell.
    fn new(cell: &'cell UnsafeCell<Payload>) -> Self {
        Self {
            pointer: cell.get(),
            lifetime: PhantomData,
        }
    }

    /// Borrows the initialized payload while this guard remains alive.
    unsafe fn as_ref(&self) -> &Payload {
        #[cfg(all(test, feature = "loom-model"))]
        {
            // SAFETY: the guard came from this live cell and keeps Loom's immutable access open.
            unsafe { self.pointer.deref() }
        }
        #[cfg(not(all(test, feature = "loom-model")))]
        {
            // SAFETY: the owner holds the phase that makes this pointer initialized and shared
            // only immutably for the guard lifetime.
            unsafe { &*self.pointer }
        }
    }
}

impl Deref for EdgeRead<'_> {
    type Target = [GraphEdge];

    fn deref(&self) -> &Self::Target {
        // SAFETY: the slot's Ready -> Reading transition proves exactly `length` elements were
        // initialized by the producer. The payload guard outlives this returned borrow, and the
        // batch's release guard drops it before the slot can become Vacant again.
        let payload = unsafe { self.payload.as_ref() };
        // SAFETY: the initialized prefix is contiguous in the inline MaybeUninit array and its
        // length was published by the producer before the Ready transition.
        unsafe { core::slice::from_raw_parts(payload.as_ptr().cast::<GraphEdge>(), self.length) }
    }
}

// SAFETY: phase names the exclusive raw writer or reader. Ready's release and Reading's acquire
// publish initialized Copy edges; a close wins a writer race and no consumer reads a closed slot.
unsafe impl Sync for EdgeSlot {}

#[derive(Debug)]
pub(super) struct SharedLease {
    pub(super) authority: GraphAuthority,
    pub(super) selected: [Option<PartitionId>; MAX_PARTITIONS],
    pub(super) selected_len: usize,
    pub(super) capacity: LeaseCapacity,
    pub(super) slots: [EdgeSlot; MAX_PARTITIONS],
    pub(super) closed: AtomicBool,
    terminal_state: AtomicU8,
    terminal: UnsafeCell<MaybeUninit<GraphTerminal>>,
    pub(super) consumer_wake: WakeCell,
    pub(super) producer_wake: WakeCell,
}

impl SharedLease {
    pub(super) fn new(
        authority: GraphAuthority,
        selected: [Option<PartitionId>; MAX_PARTITIONS],
        selected_len: usize,
        capacity: LeaseCapacity,
    ) -> Self {
        Self {
            authority,
            selected,
            selected_len,
            capacity,
            slots: core::array::from_fn(|_| EdgeSlot::new()),
            closed: AtomicBool::new(false),
            terminal_state: AtomicU8::new(TerminalState::Open as u8),
            terminal: UnsafeCell::new(MaybeUninit::uninit()),
            consumer_wake: WakeCell::new(),
            producer_wake: WakeCell::new(),
        }
    }

    pub(super) fn selected_index(&self, partition: PartitionId) -> Option<usize> {
        self.selected[..self.selected_len]
            .iter()
            .position(|selected| *selected == Some(partition))
    }

    fn terminal_state(&self) -> TerminalState {
        TerminalState::observe(self.terminal_state.load(Ordering::Acquire))
    }

    pub(super) fn terminal_ready(&self) -> bool {
        matches!(
            self.terminal_state(),
            TerminalState::Ready | TerminalState::Corrupt
        )
    }

    pub(super) fn terminal_claimed(&self) -> bool {
        self.terminal_state() != TerminalState::Open
    }

    pub(super) fn publish_terminal(&self, terminal: GraphTerminal) -> bool {
        if self.closed.load(Ordering::Acquire)
            || self
                .terminal_state
                .compare_exchange(
                    TerminalState::Open as u8,
                    TerminalState::Writing as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
        {
            return false;
        }
        // Open -> Writing is the one terminal initializer. Ready's release precedes every terminal
        // read; `GraphTerminal` is Copy and no destructive move occurs.
        cell::write(&self.terminal, |storage| {
            // SAFETY: Open -> Writing is the one terminal initializer; this pointer never
            // escapes the callback.
            unsafe { (*storage).write(terminal) };
        });
        self.terminal_state
            .store(TerminalState::Ready as u8, Ordering::Release);
        self.consumer_wake.wake();
        true
    }

    pub(super) fn terminal(&self) -> Result<Option<GraphTerminal>, StreamCapacityError> {
        match self.terminal_state() {
            TerminalState::Open | TerminalState::Writing => Ok(None),
            TerminalState::Ready => {
                // Ready acquire observes the one terminal initialization above. The read stays
                // inside Loom's access callback, so a concurrent invalid read is modeled.
                Ok(Some(cell::read(&self.terminal, |storage| {
                    // SAFETY: Ready acquire observes the one terminal initialization above.
                    unsafe { (*storage).assume_init_read() }
                })))
            }
            TerminalState::Corrupt => Err(StreamCapacityError::CorruptState {
                cell: LeaseStateCell::Terminal,
            }),
        }
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        for slot in &self.slots[..self.selected_len] {
            slot.close();
        }
        self.consumer_wake.wake();
        self.producer_wake.wake();
    }

    pub(super) fn load(&self) -> LeaseLoad {
        let mut load = LeaseLoad { edges: 0, bytes: 0 };
        for slot in &self.slots[..self.selected_len] {
            if matches!(slot.phase(), SlotPhase::Ready | SlotPhase::Reading) {
                let edges = usize::from(slot.length());
                load.edges += edges;
                load.bytes += edges * size_of::<GraphEdge>();
            }
        }
        load
    }
}

// SAFETY: endpoint borrowing prevents duplicate logical writers/readers; atomics protect the raw
// cell hand-off across a scoped thread.
unsafe impl Sync for SharedLease {}

#[cfg(test)]
mod trait_contracts {
    use super::{EdgeSlot, SharedLease, WakeCell};

    fn require_send<Type: Send>() {}
    fn require_sync<Type: Sync>() {}

    #[test]
    fn atomic_payload_boundaries_are_shareable_by_state_protocol() {
        require_send::<EdgeSlot>();
        require_send::<SharedLease>();
        require_send::<WakeCell>();
        require_sync::<EdgeSlot>();
        require_sync::<SharedLease>();
        require_sync::<WakeCell>();
    }
}

#[cfg(all(test, feature = "loom-model"))]
mod loom_tests {
    use core::sync::atomic::Ordering;

    use backend_semantic::ir::EntityId;
    use loom::{
        sync::{Arc, atomic::AtomicBool},
        thread,
    };
    use server_index_vocabulary::IndexSnapshotId;

    use super::{EdgeSlot, SlotPhase};
    use crate::{GraphAuthority, GraphEdge, PartitionId, ProjectionId};

    const INITIALIZER_STACK_BYTES: usize = 64 * 1024;

    fn edge() -> GraphEdge {
        let authority = GraphAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"graph lease loom authority"),
            ProjectionId::new(1),
        );
        GraphEdge::new(
            authority,
            PartitionId::new(0),
            EntityId::new(1),
            EntityId::new(2),
        )
    }

    #[test]
    fn ready_release_acquire_lends_exact_initialized_edge() {
        loom::model(|| {
            let slot = Arc::new(EdgeSlot::new());
            let writer_slot = Arc::clone(&slot);
            let writer = thread::spawn(move || {
                assert!(writer_slot.try_begin_write());
                assert!(writer_slot.publish(&[edge()]));
            });
            let reader_slot = Arc::clone(&slot);
            let reader = thread::spawn(move || {
                while reader_slot.phase() != SlotPhase::Ready {
                    thread::yield_now();
                }
                assert!(reader_slot.try_begin_read());
                let borrowed = reader_slot.borrowed();
                assert_eq!(&*borrowed, &[edge()]);
                drop(borrowed);
                reader_slot.release_read();
                assert_eq!(reader_slot.phase(), SlotPhase::Vacant);
            });
            assert!(writer.join().is_ok());
            assert!(reader.join().is_ok());
        });
    }

    #[test]
    fn close_wins_against_an_inflight_writer_without_exposing_payload() {
        loom::model(|| {
            let slot = Arc::new(EdgeSlot::new());
            let writing = Arc::new(AtomicBool::new(false));
            let writer_slot = Arc::clone(&slot);
            let writer_writing = Arc::clone(&writing);
            let writer = thread::spawn(move || {
                assert!(writer_slot.try_begin_write());
                writer_writing.store(true, Ordering::Release);
                while writer_slot.phase() == SlotPhase::Writing {
                    thread::yield_now();
                }
                assert!(!writer_slot.publish(&[edge()]));
            });
            let closer_slot = Arc::clone(&slot);
            let closer_writing = Arc::clone(&writing);
            let closer = thread::spawn(move || {
                while !closer_writing.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                closer_slot.close();
                assert_eq!(closer_slot.phase(), SlotPhase::Closed);
            });
            assert!(writer.join().is_ok());
            assert!(closer.join().is_ok());
        });
    }

    #[test]
    fn terminal_publish_close_and_read_have_one_valid_outcome() {
        let mut model = loom::model::Builder::new();
        model.max_branches = 128;
        model.max_permutations = Some(32);
        model.check(|| {
            let authority = GraphAuthority::new(
                IndexSnapshotId::from_canonical_bytes(b"terminal loom authority"),
                ProjectionId::new(2),
            );
            let selected: [Option<PartitionId>; crate::MAX_PARTITIONS] =
                core::array::from_fn(|index| (index == 0).then_some(PartitionId::new(0)));
            let (sender, receiver) = loom::sync::mpsc::channel();
            let initializer = loom::thread::Builder::new()
                .stack_size(INITIALIZER_STACK_BYTES)
                .spawn(move || {
                    let shared = Arc::new(super::SharedLease::new(
                        authority,
                        selected,
                        1,
                        super::LeaseCapacity {
                            edges_per_partition: 1,
                            bytes_per_partition: core::mem::size_of::<GraphEdge>(),
                        },
                    ));
                    sender.send(shared).is_ok()
                });
            assert!(initializer.is_ok());
            let Ok(initializer) = initializer else {
                return;
            };
            assert!(initializer.join().is_ok());
            let shared = receiver.recv();
            assert!(shared.is_ok());
            let Ok(shared) = shared else {
                return;
            };

            let publisher_shared = Arc::clone(&shared);
            let publisher = thread::spawn(move || {
                publisher_shared.publish_terminal(super::GraphTerminal::Complete { authority })
            });
            let closer_shared = Arc::clone(&shared);
            let closer = thread::spawn(move || closer_shared.close());
            let reader_shared = Arc::clone(&shared);
            let reader = thread::spawn(move || match reader_shared.terminal() {
                Ok(None) => true,
                Ok(Some(super::GraphTerminal::Complete {
                    authority: observed,
                })) => observed == authority,
                Ok(Some(_)) | Err(_) => false,
            });
            assert!(publisher.join().is_ok());
            assert!(closer.join().is_ok());
            assert!(matches!(reader.join(), Ok(true)));

            let valid = match shared.terminal() {
                Ok(None) => true,
                Ok(Some(super::GraphTerminal::Complete {
                    authority: observed,
                })) => observed == authority,
                Ok(Some(_)) | Err(_) => false,
            };
            assert!(valid);
        });
    }
}
