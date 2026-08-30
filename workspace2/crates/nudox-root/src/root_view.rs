//! Borrowed validation and locality composition for canonical root bytes.

use alloc::{collections::TryReserveError, vec::Vec};
use core::{mem::size_of, ops::Deref, slice};

use nudox_id::{ContentAuthority, ContentAuthorityError, Domain, GenerationId};
use nudox_object::{ObjectDescriptorDecodeError, ObjectKind, ObjectLength, ObjectRef};
use thiserror::Error;
use zerocopy::{FromBytes, TryFromBytes};

use crate::{
    ClosureError, ClosureScratch, EntryKey, EntryRange, GenerationEntry, LocalityError, RootEntry,
    RootEntryCount, ValidatedLocality,
    encode::{ParentWire, ROOT_ROW_RECORD_BYTES, RootHeaderRecord, RootWireRecord},
    locality::LocalityCursor,
    packed::{NO_PARENT, RowIndex},
};

const ROOT_HEADER_BYTES: usize = size_of::<RootHeaderRecord>();

/// Immutable facts decoded from one complete canonical root artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedRootFacts<'bytes> {
    /// Complete canonical bytes retained by reference.
    pub bytes: &'bytes [u8],
    /// Canonical generation identity of those exact bytes.
    pub id: GenerationId,
    /// Validated canonical row count.
    pub entry_count: RootEntryCount,
}

enum BorrowedRootRows<'bytes, DomainTag> {
    Empty(&'bytes [RootWireRecord]),
    Populated {
        rows: &'bytes [RootWireRecord],
        authority: ContentAuthority<DomainTag>,
    },
}

impl<DomainTag> Copy for BorrowedRootRows<'_, DomainTag> {}

impl<DomainTag> Clone for BorrowedRootRows<'_, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'bytes, DomainTag> BorrowedRootRows<'bytes, DomainTag> {
    const fn rows(&self) -> &'bytes [RootWireRecord] {
        match *self {
            Self::Empty(rows) | Self::Populated { rows, .. } => rows,
        }
    }
}

/// Borrowed proof that one complete byte slice is a canonical generation root.
///
/// The witness owns no row or descriptor backing. The only allocation used by
/// validation is a transient compact parent-coordinate lane which is released
/// before this value is returned.
pub struct ValidatedRoot<'bytes, DomainTag> {
    facts: BorrowedRootFacts<'bytes>,
    rows: BorrowedRootRows<'bytes, DomainTag>,
}

/// Canonical-root byte validation failure.
#[derive(Debug, Error)]
pub enum RootReadError {
    /// The complete fixed header was not present.
    #[error("root header needs {required} bytes but only {available} are available")]
    HeaderTruncated {
        /// Fixed header width.
        required: usize,
        /// Complete supplied byte length.
        available: usize,
    },
    /// The declared row count does not fit the root's compact coordinate space.
    #[error("root declares {declared} rows, exceeding the compact row limit")]
    CountOutOfRange {
        /// Complete observed wide count.
        declared: u64,
        /// Narrowing failure.
        #[source]
        source: core::num::TryFromIntError,
    },
    /// The fixed-width grammar could not fit the host address space.
    #[error("root byte layout overflows for {count} rows")]
    LayoutOverflow {
        /// Declared compact row count.
        count: u32,
    },
    /// The artifact ended before its declared row extent.
    #[error("root declares an exact {required}-byte extent but only {available} are available")]
    Truncated {
        /// Exact declared grammar extent.
        required: usize,
        /// Complete supplied byte length.
        available: usize,
    },
    /// The artifact contains bytes after its declared rows.
    #[error("root has {actual} bytes but its exact grammar requires {expected}")]
    TrailingBytes {
        /// Exact declared grammar extent.
        expected: usize,
        /// Complete supplied byte length.
        actual: usize,
    },
    /// One row carried a parent-presence value outside the closed wire grammar.
    #[error("root row {ordinal} has invalid parent tag {observed}")]
    ParentTag {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Complete observed tag byte.
        observed: u8,
    },
    /// One row carried an invalid nested object descriptor.
    #[error("root row {ordinal} has an invalid object descriptor")]
    Descriptor {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Exact nested descriptor rejection.
        #[source]
        source: ObjectDescriptorDecodeError,
    },
    /// Exact geometry passed, but the bytes could not inhabit the typed row grammar.
    #[error("root rows violated their typed wire representation")]
    TypedRows,
    /// One row's content identity belongs to a different closed domain.
    #[error("root row {ordinal} content authority failed checked decode")]
    ContentAuthority {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Checked authority rejection with complete operands.
        #[source]
        source: ContentAuthorityError,
    },
    /// Canonical keys were duplicated or not strictly increasing.
    #[error("root row {ordinal} key {current:?} does not strictly follow {previous:?}")]
    KeyOrder {
        /// Canonical row ordinal of `current`.
        ordinal: u32,
        /// Previous key.
        previous: EntryKey,
        /// Current key.
        current: EntryKey,
    },
    /// An absent parent marker carried a nonzero key.
    #[error("root row {ordinal} key {key:?} has absent parent marker with key {observed:?}")]
    AbsentParentKey {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Child key.
        key: EntryKey,
        /// Noncanonical parent-key cell.
        observed: EntryKey,
    },
    /// A row names itself as its parent.
    #[error("root row {ordinal} key {key:?} names itself as parent")]
    SelfParent {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Self-parented key.
        key: EntryKey,
    },
    /// A present parent key was absent from the same canonical root.
    #[error("root row {ordinal} key {key:?} names missing parent {parent:?}")]
    MissingParent {
        /// Canonical row ordinal.
        ordinal: u32,
        /// Child key.
        key: EntryKey,
        /// Missing parent key.
        parent: EntryKey,
    },
    /// The exact compact hierarchy lane could not be reserved.
    #[error("root hierarchy validation could not reserve {requested_bytes} transient bytes")]
    HierarchyScratch {
        /// Exact logical `u32` lane width.
        requested_bytes: usize,
        /// Allocator reservation cause.
        #[source]
        source: TryReserveError,
    },
    /// The parent relation contains a cycle.
    #[error("root hierarchy contains a cycle reachable from {key:?}")]
    HierarchyCycle {
        /// First canonical start whose unresolved path exposed the cycle.
        key: EntryKey,
    },
}

