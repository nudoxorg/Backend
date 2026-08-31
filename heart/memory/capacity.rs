//! Defines capacity behavior for `heart-memory`, whose purpose is to store immutable objects in bounded caller-selected memory.
//! This module owns the capacity invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use alloc::collections::TryReserveError;
use core::{mem::size_of, ops::Deref};
use thiserror::Error;

macro_rules! scalar_unit {
    ($(#[$attribute:meta])* $name:ident($scalar:ty)) => {
        $(#[$attribute])*
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(transparent)]
        pub struct $name($scalar);

        impl From<$scalar> for $name {
            fn from(value: $scalar) -> Self {
                Self(value)
            }
        }

        impl Deref for $name {
            type Target = $scalar;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}

scalar_unit!(
    /// Exact initialized object-slot occupancy.
    OccupiedSlots(usize)
);
scalar_unit!(
    /// Exact immutable payload bytes currently retained.
    RetainedBytes(u64)
);
scalar_unit!(
    /// Fixed content-index bucket cardinality.
    IndexBucketCount(usize)
);
scalar_unit!(
    /// Fixed content-index storage bytes excluding allocator bookkeeping.
    IndexMetadataBytes(usize)
);

/// Exact retained-object payload budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ByteCapacity(u64);
impl From<u64> for ByteCapacity {
    fn from(bytes: u64) -> Self {
        Self(bytes)
    }
}
impl Deref for ByteCapacity {
    type Target = u64;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ByteCapacity {
    /// Remaining bytes under a store-owned counter proven not to exceed this budget.
    pub(crate) const fn remaining_after(self, current: RetainedBytes) -> u64 {
        self.0 - current.0
    }
}

/// Exact retained-object slot budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct SlotCapacity(u32);
impl From<u32> for SlotCapacity {
    fn from(slots: u32) -> Self {
        Self(slots)
    }
}
impl Deref for SlotCapacity {
    type Target = u32;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SlotCapacity {
    /// One newly admitted immutable object consumes one typed slot.
    pub const ONE: Self = Self(1);
}

/// Required object payload and index slot budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreCapacity {
    /// Exact retained payload byte budget.
    pub bytes: ByteCapacity,
    /// Exact immutable object slot budget.
    pub slots: SlotCapacity,
}

const BUCKETS_PER_ENTRY: usize = 2;
const MIN_BUCKETS: usize = 1;

#[cfg(target_pointer_width = "64")]
pub(crate) const MAX_INDEX_SLOTS: u32 = 1_u32 << 30;

#[cfg(target_pointer_width = "32")]
pub(crate) const MAX_INDEX_SLOTS: u32 = 1_u32 << 27;

#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!("heart-memory requires 32-bit or 64-bit usize coordinates");

/// One validated geometry shared by retained entries and their content index.
/// It is built once before either allocation, so their capacities cannot drift.
pub(crate) struct StoreLayout {
    pub(crate) entry_capacity: usize,
    pub(crate) bucket_count: IndexBucketCount,
    pub(crate) index_bytes: IndexMetadataBytes,
}

impl StoreLayout {
    pub(crate) fn new(capacity: StoreCapacity) -> Result<Self, StoreInitError> {
        if *capacity.slots > MAX_INDEX_SLOTS {
            return Err(StoreInitError::IndexCapacityTooLarge {
                requested: capacity.slots,
                maximum: MAX_INDEX_SLOTS.into(),
            });
        }
        #[allow(
            clippy::as_conversions,
            reason = "u32 slot counts are lossless on the crate's target-gated 32/64-bit usize architectures"
        )]
        let entry_capacity = *capacity.slots as usize;
        // `MAX_INDEX_SLOTS` makes this multiplication and the power-of-two
        // round-up representable on every supported target.
        let requested_buckets = entry_capacity * BUCKETS_PER_ENTRY;
        let bucket_count = if requested_buckets == 0 {
            MIN_BUCKETS
        } else {
            requested_buckets.next_power_of_two()
        };
        Ok(Self {
            entry_capacity,
            bucket_count: bucket_count.into(),
            index_bytes: (bucket_count * size_of::<u32>()).into(),
        })
    }

    pub(crate) const fn bucket_mask(&self) -> usize {
        self.bucket_count.0 - MIN_BUCKETS
    }
}

/// Failure to construct all bounded allocations before store use.
#[derive(Debug, Error)]
pub enum StoreInitError {
    /// Slot count exceeds the fixed half-load index geometry on this architecture.
    #[error("slot capacity {requested:?} exceeds index-safe maximum {maximum:?}")]
    IndexCapacityTooLarge {
        /// Rejected requested object slots.
        requested: SlotCapacity,
        /// Largest slot count whose typed half-load layout is representable.
        maximum: SlotCapacity,
    },
    /// Exact retained-object index reservation failed with allocator source.
    #[error("could not reserve retained-object entries")]
    EntryReservation(#[source] TryReserveError),
    /// Exact open-addressed bucket reservation failed with allocator source.
    #[error("could not reserve content-index buckets")]
    BucketReservation(#[source] TryReserveError),
    /// A compile-time inline entry policy cannot hold the declared slot budget.
    #[error("inline entry backing holds {available} values but requires {required}")]
    InlineEntriesTooSmall {
        /// Declared entry cardinality.
        required: usize,
        /// Compile-time inline entry capacity.
        available: usize,
    },
    /// A compile-time inline index policy cannot hold the derived bucket geometry.
    #[error("inline bucket backing holds {available} values but requires {required}")]
    InlineBucketsTooSmall {
        /// Derived bucket cardinality.
        required: usize,
        /// Compile-time inline bucket capacity.
        available: usize,
    },
}

/// Exact current payload/slot counters. Byte values exclude index and allocator overhead.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoreStats {
    /// Immutable store budgets.
    pub capacity: StoreCapacity,
    /// Fixed content-index bucket count allocated before the first write.
    pub index_buckets: IndexBucketCount,
    /// Fixed content-index bytes allocated before the first write.
    pub index_bytes: IndexMetadataBytes,
    /// Exact currently retained payload bytes.
    pub retained_bytes: RetainedBytes,
    /// Exact currently occupied object slots.
    pub occupied_slots: OccupiedSlots,
}
