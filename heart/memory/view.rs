//! Defines view behavior for `heart-memory`, whose purpose is to store immutable objects in bounded caller-selected memory.
//! This module owns the view invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use heart_object::ObjectRef;

/// Borrowed immutable object view whose lifetime is visibly tied to the store.
pub struct StoredObjectView<'store, DomainTag> {
    /// Admitted immutable descriptor.
    pub reference: ObjectRef<DomainTag>,
    /// Retained immutable bytes without allocation, lock, or refcount.
    pub bytes: &'store [u8],
}

/// Bounded open-address lookup evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lookup {
    /// The object identity is resident after this many occupied-bucket probes.
    Present {
        /// Occupied-bucket probes performed by this lookup.
        probes: usize,
    },
    /// The object identity is not resident after this many occupied-bucket probes.
    Absent {
        /// Occupied-bucket probes performed by this lookup.
        probes: usize,
    },
}