#[derive(Default)]
struct RootValidationWork {
    authority_rows: usize,
    order_rows: usize,
    parent_rows: usize,
    hierarchy_steps: usize,
    scratch_high_water_bytes: usize,
}

impl<'bytes, DomainTag: Domain> TryFrom<&'bytes [u8]> for ValidatedRoot<'bytes, DomainTag> {
    type Error = RootReadError;

    fn try_from(bytes: &'bytes [u8]) -> Result<Self, Self::Error> {
        parse_root(bytes, &mut RootValidationWork::default())
    }
}

fn parse_root<'bytes, DomainTag: Domain>(
    bytes: &'bytes [u8],
    work: &mut RootValidationWork,
) -> Result<ValidatedRoot<'bytes, DomainTag>, RootReadError> {
    let header_bytes = bytes
        .get(..ROOT_HEADER_BYTES)
        .ok_or(RootReadError::HeaderTruncated {
            required: ROOT_HEADER_BYTES,
            available: bytes.len(),
        })?;
    let header = RootHeaderRecord::ref_from_bytes(header_bytes).map_err(|source| {
        RootReadError::HeaderTruncated {
            required: ROOT_HEADER_BYTES,
            available: source.into_src().len(),
        }
    })?;
    let declared = header.count.get();
    let count = u32::try_from(declared)
        .map_err(|source| RootReadError::CountOutOfRange { declared, source })?;
    let native_count = native(count);
    let row_bytes = native_count
        .checked_mul(ROOT_ROW_RECORD_BYTES)
        .ok_or(RootReadError::LayoutOverflow { count })?;
    let expected = ROOT_HEADER_BYTES
        .checked_add(row_bytes)
        .ok_or(RootReadError::LayoutOverflow { count })?;
    if bytes.len() < expected {
        return Err(RootReadError::Truncated {
            required: expected,
            available: bytes.len(),
        });
    }
    if bytes.len() > expected {
        return Err(RootReadError::TrailingBytes {
            expected,
            actual: bytes.len(),
        });
    }
    #[allow(
        clippy::indexing_slicing,
        reason = "the exact-extent checks immediately above prove this complete declared row region"
    )]
    let row_region = &bytes[ROOT_HEADER_BYTES..expected];
    let rows = <[RootWireRecord]>::try_ref_from_bytes_with_elems(row_region, native_count)
        .map_err(|_| diagnose_typed_rows(row_region))?;
    let rows = validate_authority::<DomainTag>(rows, work)?;
    validate_key_order(rows.rows(), work)?;
    validate_hierarchy(rows.rows(), work)?;
    Ok(ValidatedRoot {
        facts: BorrowedRootFacts {
            bytes,
            id: GenerationId::from_canonical_bytes(bytes),
            entry_count: count.into(),
        },
        rows,
    })
}

fn diagnose_typed_rows(bytes: &[u8]) -> RootReadError {
    const PARENT_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_present);
    const DESCRIPTOR_OFFSET: usize = core::mem::offset_of!(RootWireRecord, descriptor);
    const SCHEMA_OFFSET: usize =
        DESCRIPTOR_OFFSET + core::mem::offset_of!(nudox_object::ObjectDescriptorWireRecord, schema);
    for (ordinal, row) in (0_u32..).zip(bytes.chunks_exact(ROOT_ROW_RECORD_BYTES)) {
        if let Some(&observed) = row.get(PARENT_OFFSET)
            && observed > u8::from(ParentWire::Present)
        {
            return RootReadError::ParentTag { ordinal, observed };
        }
        let schema = row
            .get(SCHEMA_OFFSET..SCHEMA_OFFSET + size_of::<u32>())
            .and_then(|bytes| <&[u8; 4]>::try_from(bytes).ok())
            .map(|bytes| u32::from_be_bytes(*bytes));
        if let Some(Err(source)) = schema.map(nudox_schema::SchemaId::try_from) {
            return RootReadError::Descriptor {
                ordinal,
                source: ObjectDescriptorDecodeError::Schema(source),
            };
        }
    }
    RootReadError::TypedRows
}

