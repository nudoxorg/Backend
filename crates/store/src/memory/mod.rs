//! The `backend_store::memory` module stores immutable objects in bounded caller-selected memory.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Owner-oriented, bounded first-write-wins memory storage.

mod backing;
mod capacity;
mod index;
mod store;
mod view;

pub use self::backing::{HeapBacking, InlineBacking};
pub use self::capacity::{
    ByteCapacity, IndexBucketCount, IndexMetadataBytes, OccupiedSlots, RetainedBytes, SlotCapacity,
    StoreCapacity, StoreInitError, StoreStats,
};
pub use self::store::{
    INLINE_BUCKET_CAPACITY, INLINE_ENTRY_CAPACITY, InlineMemoryStore, InsertOutcome,
    LeanMemoryStore, MemoryStore, RejectedInsert, Store, StoreAdmission, StoreError,
    StoreProbeEvent,
};
pub use self::view::{Lookup, StoredObjectView};

#[cfg(test)]
mod tests;
