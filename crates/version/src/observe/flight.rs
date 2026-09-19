//! Defines flight behavior for `observe`, whose purpose is to define allocation-free observation events and probe capabilities.
//! This module owns the flight invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed-capacity O(1) recorder with static retention semantics.

use arrayvec::ArrayVec;
use core::marker::PhantomData;

use super::Probe;

/// Static retention policy selected as part of a recorder's concrete type.
///
/// Implementations decide what happens at capacity without requiring a dynamic policy branch in
/// a hot observation call. The two supplied policy markers have no runtime state.
pub trait RetentionPolicy {
    /// Retains or discards an event when a recorder is full.
    fn record_full<Event, Build, const CAP: usize>(
        recorder: &mut FlightRecorder<Event, Self, CAP>,
        build: Build,
    ) -> RecordingDisposition
    where
        Self: Sized,
        Build: FnOnce() -> Event;
}

/// Full recorders retain their existing tail and discard incoming events lazily.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropNewest {}

impl RetentionPolicy for DropNewest {
    fn record_full<Event, Build, const CAP: usize>(
        _recorder: &mut FlightRecorder<Event, Self, CAP>,
        _build: Build,
    ) -> RecordingDisposition
    where
        Build: FnOnce() -> Event,
    {
        RecordingDisposition::DroppedNewest
    }
}

/// Full recorders evict exactly their oldest retained event before appending the incoming event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverwriteOldest {}

impl RetentionPolicy for OverwriteOldest {
    #[allow(
        clippy::indexing_slicing,
        reason = "a full nonzero ArrayVec has CAP initialized slots and head is advanced modulo CAP"
    )]
    fn record_full<Event, Build, const CAP: usize>(
        recorder: &mut FlightRecorder<Event, Self, CAP>,
        build: Build,
    ) -> RecordingDisposition
    where
        Build: FnOnce() -> Event,
    {
        if CAP == 0 {
            return RecordingDisposition::DroppedNewest;
        }
        let oldest = recorder.head;
        let replaced = core::mem::replace(&mut recorder.slots[oldest], build());
        recorder.head = FlightRecorder::<Event, Self, CAP>::advance(oldest);
        drop(replaced);
        RecordingDisposition::ReplacedOldest
    }
}

/// Result of attempting to record one event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingDisposition {
    /// The event was appended without evicting another event.
    Retained,
    /// The event was discarded because this recorder has no representable slot.
    DroppedNewest,
    /// The oldest event was evicted and the incoming event was appended.
    ReplacedOldest,
}

/// A bounded, allocation-free ring of caller-owned observations.
///
/// `Policy` and `CAP` are both static parts of the type. `record_with` therefore has a single
/// policy path after monomorphization, while overwrites replace one slot in O(1) time. Semantic
/// owner crates declare their own closed event families and use this generic retention mechanism.
pub struct FlightRecorder<Event, Policy, const CAP: usize> {
    slots: ArrayVec<Event, CAP>,
    head: usize,
    policy: PhantomData<Policy>,
}

impl<Event, Policy, const CAP: usize> FlightRecorder<Event, Policy, CAP> {
    /// Creates an empty recorder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: ArrayVec::new_const(),
            head: 0,
            policy: PhantomData,
        }
    }

    /// Returns the maximum number of retained events.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        CAP
    }

    /// Returns the number of retained events.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.slots.len()
    }

    /// Reports whether no event is currently retained.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Iterates the exact retained tail in chronological order, including after ring wraparound.
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        let (prefix, suffix) = self.slots.as_slice().split_at(self.head);
        suffix.iter().chain(prefix)
    }

    fn append(&mut self, event: Event) -> RecordingDisposition {
        if self.slots.try_push(event).is_ok() {
            RecordingDisposition::Retained
        } else {
            RecordingDisposition::DroppedNewest
        }
    }

    const fn advance(index: usize) -> usize {
        if index + 1 == CAP { 0 } else { index + 1 }
    }
}

impl<Event, Policy: RetentionPolicy, const CAP: usize> FlightRecorder<Event, Policy, CAP> {
    /// Evaluates and records one event according to this recorder's static bounded policy.
    ///
    /// Full `DropNewest` recorders and all zero-capacity recorders preserve builder laziness.
    pub fn record<Build>(&mut self, build: Build) -> RecordingDisposition
    where
        Build: FnOnce() -> Event,
    {
        if self.slots.len() < CAP {
            self.append(build())
        } else {
            Policy::record_full(self, build)
        }
    }
}

impl<Event, Policy: RetentionPolicy, const CAP: usize> Probe<Event>
    for FlightRecorder<Event, Policy, CAP>
{
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> Event,
    {
        let _disposition = Self::record(self, build);
    }
}