fn validate_authority<'bytes, DomainTag: Domain>(
    rows: &'bytes [RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<BorrowedRootRows<'bytes, DomainTag>, RootReadError> {
    let Some((first, remaining)) = rows.split_first() else {
        return Ok(BorrowedRootRows::Empty(rows));
    };
    work.authority_rows += 1;
    let authority = ContentAuthority::try_from(first.descriptor.content[0])
        .map_err(|source| RootReadError::ContentAuthority { ordinal: 0, source })?;
    for (ordinal, row) in (1_u32..).zip(remaining) {
        work.authority_rows += 1;
        ContentAuthority::<DomainTag>::try_from(row.descriptor.content[0])
            .map_err(|source| RootReadError::ContentAuthority { ordinal, source })?;
    }
    Ok(BorrowedRootRows::Populated { rows, authority })
}

fn validate_key_order(
    rows: &[RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    for (ordinal, pair) in (1_u32..).zip(rows.windows(2)) {
        work.order_rows += 1;
        let [previous_row, current_row] = pair else {
            continue;
        };
        let previous = EntryKey::from(previous_row.key.get());
        let current = EntryKey::from(current_row.key.get());
        if previous >= current {
            return Err(RootReadError::KeyOrder {
                ordinal,
                previous,
                current,
            });
        }
    }
    Ok(())
}

#[allow(
    clippy::indexing_slicing,
    reason = "the parent lane is allocated to the exact typed row count and every ordinal comes from that row iterator"
)]
fn validate_hierarchy(
    rows: &[RootWireRecord],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    let requested_bytes = rows
        .len()
        .checked_mul(size_of::<u32>())
        .ok_or(RootReadError::LayoutOverflow { count: u32::MAX })?;
    let mut parents = Vec::new();
    parents
        .try_reserve_exact(rows.len())
        .map_err(|source| RootReadError::HierarchyScratch {
            requested_bytes,
            source,
        })?;
    parents.resize(rows.len(), NO_PARENT);
    work.scratch_high_water_bytes = requested_bytes;

    for (ordinal, row) in (0_u32..).zip(rows) {
        work.parent_rows += 1;
        let key = EntryKey::from(row.key.get());
        let parent = EntryKey::from(row.parent_key.get());
        match row.parent_present {
            ParentWire::Absent => {
                if *parent != 0 {
                    return Err(RootReadError::AbsentParentKey {
                        ordinal,
                        key,
                        observed: parent,
                    });
                }
            }
            ParentWire::Present => {
                if parent == key {
                    return Err(RootReadError::SelfParent { ordinal, key });
                }
                let position = rows
                    .binary_search_by_key(&*parent, |candidate| candidate.key.get())
                    .map_err(|_| RootReadError::MissingParent {
                        ordinal,
                        key,
                        parent,
                    })?;
                parents[native(ordinal)] = compact(position);
            }
        }
    }
    validate_acyclic(rows, &mut parents, work)
}

#[allow(
    clippy::indexing_slicing,
    clippy::needless_range_loop,
    reason = "every compact coordinate was binary-resolved within this exact lane; the numeric start is also the diagnostic root-row coordinate"
)]
fn validate_acyclic(
    rows: &[RootWireRecord],
    parents: &mut [u32],
    work: &mut RootValidationWork,
) -> Result<(), RootReadError> {
    for start in 0..parents.len() {
        let mut slow = compact(start);
        let mut fast = slow;
        loop {
            work.hierarchy_steps += 1;
            slow = parents[native(slow)];
            if slow == NO_PARENT {
                break;
            }
            fast = parents[native(fast)];
            if fast == NO_PARENT {
                break;
            }
            fast = parents[native(fast)];
            if fast == NO_PARENT {
                break;
            }
            if slow == fast {
                return Err(RootReadError::HierarchyCycle {
                    key: EntryKey::from(rows[start].key.get()),
                });
            }
        }
        let mut cursor = compact(start);
        while cursor != NO_PARENT {
            work.hierarchy_steps += 1;
            let next = parents[native(cursor)];
            parents[native(cursor)] = NO_PARENT;
            cursor = next;
        }
    }
    Ok(())
}

impl<'bytes, DomainTag> Deref for ValidatedRoot<'bytes, DomainTag> {
    type Target = BorrowedRootFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<DomainTag: Domain> ValidatedRoot<'_, DomainTag> {
    /// Returns the validated row count as a native slice length.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.rows().len()
    }

    /// Returns whether this root has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.rows().is_empty()
    }

    /// Finds one borrowed-root entry through canonical-key binary search.
    #[must_use]
    pub fn get(&self, key: EntryKey) -> Option<RootEntry<DomainTag>> {
        match self.rows {
            BorrowedRootRows::Empty(_) => None,
            BorrowedRootRows::Populated { rows, authority } => rows
                .binary_search_by_key(&*key, |row| row.key.get())
                .ok()
                .and_then(|position| rows.get(position))
                .map(|row| project_root_entry(row, authority)),
        }
    }
}

