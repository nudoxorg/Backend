use alloc::boxed::Box;
use core::{borrow::Borrow, mem::size_of, ops::Deref};

use nudox_id::{ContentId, GenerationId};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef};
use nudox_schema::SchemaId;

use crate::entry::{EntryKey, EntryRange, RootEntry};

pub(crate) const NO_PARENT: u32 = u32::MAX;

/// Builder-validated hierarchy depth in parent edges.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct HierarchyDepth(u32);

impl From<u32> for HierarchyDepth {
    fn from(depth: u32) -> Self {
        Self(depth)
    }
}

impl Deref for HierarchyDepth {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

const _: [(); size_of::<HierarchyDepth>()] = [(); size_of::<u32>()];

/// Semantic count measured in root entries.
///
/// A root's concrete value is set by the builder, but this unit itself is not
/// a provenance capability: it can also name a declared root capacity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct RootEntryCount(u32);

impl From<u32> for RootEntryCount {
    fn from(count: u32) -> Self {
        Self(count)
    }
}

impl From<RootEntryCount> for u32 {
    fn from(count: RootEntryCount) -> Self {
        count.0
    }
}

impl Deref for RootEntryCount {
    type Target = u32;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u32> for RootEntryCount {
    fn as_ref(&self) -> &u32 {
        self
    }
}

impl Borrow<u32> for RootEntryCount {
    fn borrow(&self) -> &u32 {
        self
    }
}

const _: [(); size_of::<RootEntryCount>()] = [(); size_of::<u32>()];

/// Semantic amount measured in retained metadata bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MetadataBytes(usize);

impl From<usize> for MetadataBytes {
    fn from(bytes: usize) -> Self {
        Self(bytes)
    }
}

impl From<MetadataBytes> for usize {
    fn from(bytes: MetadataBytes) -> Self {
        bytes.0
    }
}

impl Deref for MetadataBytes {
    type Target = usize;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<usize> for MetadataBytes {
    fn as_ref(&self) -> &usize {
        self
    }
}

impl Borrow<usize> for MetadataBytes {
    fn borrow(&self) -> &usize {
        self
    }
}

impl MetadataBytes {
    /// Checked addition for independently measured metadata byte amounts.
    #[must_use]
    pub const fn checked_combined(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(bytes) => Some(Self(bytes)),
            None => None,
        }
    }
}

const _: [(); size_of::<MetadataBytes>()] = [(); size_of::<usize>()];

// Packed parents are `u32`; Rust's supported host targets for this crate have
// at least 32-bit `usize` coordinates. Keeping the target assumption explicit
// makes the one compact-to-native conversion below auditable.
#[cfg(not(any(target_pointer_width = "32", target_pointer_width = "64")))]
compile_error!("nudox-root requires at least 32-bit usize coordinates");

/// Validated coordinate into the immutable packed root arena.
///
/// Only root iteration and checked builder output create this type. Keeping it
/// separate from public keys prevents raw offsets from crossing the boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RowIndex(u32);

impl RowIndex {
    /// Projects this compact root coordinate onto this process's exact packed
    /// root array. This is the sole native-index conversion for row access.
    #[allow(
        clippy::as_conversions,
        reason = "the crate's explicit 32/64-bit target gate makes every compact u32 root coordinate a native array index"
    )]
    pub(crate) const fn array_index(self) -> usize {
        self.0 as usize
    }

    /// Returns the packed sparse-route coordinate without narrowing.
    pub(crate) const fn compact(self) -> u32 {
        self.0
    }

    /// Creates a coordinate returned by an arena cursor or binary search.
    /// Both producers derive `position` from this exact frozen row slice.
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "every producer traverses or searches one root whose builder proved its row length fits a compact u32"
    )]
    pub(crate) const fn from_arena_position(position: usize) -> Self {
        Self(position as u32)
    }

    /// Creates the coordinate retained by a validated borrowed wire row.
    /// The validator proves this ordinal is within the typed slice; it is a
    /// distinct provenance bridge from the packed C0 arena coordinate.
    pub(crate) const fn from_validated_borrowed_root_position(position: usize) -> Self {
        Self::from_arena_position(position)
    }

    /// Decodes a compact parent only after construction verified it against the
    /// exact immutable row arena.
    #[allow(
        clippy::as_conversions,
        reason = "the explicit 32/64-bit target gate proves every u32 parent fits usize"
    )]
    const fn from_validated_parent(parent: u32) -> Self {
        Self(parent)
    }
}

