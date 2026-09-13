//! Defines locality behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Generation-local placement facts backed by one canonical byte artifact.

use alloc::collections::TryReserveError;
use alloc::vec::Vec;
use core::{borrow::Borrow, marker::PhantomData, mem::size_of, ops::Deref};

use heart_identity::GenerationId;
use heart_object::{ObjectRef, ProviderSet, RemoteBase};

use crate::entry::EntryKey;
use crate::packed::{GenerationRoot, RootEntryCount, RowIndex};

mod artifact;
mod cursor;
mod error;
mod view;

pub(crate) use artifact::LocalityEncoder;
pub use artifact::{
    LocalityError, LocalityLayout, LocalityLayoutView, LocalityValidator, LocalityWriteError,
    PreparedLocality, ValidatedLocality, ValidatedLocalityFacts, with_validated_locality,
};
pub use cursor::{LocalityLookupWork, LocalityScanWork};
pub use error::SelectedOrdinalBufferError;
pub use view::{
    GenerationScan, GenerationView, MeasuredGenerationLookup, MeasuredGenerationScan,
    SelectedGeneration,
};

pub(crate) use cursor::LocalityCursor;

/// Honest placement state composed with one semantic root entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Locality<DomainTag> {
    /// No exceptional locality fact exists; the descriptor is locally resident.
    Resident,
    /// Descriptor is absent locally but has a non-empty eligible provider set.
    Promised(ProviderSet),
    /// Descriptor is locally replaced with exact named remote-base evidence.
    Overlaid(RemoteBase<DomainTag>),
}

/// A root-issued compact coordinate for one immutable canonical row.
///
/// Its construction is restricted to [`GenerationRoot::locality_row`], which
/// binds the coordinate to the root generation, root cardinality, and key. It
/// is deliberately not a transparent integer: callers cannot direct an
/// infallible locality writer at an arbitrary row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalityRow<DomainTag> {
    /// Stable semantic key at this immutable root row.
    pub key: EntryKey,
    generation: GenerationId,
    root_count: RootEntryCount,
    index: RowIndex,
    domain: PhantomData<fn() -> DomainTag>,
}

/// Non-resident placement admitted to canonical sparse locality input.
///
/// Resident locality is deliberately absent: it is represented by the lack
/// of an exception row and therefore cannot spend an artifact entry.
#[derive(Debug, Eq, PartialEq)]
pub enum NonResident<DomainTag> {
    /// Descriptor is absent locally but has a non-empty eligible provider set.
    Promised(ProviderSet),
    /// Descriptor is locally replaced with exact named remote-base evidence.
    Overlaid(RemoteBase<DomainTag>),
}

impl<DomainTag> Copy for NonResident<DomainTag> {}

impl<DomainTag> Clone for NonResident<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

/// One sparse, root-proven non-resident locality exception.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalityException<DomainTag> {
    /// Root-proven row coordinate and semantic key.
    pub row: LocalityRow<DomainTag>,
    /// Explicit non-resident placement.
    pub placement: NonResident<DomainTag>,
}

impl<DomainTag> LocalityException<DomainTag> {
    /// Pairs a root-issued row coordinate with one non-resident placement.
    #[must_use]
    pub const fn new(row: LocalityRow<DomainTag>, placement: NonResident<DomainTag>) -> Self {
        Self { row, placement }
    }
}

impl<DomainTag> GenerationRoot<DomainTag> {
    /// Issues the unforgeable locality coordinate for one semantic key.
    #[must_use]
    pub fn locality_row(&self, key: EntryKey) -> Option<LocalityRow<DomainTag>> {
        self.rows
            .binary_search_by_key(&key, |row| row.key)
            .ok()
            .map(|position| LocalityRow {
                key,
                generation: self.id,
                root_count: self.entry_count,
                index: RowIndex::from_arena_position(position),
                domain: PhantomData,
            })
    }
}

/// Semantic root entry composed with its separately encoded placement state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationEntry<DomainTag> {
    /// Stable semantic key.
    pub key: EntryKey,
    /// Containing key, when non-root.
    pub parent: Option<EntryKey>,
    /// Canonical immutable descriptor.
    pub object: ObjectRef<DomainTag>,
    /// Honest non-semantic placement state.
    pub locality: Locality<DomainTag>,
}

/// Private compact coordinate issued by one selected closure only.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
struct SelectedOrdinal(u32);

impl SelectedOrdinal {
    const fn from_compact(position: u32) -> Self {
        Self(position)
    }

    #[allow(
        clippy::as_conversions,
        reason = "selected positions come from a root-bounded compact u32 sequence"
    )]
    const fn array_index(self) -> usize {
        self.0 as usize
    }
}

/// Reusable caller-owned compact storage for sparse selected ordinals.
pub struct SelectedOrdinalBuffer {
    positions: Vec<u32>,
    pub(crate) capacity: SelectedCount,
}

impl SelectedOrdinalBuffer {
    /// Reserves storage for a declared selected cardinality.
    ///
    /// # Errors
    ///
    /// Returns the allocator reservation source before retaining any position.
    pub fn new(capacity: SelectedCount) -> Result<Self, TryReserveError> {
        let mut positions = Vec::new();
        positions.try_reserve_exact(capacity.array_capacity())?;
        Ok(Self {
            positions,
            capacity,
        })
    }

    /// Returns retained ordinal bytes excluding allocator bookkeeping.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.positions.capacity() * size_of::<u32>()
    }

    /// Returns whether no positions are retained for the latest plan.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        self.positions.clear();
    }

    fn push(&mut self, ordinal: SelectedOrdinal) {
        self.positions.push(ordinal.0);
    }
}

/// Borrowed ordinal list structurally bound to the selection that produced it.
pub struct SelectedOrdinals<'selection, 'storage, DomainTag> {
    pub(crate) selected: SelectedGeneration<'selection, DomainTag>,
    pub(crate) positions: &'storage [u32],
}

const _: [(); size_of::<SelectedOrdinal>()] = [(); size_of::<u32>()];

/// Semantic selected descriptor count.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct SelectedCount(u32);

impl From<u32> for SelectedCount {
    fn from(count: u32) -> Self {
        Self(count)
    }
}
impl From<RootEntryCount> for SelectedCount {
    fn from(count: RootEntryCount) -> Self {
        Self(u32::from(count))
    }
}
impl From<SelectedCount> for u32 {
    fn from(count: SelectedCount) -> Self {
        count.0
    }
}
impl Deref for SelectedCount {
    type Target = u32;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl AsRef<u32> for SelectedCount {
    fn as_ref(&self) -> &u32 {
        self
    }
}
impl Borrow<u32> for SelectedCount {
    fn borrow(&self) -> &u32 {
        self
    }
}
impl SelectedCount {
    /// Empty selected count.
    pub const ZERO: Self = Self(0);
    /// Checked independent count increment.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(next) => Some(Self(next)),
            None => None,
        }
    }
    #[allow(
        clippy::as_conversions,
        reason = "the root target gate admits 32/64-bit native array coordinates"
    )]
    pub(crate) const fn array_capacity(self) -> usize {
        self.0 as usize
    }
}