/// Borrowed coherent composition of canonical root bytes and validated locality.
pub struct BorrowedGenerationView<'bytes, 'locality, DomainTag> {
    facts: BorrowedRootFacts<'bytes>,
    rows: BorrowedRootRows<'bytes, DomainTag>,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}

impl<'bytes, DomainTag> Deref for BorrowedGenerationView<'bytes, '_, DomainTag> {
    type Target = BorrowedRootFacts<'bytes>;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'bytes, 'locality, DomainTag: Domain> BorrowedGenerationView<'bytes, 'locality, DomainTag> {
    /// Pairs root and locality witnesses after checking their generation and cardinality.
    ///
    /// # Errors
    ///
    /// Returns the existing exact locality mismatch with both complete operands.
    pub fn new(
        root: &ValidatedRoot<'bytes, DomainTag>,
        locality: &'locality ValidatedLocality<'locality, DomainTag>,
    ) -> Result<Self, LocalityError> {
        if locality.generation != root.id {
            return Err(LocalityError::GenerationMismatch {
                locality_generation: locality.generation,
                root_generation: root.id,
            });
        }
        if locality.root_count != root.entry_count {
            return Err(LocalityError::RootCountMismatch {
                locality_count: locality.root_count,
                root_count: root.entry_count,
            });
        }
        Ok(Self {
            facts: root.facts,
            rows: root.rows,
            locality,
        })
    }

    /// Returns the canonical composed row count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.rows().len()
    }

    /// Returns whether this composed view has no rows.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.rows().is_empty()
    }

    /// Iterates composed entries in canonical key order without allocation.
    #[must_use]
    pub fn closure(&self) -> BorrowedGenerationScan<'_, 'locality, DomainTag> {
        BorrowedGenerationScan::new(self.rows, self.locality)
    }

    /// Finds one composed entry by semantic key.
    #[must_use]
    pub fn get(&self, key: EntryKey) -> Option<GenerationEntry<DomainTag>> {
        match self.rows {
            BorrowedRootRows::Empty(_) => None,
            BorrowedRootRows::Populated { rows, authority } => {
                let position = rows.binary_search_by_key(&*key, |row| row.key.get()).ok()?;
                let index = RowIndex::from_validated_borrowed_root_position(position);
                let row = rows.get(position)?;
                let root = project_root_entry(row, authority);
                Some(GenerationEntry {
                    key: root.key,
                    parent: root.parent,
                    object: root.object,
                    locality: self.locality.locality_for(index),
                })
            }
        }
    }

    /// Selects a projection and all ancestors into reusable caller scratch.
    ///
    /// # Errors
    ///
    /// Returns [`ClosureError::ScratchTooSmall`] before changing scratch when
    /// the caller's declared capacity cannot cover this root.
    pub fn select_closure<'selected>(
        &'selected self,
        range: Option<EntryRange>,
        scratch: &'selected mut ClosureScratch,
    ) -> Result<BorrowedSelectedGeneration<'selected, 'bytes, 'locality, DomainTag>, ClosureError>
    {
        let rows = self.rows.rows();
        if scratch.capacity < rows.len() {
            return Err(ClosureError::ScratchTooSmall {
                required: rows.len(),
                available: scratch.capacity,
            });
        }
        scratch.begin_selection();
        match range {
            None => {
                for position in 0..rows.len() {
                    scratch.record_projected_row();
                    scratch.mark(RowIndex::from_validated_borrowed_root_position(position));
                }
            }
            Some(range) => {
                let start = rows.partition_point(|row| row.key.get() < *range.start());
                let end = rows.partition_point(|row| row.key.get() <= *range.end());
                for position in start..end {
                    scratch.record_projected_row();
                    mark_borrowed_ancestors(rows, scratch, position);
                }
                scratch.sort_selected_indices();
            }
        }
        Ok(BorrowedSelectedGeneration {
            rows: self.rows,
            selected: scratch.selected_indices(),
            compact_count: scratch.selected_count(),
            locality: self.locality,
        })
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "the start and every binary-resolved parent position belong to this exact validated typed row slice"
)]
fn mark_borrowed_ancestors(rows: &[RootWireRecord], scratch: &mut ClosureScratch, start: usize) {
    let mut position = start;
    loop {
        let index = RowIndex::from_validated_borrowed_root_position(position);
        if scratch.is_marked(index) {
            return;
        }
        scratch.mark(index);
        let row = &rows[position];
        if matches!(row.parent_present, ParentWire::Absent) {
            return;
        }
        let Ok(parent) =
            rows.binary_search_by_key(&row.parent_key.get(), |candidate| candidate.key.get())
        else {
            return;
        };
        scratch.record_ancestor_edge();
        position = parent;
    }
}

/// Allocation-free scan over a borrowed root/locality composition.
pub struct BorrowedGenerationScan<'scan, 'locality, DomainTag> {
    state: BorrowedGenerationScanState<'scan, 'locality, DomainTag>,
}