/// Packed semantic facts retained for every root entry.
///
/// The descriptor's otherwise trailing two-byte padding is made explicit as a
/// private construction state. The final word is phase-reused: while collecting
/// it carries the parent key; after validation it carries packed parent/depth.
#[repr(C)]
pub(crate) struct RootRow<DomainTag> {
    content: ContentId<DomainTag>,
    length: ObjectLength,
    schema: SchemaId,
    kind: ObjectKind,
    state: RootRowPhase,
    pub(crate) key: EntryKey,
    payload: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
/// Phase-reused state stored inside one packed root row during construction.
pub enum RootRowPhase {
    /// Collected row whose payload is a parent key.
    CollectedParent,
    /// Collected hierarchy root.
    CollectedRoot,
    /// Resolved row not yet visited by hierarchy validation.
    Unseen,
    /// Row on the active hierarchy traversal chain.
    Visiting,
    /// Validated row carrying its final parent coordinate and depth.
    Published,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HierarchyState {
    Unseen,
    Visiting,
    Published,
}

impl<DomainTag> Copy for RootRow<DomainTag> {}
impl<DomainTag> Clone for RootRow<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<DomainTag> RootRow<DomainTag> {
    pub(crate) fn collected(entry: RootEntry<DomainTag>) -> Self {
        let RootEntry {
            key,
            parent,
            object,
        } = entry;
        Self {
            content: object.content,
            length: object.length,
            schema: object.schema,
            kind: object.kind,
            state: if parent.is_some() {
                RootRowPhase::CollectedParent
            } else {
                RootRowPhase::CollectedRoot
            },
            key,
            payload: parent.map_or(0, |parent| *parent),
        }
    }
    pub(crate) const fn object(&self) -> ObjectRef<DomainTag> {
        ObjectRef {
            content: self.content,
            length: self.length,
            schema: self.schema,
            kind: self.kind,
        }
    }
    pub(crate) fn collected_parent(&self) -> Result<Option<EntryKey>, RootRowPhase> {
        match self.state {
            RootRowPhase::CollectedRoot => Ok(None),
            RootRowPhase::CollectedParent => Ok(Some(EntryKey::from(self.payload))),
            observed => Err(observed),
        }
    }
    pub(crate) const fn parent(&self) -> u32 {
        debug_assert!(matches!(
            self.state,
            RootRowPhase::Unseen | RootRowPhase::Visiting | RootRowPhase::Published
        ));
        let [first, second, third, fourth, _, _, _, _] = self.payload.to_le_bytes();
        u32::from_le_bytes([first, second, third, fourth])
    }
    pub(crate) const fn depth(&self) -> HierarchyDepth {
        debug_assert!(matches!(self.state, RootRowPhase::Published));
        let [_, _, _, _, first, second, third, fourth] = self.payload.to_le_bytes();
        HierarchyDepth(u32::from_le_bytes([first, second, third, fourth]))
    }
    pub(crate) const fn hierarchy_state(&self) -> Result<HierarchyState, RootRowPhase> {
        match self.state {
            RootRowPhase::Unseen => Ok(HierarchyState::Unseen),
            RootRowPhase::Visiting => Ok(HierarchyState::Visiting),
            RootRowPhase::Published => Ok(HierarchyState::Published),
            observed => Err(observed),
        }
    }
    pub(crate) fn set_unseen(&mut self, parent: u32) {
        self.payload = u64::from(parent);
        self.state = RootRowPhase::Unseen;
    }
    pub(crate) fn mark_visiting(&mut self) {
        debug_assert_eq!(self.state, RootRowPhase::Unseen);
        self.state = RootRowPhase::Visiting;
    }
    pub(crate) fn publish(&mut self, depth: HierarchyDepth) {
        debug_assert_eq!(self.state, RootRowPhase::Visiting);
        let parent = self.parent();
        self.payload = (u64::from(depth.0) << 32) | u64::from(parent);
        self.state = RootRowPhase::Published;
    }
}

/// Immutable canonical root with packed semantic rows only.
pub struct GenerationRoot<DomainTag> {
    pub(crate) facts: GenerationRootFacts,
    /// One allocation owns all fixed-width canonical semantic rows. A boxed
    /// slice prevents later capacity growth from changing immutable storage.
    pub(crate) rows: Box<[RootRow<DomainTag>]>,
}

/// Immutable semantic facts exposed by dereferencing a generation root.
///
/// `GenerationRoot` deliberately implements `Deref` but not `DerefMut`: users
/// can read its canonical identity as `root.id`, yet cannot replace it without
/// reconstructing the validated packed root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationRootFacts {
    /// Canonical semantic identity of the packed rows.
    pub id: GenerationId,
    /// Builder-proven compact row count for this immutable root.
    pub entry_count: RootEntryCount,
    /// Exact maximum simultaneously live input/row payload bytes. The
    /// streaming builder needs only the published row arena; the owned-vector
    /// convenience path also accounts for its caller-shaped input allocation.
    pub construction_peak_bytes: MetadataBytes,
}

impl<DomainTag> Deref for GenerationRoot<DomainTag> {
    type Target = GenerationRootFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag> GenerationRoot<DomainTag> {
    /// Returns count of closure entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }
    /// Returns whether this root has no entries.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// Iterates reconstructed entries in canonical key order without payload reads.
    pub fn closure(&self) -> impl Iterator<Item = RootEntry<DomainTag>> + '_ {
        self.canonical_rows().map(|row| row.entry)
    }
    /// Finds one reconstructed entry by semantic key through binary search.
    #[must_use]
    pub fn get(&self, key: EntryKey) -> Option<RootEntry<DomainTag>> {
        self.rows
            .binary_search_by_key(&key, |row| row.key)
            .ok()
            .map(|index| self.entry_at(RowIndex::from_arena_position(index)))
    }
    /// Returns packed metadata bytes, excluding allocator bookkeeping.
    #[must_use]
    pub fn metadata_bytes(&self) -> MetadataBytes {
        (self.rows.len() * size_of::<RootRow<DomainTag>>()).into()
    }
    /// Returns the precomputed checked hierarchy depth for `key`.
    #[must_use]
    pub fn depth_of(&self, key: EntryKey) -> Option<HierarchyDepth> {
        self.rows
            .binary_search_by_key(&key, |row| row.key)
            .ok()
            .map(|index| self.row(RowIndex::from_arena_position(index)).depth())
    }
    /// Reconstructs an entry at a validated packed-arena coordinate.
    pub(crate) fn entry_at(&self, index: RowIndex) -> RootEntry<DomainTag> {
        let row = self.row(index);
        let parent = match row.parent() {
            NO_PARENT => None,
            parent => Some(self.row(RowIndex::from_validated_parent(parent)).key),
        };
        RootEntry {
            key: row.key,
            parent,
            object: row.object(),
        }
    }
    /// Returns a validated parent coordinate, when this is not a hierarchy root.
    pub(crate) fn parent_index(&self, index: RowIndex) -> Option<RowIndex> {
        let parent = self.row(index).parent();
        if parent == NO_PARENT {
            None
        } else {
            Some(RowIndex::from_validated_parent(parent))
        }
    }
    pub(crate) fn canonical_rows(&self) -> CanonicalRows<'_, DomainTag> {
        CanonicalRows {
            root: self,
            range: CanonicalRange::whole(self.rows.len()),
        }
    }
    pub(crate) fn projected_rows(&self, range: EntryRange) -> CanonicalRows<'_, DomainTag> {
        let next = self.rows.partition_point(|row| row.key < range.start);
        let end = self.rows.partition_point(|row| row.key <= range.end);
        CanonicalRows {
            root: self,
            range: CanonicalRange::projected(next, end),
        }
    }
    /// The sole direct packed-root access boundary.
    ///
    /// `GenerationRootBuilder` caps row count at `u32::MAX - 1`, resolves each
    /// parent from that sorted row set, and freezes the resulting boxed slice.
    /// Cursors/searches are derived from the same slice; therefore every
    /// `RowIndex` reaching this boundary is in range.
    #[allow(
        clippy::indexing_slicing,
        reason = "RowIndex is private proof from this immutable arena's builder, cursor, search, or validated parent relation"
    )]
    fn row(&self, index: RowIndex) -> &RootRow<DomainTag> {
        &self.rows[index.array_index()]
    }
}