impl<Event, Policy, const CAP: usize> Default for FlightRecorder<Event, Policy, CAP> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use core::{
        cell::{Cell, RefCell},
        mem::size_of,
    };
    use std::{rc::Rc, vec::Vec};

    use super::{DropNewest, FlightRecorder, OverwriteOldest, RecordingDisposition};
    use super::Probe;

    #[test]
    fn noop_and_drop_newest_do_not_construct_event_fields() {
        let constructed = Cell::new(0_usize);
        let mut noop = ();
        noop.record_with(|| {
            constructed.set(constructed.get() + 1);
            [7_u8; 4096]
        });
        assert_eq!(constructed.get(), 0);

        let mut recorder = FlightRecorder::<u8, DropNewest, 1>::new();
        assert_eq!(recorder.record(|| 2), RecordingDisposition::Retained);
        assert_eq!(
            recorder.record(|| {
                constructed.set(constructed.get() + 1);
                3
            }),
            RecordingDisposition::DroppedNewest
        );
        assert_eq!(constructed.get(), 0);
        assert_eq!(recorder.events().next(), Some(&2));
    }

    #[test]
    fn overwrite_oldest_wraps_without_shifting_and_keeps_exact_order() {
        let mut recorder = FlightRecorder::<u8, OverwriteOldest, 3>::new();
        for event in [1, 2, 3] {
            assert_eq!(recorder.record(|| event), RecordingDisposition::Retained);
        }
        assert_eq!(recorder.record(|| 4), RecordingDisposition::ReplacedOldest);
        assert_eq!(recorder.record(|| 5), RecordingDisposition::ReplacedOldest);
        let ordered: [Option<&u8>; 4] = [
            recorder.events().next(),
            recorder.events().nth(1),
            recorder.events().nth(2),
            recorder.events().nth(3),
        ];
        assert_eq!(ordered, [Some(&3), Some(&4), Some(&5), None]);
    }

    #[test]
    fn capacity_zero_and_one_preserve_exact_drop_and_replacement_rules() {
        let invoked = Cell::new(false);
        let mut zero = FlightRecorder::<u8, OverwriteOldest, 0>::new();
        assert_eq!(
            zero.record(|| {
                invoked.set(true);
                1
            }),
            RecordingDisposition::DroppedNewest
        );
        assert!(!invoked.get());

        let mut one = FlightRecorder::<u8, OverwriteOldest, 1>::new();
        assert_eq!(one.record(|| 1), RecordingDisposition::Retained);
        assert_eq!(one.record(|| 2), RecordingDisposition::ReplacedOldest);
        assert_eq!(one.events().next(), Some(&2));
    }

    #[test]
    fn dense_storage_layout_and_overwrite_drop_order_are_exact() {
        assert_eq!(size_of::<FlightRecorder<u64, OverwriteOldest, 3>>(), 40);
        let drops = Rc::new(RefCell::new(Vec::new()));
        {
            let mut recorder = FlightRecorder::<DropEvent, OverwriteOldest, 2>::new();
            for id in [1, 2, 3] {
                let log = Rc::clone(&drops);
                let disposition = recorder.record(|| DropEvent {
                    id,
                    panic_on_drop: false,
                    log,
                });
                assert_eq!(
                    disposition,
                    if id == 3 {
                        RecordingDisposition::ReplacedOldest
                    } else {
                        RecordingDisposition::Retained
                    }
                );
            }
            assert_eq!(drops.borrow().as_slice(), &[1]);
        }
        assert_eq!(drops.borrow().as_slice(), &[1, 3, 2]);
    }

    #[test]
    fn panicking_eviction_keeps_the_incoming_event_owned_by_the_ring() {
        let drops = Rc::new(RefCell::new(Vec::new()));
        let mut recorder = FlightRecorder::<DropEvent, OverwriteOldest, 1>::new();
        let first_log = Rc::clone(&drops);
        assert_eq!(
            recorder.record(|| DropEvent {
                id: 1,
                panic_on_drop: true,
                log: first_log,
            }),
            RecordingDisposition::Retained
        );
        let second_log = Rc::clone(&drops);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            recorder.record(|| DropEvent {
                id: 2,
                panic_on_drop: false,
                log: second_log,
            })
        }));
        assert!(unwind.is_err());
        assert_eq!(recorder.events().next().map(|event| event.id), Some(2));
        assert_eq!(drops.borrow().as_slice(), &[1]);
        drop(recorder);
        assert_eq!(drops.borrow().as_slice(), &[1, 2]);
    }

    struct DropEvent {
        id: u8,
        panic_on_drop: bool,
        log: Rc<RefCell<Vec<u8>>>,
    }

    impl Drop for DropEvent {
        #[allow(
            clippy::panic,
            reason = "this one test fixture proves replacement ownership across a panicking event destructor"
        )]
        fn drop(&mut self) {
            self.log.borrow_mut().push(self.id);
            if self.panic_on_drop {
                std::panic::panic_any("adversarial recorder event drop");
            }
        }
    }
}