enum BorrowedGenerationScanState<'scan, 'locality, DomainTag> {
    Empty,
    Populated {
        rows: core::iter::Enumerate<slice::Iter<'scan, RootWireRecord>>,
        authority: ContentAuthority<DomainTag>,
        locality: LocalityCursor<'locality, DomainTag>,
    },
}

impl<'scan, 'locality, DomainTag: Domain> BorrowedGenerationScan<'scan, 'locality, DomainTag> {
    fn new(
        rows: BorrowedRootRows<'scan, DomainTag>,
        locality: &'locality ValidatedLocality<'locality, DomainTag>,
    ) -> Self {
        let state = match rows {
            BorrowedRootRows::Empty(_) => BorrowedGenerationScanState::Empty,
            BorrowedRootRows::Populated { rows, authority } => {
                BorrowedGenerationScanState::Populated {
                    rows: rows.iter().enumerate(),
                    authority,
                    locality: LocalityCursor::new(locality),
                }
            }
        };
        Self { state }
    }
}

impl<DomainTag: Domain> Iterator for BorrowedGenerationScan<'_, '_, DomainTag> {
    type Item = GenerationEntry<DomainTag>;

    fn next(&mut self) -> Option<Self::Item> {
        let BorrowedGenerationScanState::Populated {
            rows,
            authority,
            locality,
        } = &mut self.state
        else {
            return None;
        };
        let (position, row) = rows.next()?;
        let root = project_root_entry(row, *authority);
        Some(GenerationEntry {
            key: root.key,
            parent: root.parent,
            object: root.object,
            locality: locality
                .locality_without_work(RowIndex::from_validated_borrowed_root_position(position)),
        })
    }
}

/// Borrowed selected closure backed by root bytes and caller-owned marks.
pub struct BorrowedSelectedGeneration<'selected, 'bytes, 'locality, DomainTag> {
    rows: BorrowedRootRows<'bytes, DomainTag>,
    selected: &'selected [RowIndex],
    compact_count: u32,
    locality: &'locality ValidatedLocality<'locality, DomainTag>,
}

impl<'bytes, 'locality, DomainTag: Domain>
    BorrowedSelectedGeneration<'_, 'bytes, 'locality, DomainTag>
{
    /// Iterates selected entries in canonical key order.
    #[must_use]
    pub fn iter(&self) -> BorrowedSelectedScan<'_, 'bytes, 'locality, DomainTag> {
        let state = match self.rows {
            BorrowedRootRows::Empty(_) => BorrowedSelectedScanState::Empty,
            BorrowedRootRows::Populated { rows, authority } => {
                BorrowedSelectedScanState::Populated {
                    selected: self.selected.iter(),
                    rows,
                    authority,
                    locality: LocalityCursor::new(self.locality),
                }
            }
        };
        BorrowedSelectedScan { state }
    }

    /// Returns the root-proven compact selected count.
    #[must_use]
    pub fn count(&self) -> crate::SelectedCount {
        crate::SelectedCount::from(self.compact_count)
    }

    /// Returns selected descriptor count.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.selected.len()
    }

    /// Returns whether this selection is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }
}

impl<'scan, 'bytes, 'locality, DomainTag: Domain> IntoIterator
    for &'scan BorrowedSelectedGeneration<'_, 'bytes, 'locality, DomainTag>
{
    type Item = GenerationEntry<DomainTag>;
    type IntoIter = BorrowedSelectedScan<'scan, 'bytes, 'locality, DomainTag>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Allocation-free canonical scan over one borrowed selected closure.
pub struct BorrowedSelectedScan<'scan, 'bytes, 'locality, DomainTag> {
    state: BorrowedSelectedScanState<'scan, 'bytes, 'locality, DomainTag>,
}

enum BorrowedSelectedScanState<'scan, 'bytes, 'locality, DomainTag> {
    Empty,
    Populated {
        selected: slice::Iter<'scan, RowIndex>,
        rows: &'bytes [RootWireRecord],
        authority: ContentAuthority<DomainTag>,
        locality: LocalityCursor<'locality, DomainTag>,
    },
}

impl<DomainTag: Domain> Iterator for BorrowedSelectedScan<'_, '_, '_, DomainTag> {
    type Item = GenerationEntry<DomainTag>;

    #[allow(
        clippy::indexing_slicing,
        reason = "selected coordinates were issued only after scratch capacity was checked against this exact validated root"
    )]
    fn next(&mut self) -> Option<Self::Item> {
        let BorrowedSelectedScanState::Populated {
            selected,
            rows,
            authority,
            locality,
        } = &mut self.state
        else {
            return None;
        };
        let index = *selected.next()?;
        let root = project_root_entry(&rows[index.array_index()], *authority);
        Some(GenerationEntry {
            key: root.key,
            parent: root.parent,
            object: root.object,
            locality: locality.locality_without_work(index),
        })
    }
}

fn project_root_entry<DomainTag: Domain>(
    row: &RootWireRecord,
    authority: ContentAuthority<DomainTag>,
) -> RootEntry<DomainTag> {
    let [_, payload @ ..] = row.descriptor.content;
    RootEntry {
        key: EntryKey::from(row.key.get()),
        parent: match row.parent_present {
            ParentWire::Absent => None,
            ParentWire::Present => Some(EntryKey::from(row.parent_key.get())),
        },
        object: ObjectRef {
            content: authority.bind(payload),
            length: ObjectLength::from(row.descriptor.length.get()),
            schema: row.descriptor.schema.get(),
            kind: ObjectKind::from(row.descriptor.kind.get()),
        },
    }
}