/// Borrowed canonical row view that carries its arena-valid coordinate.
pub(crate) struct CanonicalRow<DomainTag> {
    pub(crate) index: RowIndex,
    pub(crate) entry: RootEntry<DomainTag>,
}

/// Borrowed canonical cursor. It creates coordinates only while traversing the
/// exact frozen row slice, so consumers never manufacture raw offsets.
pub(crate) struct CanonicalRows<'root, DomainTag> {
    root: &'root GenerationRoot<DomainTag>,
    range: CanonicalRange,
}

/// Private half-open range proof produced only from this arena's length or
/// partition points. It is the sole producer of cursor coordinates.
struct CanonicalRange {
    next: usize,
    end: usize,
}

impl CanonicalRange {
    const fn whole(end: usize) -> Self {
        Self { next: 0, end }
    }

    const fn projected(next: usize, end: usize) -> Self {
        Self { next, end }
    }

    const fn next_index(&mut self) -> Option<RowIndex> {
        if self.next == self.end {
            return None;
        }
        let index = RowIndex::from_arena_position(self.next);
        self.next += 1;
        Some(index)
    }
}

impl<DomainTag> Iterator for CanonicalRows<'_, DomainTag> {
    type Item = CanonicalRow<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.range.next_index()?;
        Some(CanonicalRow {
            index,
            entry: self.root.entry_at(index),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RootRow;
    use core::mem::{align_of, offset_of, size_of};
    use nudox_id::ObjectDomain;
    #[test]
    fn resident_row_is_exactly_sixty_four_bytes() {
        assert_eq!(size_of::<RootRow<ObjectDomain>>(), 64);
        assert_eq!(align_of::<RootRow<ObjectDomain>>(), 8);
        assert_eq!(offset_of!(RootRow<ObjectDomain>, state), 46);
        assert_eq!(offset_of!(RootRow<ObjectDomain>, key), 48);
        assert_eq!(offset_of!(RootRow<ObjectDomain>, payload), 56);
    }
}
