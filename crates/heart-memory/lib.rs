//! The `heart-memory` crate exists to store immutable objects in bounded caller-selected memory.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![forbid(unsafe_code)]
#![no_std]
//! Owner-oriented, bounded first-write-wins memory storage.

extern crate alloc;

mod backing;
mod capacity;
mod index;
mod store;
mod view;

pub use backing::{HeapBacking, InlineBacking};
pub use capacity::{
    ByteCapacity, IndexBucketCount, IndexMetadataBytes, OccupiedSlots, RetainedBytes, SlotCapacity,
    StoreCapacity, StoreInitError, StoreStats,
};
pub use store::{
    INLINE_BUCKET_CAPACITY, INLINE_ENTRY_CAPACITY, InlineMemoryStore, InsertOutcome,
    LeanMemoryStore, MemoryStore, RejectedInsert, Store, StoreAdmission, StoreError,
    StoreProbeEvent,
};
pub use view::{Lookup, StoredObjectView};

#[cfg(test)]
mod tests;
