//! Inline SPSC slots and terminal storage; this is the only raw graph-edge memory boundary.

#[cfg(not(all(test, feature = "loom-model")))]
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, size_of},
};
#[cfg(all(test, feature = "loom-model"))]
use loom::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::{
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
    edges: [UnsafeCell<MaybeUninit<GraphEdge>>; MAX_EDGES_PER_PARTITION],
}

impl EdgeSlot {
    fn new() -> Self {
        Self {
            phase: AtomicU8::new(SlotPhase::Vacant as u8),
            length: AtomicU8::new(0),
            edges: core::array::from_fn(|_| UnsafeCell::new(MaybeUninit::uninit())),
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
        // SAFETY: this producer owns Vacant -> Writing. A consumer reads only after the release
        // publication below. `GraphEdge` is Copy, so a closed unpublished slot has no destructor.
        for (storage, edge) in self.edges.iter().zip(edges.iter().copied()) {
            // SAFETY: `Writing` grants this unique producer the only mutable raw slot access.
            unsafe { (*storage.get()).write(edge) };
        }
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

    pub(super) fn borrowed(&self) -> &[GraphEdge] {
        let length = usize::from(self.length.load(Ordering::Relaxed));
        // SAFETY: Ready -> Reading acquired this producer's release. The batch borrow prevents a
        // second consumer poll from returning this slot to Vacant while the slice is reachable.
        unsafe { core::slice::from_raw_parts(self.edges.as_ptr().cast::<GraphEdge>(), length) }
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
        // SAFETY: Open -> Writing is the one terminal initializer. Ready's release precedes every
        // terminal read; `GraphTerminal` is Copy and no destructive move occurs.
        unsafe { (*self.terminal.get()).write(terminal) };
        self.terminal_state
            .store(TerminalState::Ready as u8, Ordering::Release);
        self.consumer_wake.wake();
        true
    }

    pub(super) fn terminal(&self) -> Result<Option<GraphTerminal>, StreamCapacityError> {
        match self.terminal_state() {
            TerminalState::Open | TerminalState::Writing => Ok(None),
            TerminalState::Ready => {
                // SAFETY: Ready acquire observes the one terminal initialization above.
                Ok(Some(unsafe { (*self.terminal.get()).assume_init_read() }))
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

#[cfg(all(test, feature = "loom-model"))]
mod loom_tests {
    use core::sync::atomic::Ordering;

    use loom::{
        sync::{Arc, atomic::AtomicBool},
        thread,
    };
    use nudox_index_vocab::IndexSnapshotId;
    use nudox_ir_vocab::EntityId;

    use super::{EdgeSlot, SlotPhase};
    use crate::{GraphAuthority, GraphEdge, PartitionId, ProjectionId};

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
            let published = Arc::new(AtomicBool::new(false));
            let writer_slot = Arc::clone(&slot);
            let writer_published = Arc::clone(&published);
            let writer = thread::spawn(move || {
                assert!(writer_slot.try_begin_write());
                assert!(writer_slot.publish(&[edge()]));
                writer_published.store(true, Ordering::Release);
            });
            let reader_slot = Arc::clone(&slot);
            let reader_published = Arc::clone(&published);
            let reader = thread::spawn(move || {
                while !reader_published.load(Ordering::Acquire) {
                    thread::yield_now();
                }
                assert!(reader_slot.try_begin_read());
                assert_eq!(reader_slot.borrowed(), &[edge()]);
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
}
