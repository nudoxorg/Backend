//! Defines entry behavior for `backend-store`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the entry invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use backend_version::object::ObjectRef;
use thiserror::Error;

/// Stable semantic key within one generation root.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct EntryKey(u64);

impl From<u64> for EntryKey {
    fn from(raw: u64) -> Self {
        Self(raw)
    }
}

impl Deref for EntryKey {
    type Target = u64;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Inclusive key projection within a generation root.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntryRange(EntryRangeBounds);

/// Immutable bounds exposed by a validated inclusive entry range.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EntryRangeBounds {
    /// First included key.
    pub start: EntryKey,
    /// Final included key.
    pub end: EntryKey,
}

impl Deref for EntryRange {
    type Target = EntryRangeBounds;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl EntryRange {
    /// Validates an inclusive canonical key range.
    ///
    /// # Errors
    ///
    /// Returns [`EntryRangeError::Inverted`] when `start` follows `end`.
    pub const fn new(start: EntryKey, end: EntryKey) -> Result<Self, EntryRangeError> {
        if start.0 <= end.0 {
            Ok(Self(EntryRangeBounds { start, end }))
        } else {
            Err(EntryRangeError::Inverted { start, end })
        }
    }
    /// Returns whether this range includes `key`.
    #[must_use]
    pub const fn contains(self, key: EntryKey) -> bool {
        self.0.start.0 <= key.0 && key.0 <= self.0.end.0
    }
}

/// Invalid key projection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EntryRangeError {
    /// The start key was greater than the end key.
    #[error("entry range start {start:?} follows end {end:?}")]
    Inverted {
        /// Rejected start.
        start: EntryKey,
        /// Rejected end.
        end: EntryKey,
    },
}

/// Canonical logical entry used for construction and iteration.
///
/// Placement is intentionally absent: providers and overlays belong to the
/// separate generation-bound [`ValidatedLocality`](crate::root::ValidatedLocality)
/// axis and never salt this root's identity.
#[derive(derive_more::Debug)]
pub struct RootEntry<DomainTag> {
    /// Stable semantic key.
    pub key: EntryKey,
    /// Containing semantic key when this is not a hierarchy root.
    pub parent: Option<EntryKey>,
    /// Selected immutable object descriptor.
    pub object: ObjectRef<DomainTag>,
}

impl<DomainTag> Copy for RootEntry<DomainTag> {}
impl<DomainTag> Clone for RootEntry<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<DomainTag> PartialEq for RootEntry<DomainTag> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key && self.parent == other.parent && self.object == other.object
    }
}
impl<DomainTag> Eq for RootEntry<DomainTag> {}