#[allow(
    clippy::as_conversions,
    reason = "the root crate supports 32/64-bit usize and every compact count fits both"
)]
const fn native(value: u32) -> usize {
    value as usize
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "positions originate in a slice whose declared length already fit u32"
)]
const fn compact(value: usize) -> u32 {
    value as u32
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::as_conversions,
        clippy::cognitive_complexity,
        clippy::indexing_slicing,
        clippy::type_complexity,
        reason = "fault fixtures mutate exact proven wire offsets and the large-cardinality control uses compile-time bounded values"
    )]
    use alloc::{vec, vec::Vec};

    use nudox_id::{ContentId, DomainCode, ObjectDomain};
    use nudox_object::{ObjectDescriptorWireRecord, ObjectKind, ObjectLength, ObjectRef};
    use nudox_schema::SchemaId;
    use thiserror::Error;
    use zerocopy::IntoBytes;

    use super::*;
    use crate::{
        EntryRangeError, GenerationRoot, GenerationView, LocalityWriteError, PreparedLocality,
        RootBuildError, RootWriteError,
    };

    const KEY_OFFSET: usize = core::mem::offset_of!(RootWireRecord, key);
    const PARENT_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_present);
    const PARENT_KEY_OFFSET: usize = core::mem::offset_of!(RootWireRecord, parent_key);
    const DESCRIPTOR_OFFSET: usize = core::mem::offset_of!(RootWireRecord, descriptor);
    const CONTENT_OFFSET: usize =
        DESCRIPTOR_OFFSET + core::mem::offset_of!(ObjectDescriptorWireRecord, content);
    const SCHEMA_OFFSET: usize =
        DESCRIPTOR_OFFSET + core::mem::offset_of!(ObjectDescriptorWireRecord, schema);

    #[derive(Debug, Error)]
    enum BorrowedRootTestError {
        #[error(transparent)]
        Build(#[from] RootBuildError),
        #[error(transparent)]
        Write(#[from] RootWriteError),
        #[error(transparent)]
        Read(#[from] RootReadError),
        #[error(transparent)]
        Locality(#[from] LocalityError),
        #[error(transparent)]
        LocalityWrite(#[from] LocalityWriteError),
        #[error(transparent)]
        Range(#[from] EntryRangeError),
        #[error(transparent)]
        Closure(#[from] ClosureError),
        #[error("closure scratch reservation failed")]
        Scratch(#[source] TryReserveError),
    }

    fn object(seed: u8) -> ObjectRef<ObjectDomain> {
        ObjectRef {
            content: ContentId::from_digest([seed; 32]),
            length: ObjectLength::from(u64::from(seed)),
            schema: SchemaId::Object,
            kind: ObjectKind::from(u16::from(seed)),
        }
    }

    fn entry(key: u64, parent: Option<u64>, seed: u8) -> RootEntry<ObjectDomain> {
        RootEntry {
            key: EntryKey::from(key),
            parent: parent.map(EntryKey::from),
            object: object(seed),
        }
    }

    fn canonical_root(
        entries: Vec<RootEntry<ObjectDomain>>,
    ) -> Result<(GenerationRoot<ObjectDomain>, Vec<u8>), BorrowedRootTestError> {
        let root = GenerationRoot::new(entries)?;
        let mut bytes = vec![0; usize::from(root.canonical_len())];
        root.write_canonical(&mut bytes)?;
        Ok((root, bytes))
    }

    const fn row_start(ordinal: usize) -> usize {
        ROOT_HEADER_BYTES + ordinal * ROOT_ROW_RECORD_BYTES
    }

    fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + size_of::<u64>()].copy_from_slice(&value.to_be_bytes());
    }

    fn constant_memory_forward_chain_steps(rows: usize) -> u64 {
        let mut steps = 0_u64;
        for start in 0..rows {
            for _ in start..rows {
                steps += 1;
            }
        }
        steps
    }

    fn exact_forward_chain_steps(rows: usize) -> u64 {
        let rows = rows as u64;
        rows * (rows + 1) / 2
    }

    #[test]
    fn borrowed_root_is_pointer_backed_and_selection_matches_owned_view()
    -> Result<(), BorrowedRootTestError> {
        let (owned, bytes) = canonical_root(Vec::from([
            entry(3, Some(2), 3),
            entry(1, None, 1),
            entry(2, Some(1), 2),
        ]))?;
        let borrowed = ValidatedRoot::<ObjectDomain>::try_from(bytes.as_slice())?;
        assert_eq!(borrowed.bytes.as_ptr(), bytes.as_ptr());
        assert_eq!(
            borrowed.rows.rows().as_ptr().cast::<u8>(),
            bytes.as_ptr().wrapping_add(8)
        );
        assert_eq!(borrowed.id, owned.id);
        assert_eq!(borrowed.entry_count, owned.entry_count);
        assert_eq!(
            borrowed.get(EntryKey::from(2)),
            owned.get(EntryKey::from(2))
        );

        let prepared = PreparedLocality::prepare(&owned, &[])?;
        let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
        let locality = prepared.write(&mut locality_bytes)?;
        let owned_view = GenerationView::new(&owned, &locality)?;
        let borrowed_view = BorrowedGenerationView::new(&borrowed, &locality)?;
        assert_eq!(
            borrowed_view.closure().collect::<Vec<_>>(),
            owned_view.closure().collect::<Vec<_>>()
        );

        let range = EntryRange::new(EntryKey::from(3), EntryKey::from(3))?;
        let mut owned_scratch =
            ClosureScratch::new(owned.len()).map_err(BorrowedRootTestError::Scratch)?;
        let mut borrowed_scratch =
            ClosureScratch::new(owned.len()).map_err(BorrowedRootTestError::Scratch)?;
        let owned_selected = owned_view.select_closure(Some(range), &mut owned_scratch)?;
        let borrowed_selected = borrowed_view.select_closure(Some(range), &mut borrowed_scratch)?;
        assert_eq!(borrowed_selected.count(), owned_selected.count());
        assert_eq!(
            borrowed_selected.iter().collect::<Vec<_>>(),
            owned_selected.iter().collect::<Vec<_>>()
        );
        assert_eq!(borrowed_scratch.work, owned_scratch.work);
        Ok(())
    }

    #[test]
    fn exact_extent_and_compact_count_rejections_precede_row_semantics()
    -> Result<(), BorrowedRootTestError> {
        let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
        for length in 0..ROOT_HEADER_BYTES {
            assert!(matches!(
                ValidatedRoot::<ObjectDomain>::try_from(&bytes[..length]),
                Err(RootReadError::HeaderTruncated { required: 8, available }) if available == length
            ));
        }
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(&bytes[..bytes.len() - 1]),
            Err(RootReadError::Truncated { required, available })
                if required == bytes.len() && available + 1 == bytes.len()
        ));
        let mut trailing = bytes.clone();
        trailing.push(0xa5);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(trailing.as_slice()),
            Err(RootReadError::TrailingBytes { expected, actual })
                if expected == bytes.len() && actual == trailing.len()
        ));
        let mut wide = bytes;
        wide[..ROOT_HEADER_BYTES].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(wide.as_slice()),
            Err(RootReadError::CountOutOfRange {
                declared: u64::MAX,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn typed_rows_report_parent_schema_and_authority_operands_exactly()
    -> Result<(), BorrowedRootTestError> {
        let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
        let base = row_start(0);

        let mut bad_parent = bytes.clone();
        bad_parent[base + PARENT_OFFSET] = 7;
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(bad_parent.as_slice()),
            Err(RootReadError::ParentTag {
                ordinal: 0,
                observed: 7
            })
        ));

        let mut bad_schema = bytes.clone();
        bad_schema[base + SCHEMA_OFFSET..base + SCHEMA_OFFSET + size_of::<u32>()]
            .copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(bad_schema.as_slice()),
            Err(RootReadError::Descriptor {
                ordinal: 0,
                source: ObjectDescriptorDecodeError::Schema(nudox_schema::UnknownSchemaId(
                    u32::MAX
                )),
            })
        ));

        let mut bad_domain = bytes;
        let observed = u8::from(DomainCode::DependencySet);
        bad_domain[base + CONTENT_OFFSET] = observed;
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(bad_domain.as_slice()),
            Err(RootReadError::ContentAuthority {
                ordinal: 0,
                source: ContentAuthorityError {
                    expected: DomainCode::Object,
                    observed: actual,
                },
            }) if actual == observed
        ));
        Ok(())
    }

    #[test]
    fn key_and_every_parent_invariant_have_distinct_rejections() -> Result<(), BorrowedRootTestError>
    {
        let (_, bytes) = canonical_root(Vec::from([entry(1, None, 1), entry(2, Some(1), 2)]))?;
        let first = row_start(0);
        let second = row_start(1);

        let mut duplicate = bytes.clone();
        set_u64(&mut duplicate, second + KEY_OFFSET, 1);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(duplicate.as_slice()),
            Err(RootReadError::KeyOrder { ordinal: 1, previous, current })
                if previous == EntryKey::from(1) && current == EntryKey::from(1)
        ));

        let mut unsorted = bytes.clone();
        set_u64(&mut unsorted, second + KEY_OFFSET, 0);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(unsorted.as_slice()),
            Err(RootReadError::KeyOrder { ordinal: 1, previous, current })
                if previous == EntryKey::from(1) && current == EntryKey::from(0)
        ));

        let mut absent_key = bytes.clone();
        set_u64(&mut absent_key, first + PARENT_KEY_OFFSET, 9);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(absent_key.as_slice()),
            Err(RootReadError::AbsentParentKey { ordinal: 0, key, observed })
                if key == EntryKey::from(1) && observed == EntryKey::from(9)
        ));

        let mut self_parent = bytes.clone();
        self_parent[first + PARENT_OFFSET] = u8::from(ParentWire::Present);
        set_u64(&mut self_parent, first + PARENT_KEY_OFFSET, 1);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(self_parent.as_slice()),
            Err(RootReadError::SelfParent { ordinal: 0, key }) if key == EntryKey::from(1)
        ));

        let mut missing = bytes.clone();
        set_u64(&mut missing, second + PARENT_KEY_OFFSET, 99);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(missing.as_slice()),
            Err(RootReadError::MissingParent { ordinal: 1, key, parent })
                if key == EntryKey::from(2) && parent == EntryKey::from(99)
        ));

        let mut cycle = bytes;
        cycle[first + PARENT_OFFSET] = u8::from(ParentWire::Present);
        set_u64(&mut cycle, first + PARENT_KEY_OFFSET, 2);
        assert!(matches!(
            ValidatedRoot::<ObjectDomain>::try_from(cycle.as_slice()),
            Err(RootReadError::HierarchyCycle { key }) if key == EntryKey::from(1)
        ));
        Ok(())
    }

    #[test]
    fn borrowed_composition_preserves_generation_and_count_mismatch_operands()
    -> Result<(), BorrowedRootTestError> {
        let (owned, root_bytes) = canonical_root(Vec::from([entry(1, None, 1)]))?;
        let root = ValidatedRoot::<ObjectDomain>::try_from(root_bytes.as_slice())?;
        let prepared = PreparedLocality::prepare(&owned, &[])?;
        let mut locality_bytes = vec![0; usize::from(prepared.required_bytes)];
        prepared.write(&mut locality_bytes)?;

        let (_, other_bytes) = canonical_root(Vec::from([entry(1, None, 2)]))?;
        let other = ValidatedRoot::<ObjectDomain>::try_from(other_bytes.as_slice())?;
        {
            let locality = ValidatedLocality::<ObjectDomain>::try_from(locality_bytes.as_slice())?;
            assert!(matches!(
                BorrowedGenerationView::new(&other, &locality),
                Err(LocalityError::GenerationMismatch {
                    locality_generation,
                    root_generation,
                }) if locality_generation == root.id && root_generation == other.id
            ));
        }

        let count_offset = 32 + size_of::<u8>();
        locality_bytes[count_offset..count_offset + size_of::<u32>()]
            .copy_from_slice(&2_u32.to_be_bytes());
        let locality = ValidatedLocality::<ObjectDomain>::try_from(locality_bytes.as_slice())?;
        assert!(matches!(
            BorrowedGenerationView::new(&root, &locality),
            Err(LocalityError::RootCountMismatch {
                locality_count,
                root_count,
            }) if locality_count == RootEntryCount::from(2) && root_count == RootEntryCount::from(1)
        ));
        Ok(())
    }

    #[test]
    fn hundred_thousand_forward_parent_rows_use_one_linear_transient_lane()
    -> Result<(), BorrowedRootTestError> {
        const ROWS: usize = 100_000;
        let mut bytes = Vec::with_capacity(ROOT_HEADER_BYTES + ROWS * ROOT_ROW_RECORD_BYTES);
        bytes.extend_from_slice(&(ROWS as u64).to_be_bytes());
        let descriptor = ObjectDescriptorWireRecord::from(&object(1));
        for ordinal in 0..ROWS {
            let last = ordinal + 1 == ROWS;
            let row = RootWireRecord {
                key: zerocopy::byteorder::U64::new((ordinal + 1) as u64),
                parent_present: if last {
                    ParentWire::Absent
                } else {
                    ParentWire::Present
                },
                parent_key: zerocopy::byteorder::U64::new(if last {
                    0
                } else {
                    (ordinal + 2) as u64
                }),
                descriptor,
            };
            bytes.extend_from_slice(row.as_bytes());
        }
        let mut work = RootValidationWork::default();
        let root = parse_root::<ObjectDomain>(bytes.as_slice(), &mut work)?;
        assert_eq!(root.len(), ROWS);
        assert_eq!(work.authority_rows, ROWS);
        assert_eq!(work.order_rows, ROWS - 1);
        assert_eq!(work.parent_rows, ROWS);
        assert_eq!(work.scratch_high_water_bytes, ROWS * size_of::<u32>());
        assert!(work.hierarchy_steps <= ROWS * 4);
        assert_eq!(
            constant_memory_forward_chain_steps(2_048),
            exact_forward_chain_steps(2_048)
        );
        assert_eq!(exact_forward_chain_steps(ROWS), 5_000_050_000);
        assert!(exact_forward_chain_steps(ROWS) > work.hierarchy_steps as u64);

        let (_, one) = canonical_root(Vec::from([entry(1, None, 1)]))?;
        let mut one_work = RootValidationWork::default();
        let one_root = parse_root::<ObjectDomain>(one.as_slice(), &mut one_work)?;
        assert_eq!(one_root.len(), 1);
        assert_eq!(one_work.scratch_high_water_bytes, size_of::<u32>());
        assert_eq!(one_work.hierarchy_steps, 2);
        assert_eq!(constant_memory_forward_chain_steps(1), 1);
        Ok(())
    }
}
