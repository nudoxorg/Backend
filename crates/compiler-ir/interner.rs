//! Contiguous, collision-safe interners used by the semantic IR.
//!
//! Keys live only once in the value, byte, or list arena. The open-addressed
//! indices contain only a hash and dense ID, avoiding duplicated map keys and
//! per-value allocations. Equality is always checked, so collisions are safe.

use alloc::{vec, vec::Vec};
use core::{
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    str,
};

use crate::{
    AtomId, DenseId, List, ListId, TextId,
    columnar::{RawColumn, Slab, SlabPlan},
};

const EMPTY: u32 = u32::MAX;
const INITIAL_SLOTS: usize = 16;

/// The arena whose fixed-width coordinate space was exhausted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapacitySpace {
    /// A generic value arena.
    Value,
    /// The atom byte arena.
    AtomBytes,
    /// A typed list's element arena.
    ListElements,
}

/// A pool cannot be represented by the IR's `u32` coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityError {
    /// Arena that exceeded `u32` coordinates.
    pub space: CapacitySpace,
    /// Host-sized value that could not be represented.
    pub actual: usize,
}

impl fmt::Display for CapacityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} arena size {} exceeds u32 coordinates",
            self.space, self.actual
        )
    }
}

#[derive(Clone, Copy)]
struct Slot {
    hash: u32,
    ordinal: u32,
}

impl Slot {
    const EMPTY: Self = Self {
        hash: 0,
        ordinal: EMPTY,
    };
}

#[derive(Default)]
pub(crate) struct HashIndex {
    slots: Vec<Slot>,
    len: usize,
}

impl HashIndex {
    pub(crate) fn reserve(&mut self, additional: usize) {
        let required = self.len.saturating_add(additional);
        if required == 0 {
            return;
        }
        let minimum_slots = required.saturating_mul(10).div_ceil(7).max(INITIAL_SLOTS);
        let target = minimum_slots
            .checked_next_power_of_two()
            .unwrap_or(minimum_slots);
        if target > self.slots.len() {
            self.rehash(target);
        }
    }

    pub(crate) fn find(&self, hash: u32, mut equals: impl FnMut(u32) -> bool) -> Option<u32> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        #[allow(
            clippy::as_conversions,
            reason = "hash truncation is intentional table indexing"
        )]
        let mut position = hash as usize & mask;
        loop {
            let slot = self.slots[position];
            if slot.ordinal == EMPTY {
                return None;
            }
            if slot.hash == hash && equals(slot.ordinal) {
                return Some(slot.ordinal);
            }
            position = (position + 1) & mask;
        }
    }

    pub(crate) fn insert(&mut self, hash: u32, ordinal: u32) {
        if self.slots.is_empty() || (self.len + 1) * 10 >= self.slots.len() * 7 {
            self.grow();
        }
        self.insert_without_growth(hash, ordinal);
    }

    fn grow(&mut self) {
        let next = self.slots.len().saturating_mul(2).max(INITIAL_SLOTS);
        self.rehash(next);
    }

    fn rehash(&mut self, next: usize) {
        let old = core::mem::replace(&mut self.slots, vec![Slot::EMPTY; next]);
        self.len = 0;
        for slot in old {
            if slot.ordinal != EMPTY {
                self.insert_without_growth(slot.hash, slot.ordinal);
            }
        }
    }

    fn insert_without_growth(&mut self, hash: u32, ordinal: u32) {
        let mask = self.slots.len() - 1;
        #[allow(
            clippy::as_conversions,
            reason = "hash truncation is intentional table indexing"
        )]
        let mut position = hash as usize & mask;
        while self.slots[position].ordinal != EMPTY {
            position = (position + 1) & mask;
        }
        self.slots[position] = Slot { hash, ordinal };
        self.len += 1;
    }
}

/// Hash-consing arena for any sized value.
pub struct Interner<T, Owner> {
    values: Vec<T>,
    index: HashIndex,
    owner: PhantomData<fn() -> Owner>,
}

impl<T, Owner> Default for Interner<T, Owner> {
    fn default() -> Self {
        Self {
            values: Vec::new(),
            index: HashIndex::default(),
            owner: PhantomData,
        }
    }
}

impl<T: Eq + Hash, Owner> Interner<T, Owner> {
    /// Reserves value and index capacity for a bounded batch.
    pub fn reserve(&mut self, additional: usize) {
        self.values.reserve(additional);
        self.index.reserve(additional);
    }
    /// Interns a value and reports whether this call inserted the canonical row.
    pub fn intern_with_status(
        &mut self,
        value: T,
    ) -> Result<(DenseId<Owner>, bool), CapacityError> {
        let value_hash = hash(&value);
        if let Some(ordinal) = self.index.find(value_hash, |ordinal| {
            self.values
                .get(ordinal as usize)
                .is_some_and(|known| known == &value)
        }) {
            return Ok((DenseId::new(ordinal), false));
        }
        let ordinal = coordinate(self.values.len(), CapacitySpace::Value)?;
        self.values.push(value);
        self.index.insert(value_hash, ordinal);
        Ok((DenseId::new(ordinal), true))
    }

    /// Interns a value, returning the existing ID for an equal value.
    pub fn intern(&mut self, value: T) -> Result<DenseId<Owner>, CapacityError> {
        self.intern_with_status(value).map(|(id, _)| id)
    }

    /// Borrows an interned value.
    #[must_use]
    pub fn get(&self, id: DenseId<Owner>) -> Option<&T> {
        self.values.get(id.index())
    }

    pub(crate) fn as_slice(&self) -> &[T] {
        &self.values
    }

    pub(crate) fn into_values(self) -> Vec<T> {
        self.values
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArenaRange {
    /// First element in the arena.
    pub start: u32,
    /// Number of elements in the value.
    pub len: u32,
}

struct AtomColumns {
    _slab: Slab,
    bytes: RawColumn<u8>,
    ranges: RawColumn<ArenaRange>,
}

impl Default for AtomColumns {
    fn default() -> Self {
        Self::with_capacity(0, 0)
    }
}

impl AtomColumns {
    fn with_capacity(atoms: usize, bytes: usize) -> Self {
        let mut plan = SlabPlan::default();
        let byte_column = plan.column::<u8>(bytes);
        let range_column = plan.column::<ArenaRange>(atoms);
        let slab = plan.allocate();
        Self {
            bytes: slab.bind(byte_column),
            ranges: slab.bind(range_column),
            _slab: slab,
        }
    }

    fn reserve(&mut self, atoms: usize, bytes: usize) {
        let atom_capacity = self.ranges.len().saturating_add(atoms);
        let byte_capacity = self.bytes.len().saturating_add(bytes);
        if atom_capacity <= self.ranges.capacity() && byte_capacity <= self.bytes.capacity() {
            return;
        }
        let mut next = Self::with_capacity(
            atom_capacity.max(self.ranges.capacity()),
            byte_capacity.max(self.bytes.capacity()),
        );
        next.bytes.extend_from_slice(&self.bytes);
        next.ranges.extend_from_slice(&self.ranges);
        *self = next;
    }

    fn reserve_one(&mut self, bytes: usize) {
        let atoms_required = self.ranges.len().saturating_add(1);
        let bytes_required = self.bytes.len().saturating_add(bytes);
        if atoms_required <= self.ranges.capacity() && bytes_required <= self.bytes.capacity() {
            return;
        }
        let atom_capacity = self
            .ranges
            .capacity()
            .saturating_mul(2)
            .max(atoms_required)
            .max(8);
        let byte_capacity = self
            .bytes
            .capacity()
            .saturating_mul(2)
            .max(bytes_required)
            .max(64);
        let mut next = Self::with_capacity(atom_capacity, byte_capacity);
        next.bytes.extend_from_slice(&self.bytes);
        next.ranges.extend_from_slice(&self.ranges);
        *self = next;
    }
}

/// Builder-side interner for arbitrary atoms stored in one contiguous byte arena.
#[derive(Default)]
pub struct AtomInterner {
    storage: AtomColumns,
    index: HashIndex,
}

impl AtomInterner {
    /// Reserves one batch's maximum distinct atoms and byte payload.
    pub fn reserve(&mut self, atoms: usize, bytes: usize) {
        self.storage.reserve(atoms, bytes);
        self.index.reserve(atoms);
    }
    /// Interns arbitrary bytes without allocating per atom.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<AtomId, CapacityError> {
        let value_hash = hash(&bytes);
        if let Some(ordinal) = self.index.find(value_hash, |ordinal| {
            self.get(AtomId::new(ordinal)) == Some(bytes)
        }) {
            return Ok(AtomId::new(ordinal));
        }
        let ordinal = coordinate(self.storage.ranges.len(), CapacitySpace::Value)?;
        let start = coordinate(self.storage.bytes.len(), CapacitySpace::AtomBytes)?;
        let len = coordinate(bytes.len(), CapacitySpace::AtomBytes)?;
        let end = self
            .storage
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(CapacityError {
                space: CapacitySpace::AtomBytes,
                actual: usize::MAX,
            })?;
        if end > u32::MAX as usize {
            return Err(CapacityError {
                space: CapacitySpace::AtomBytes,
                actual: end,
            });
        }
        self.storage.reserve_one(bytes.len());
        self.storage.bytes.extend_from_slice(bytes);
        self.storage.ranges.push(ArenaRange { start, len });
        self.index.insert(value_hash, ordinal);
        Ok(AtomId::new(ordinal))
    }

    /// Interns UTF-8 documentation text in the same universal atom arena.
    pub fn intern_text(&mut self, text: &str) -> Result<TextId, CapacityError> {
        self.intern(text.as_bytes())
            .map(|atom| TextId::new(atom.raw))
    }

    /// Borrows arbitrary interned bytes.
    #[must_use]
    pub fn get(&self, id: AtomId) -> Option<&[u8]> {
        bytes(&self.storage.bytes, &self.storage.ranges, id.raw)
    }

    /// Borrows an atom previously admitted through [`Self::intern_text`].
    #[must_use]
    pub fn text(&self, id: TextId) -> Option<&str> {
        str::from_utf8(bytes(&self.storage.bytes, &self.storage.ranges, id.raw)?).ok()
    }

    pub(crate) fn freeze(self) -> AtomTable {
        AtomTable {
            storage: self.storage,
        }
    }
}

/// Immutable, compact universal atom table.
pub struct AtomTable {
    storage: AtomColumns,
}

/// Direct storage/render view of the universal atom arena.
#[derive(Clone, Copy, Debug)]
pub struct AtomTableView<'ir> {
    /// Contiguous bytes for every atom.
    pub bytes: &'ir [u8],
    /// `AtomId`-indexed start/length pairs.
    pub ranges: &'ir [ArenaRange],
}

impl AtomTable {
    /// Borrows the exact underlying columns without rebuilding a directory.
    #[must_use]
    pub fn view(&self) -> AtomTableView<'_> {
        AtomTableView {
            bytes: &self.storage.bytes,
            ranges: &self.storage.ranges,
        }
    }
    /// Borrows arbitrary atom bytes by ID.
    #[must_use]
    pub fn get(&self, id: AtomId) -> Option<&[u8]> {
        bytes(&self.storage.bytes, &self.storage.ranges, id.raw)
    }

    /// Borrows UTF-8 documentation text by its proof-carrying ID.
    #[must_use]
    pub fn text(&self, id: TextId) -> Option<&str> {
        str::from_utf8(bytes(&self.storage.bytes, &self.storage.ranges, id.raw)?).ok()
    }

    /// Number of distinct interned strings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.storage.ranges.len()
    }

    /// Whether no strings are interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.storage.ranges.is_empty()
    }
}

fn bytes<'a>(bytes: &'a [u8], ranges: &[ArenaRange], raw: u32) -> Option<&'a [u8]> {
    let range = *ranges.get(raw as usize)?;
    let start = range.start as usize;
    let end = start + range.len as usize;
    bytes.get(start..end)
}

/// Builder-side interner for variable-length typed lists in one flat arena.
pub struct ListInterner<T> {
    elements: Vec<T>,
    ranges: RangeStorage,
    index: HashIndex,
    empty: Option<u32>,
}

impl<T> Default for ListInterner<T> {
    fn default() -> Self {
        Self {
            elements: Vec::new(),
            ranges: RangeStorage::default(),
            index: HashIndex::default(),
            empty: None,
        }
    }
}

impl<T: Copy + Eq + Hash> ListInterner<T> {
    /// Reserves list-directory and flat-element capacity for one batch.
    pub fn reserve(&mut self, lists: usize, elements: usize) {
        self.elements.reserve(elements);
        self.ranges.reserve(lists);
        // The empty list is canonicalized without hashing. This matters for
        // compiler trees whose members, attributes, and docs are all empty:
        // each directory remains entirely inline instead of allocating a
        // one-row Vec and a sixteen-slot hash table.
        self.index.reserve(lists.saturating_sub(1));
    }
    /// Interns a typed slice. Equal slices share one ID and one backing range.
    pub fn intern(&mut self, values: &[T]) -> Result<ListId<T>, CapacityError> {
        if values.is_empty() {
            if let Some(ordinal) = self.empty {
                return Ok(DenseId::<List<T>>::new(ordinal));
            }
            let ordinal = coordinate(self.ranges.len(), CapacitySpace::Value)?;
            self.ranges.push(ArenaRange { start: 0, len: 0 });
            self.empty = Some(ordinal);
            return Ok(DenseId::<List<T>>::new(ordinal));
        }
        let value_hash = hash(&values);
        if let Some(ordinal) = self.index.find(value_hash, |ordinal| {
            self.get(DenseId::<List<T>>::new(ordinal)) == Some(values)
        }) {
            return Ok(DenseId::<List<T>>::new(ordinal));
        }
        let ordinal = coordinate(self.ranges.len(), CapacitySpace::Value)?;
        let start = coordinate(self.elements.len(), CapacitySpace::ListElements)?;
        let len = coordinate(values.len(), CapacitySpace::ListElements)?;
        let end = self
            .elements
            .len()
            .checked_add(values.len())
            .ok_or(CapacityError {
                space: CapacitySpace::ListElements,
                actual: usize::MAX,
            })?;
        if end > u32::MAX as usize {
            return Err(CapacityError {
                space: CapacitySpace::ListElements,
                actual: end,
            });
        }
        self.elements.extend_from_slice(values);
        self.ranges.push(ArenaRange { start, len });
        self.index.insert(value_hash, ordinal);
        Ok(DenseId::<List<T>>::new(ordinal))
    }

    /// Borrows an interned typed slice.
    #[must_use]
    pub fn get(&self, id: ListId<T>) -> Option<&[T]> {
        list(&self.elements, &self.ranges, id.raw)
    }

    pub(crate) fn freeze(self) -> ListTable<T> {
        ListTable {
            elements: self.elements,
            ranges: self.ranges,
        }
    }
}

/// Immutable, compact table of interned typed slices.
pub struct ListTable<T> {
    elements: Vec<T>,
    ranges: RangeStorage,
}

/// Direct view of a hash-consed typed-list arena.
#[derive(Clone, Copy, Debug)]
pub struct ListTableView<'ir, T> {
    /// Contiguous elements shared by all lists.
    pub elements: &'ir [T],
    /// `ListId<T>`-indexed start/length pairs.
    pub ranges: &'ir [ArenaRange],
}

impl<T> ListTable<T> {
    /// Borrows both exact backing columns without per-list materialization.
    #[must_use]
    pub fn view(&self) -> ListTableView<'_, T> {
        ListTableView {
            elements: &self.elements,
            ranges: &self.ranges,
        }
    }
    /// Borrows a list by ID.
    #[must_use]
    pub fn get(&self, id: ListId<T>) -> Option<&[T]> {
        list(&self.elements, &self.ranges, id.raw)
    }

    /// Number of distinct lists.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Whether no lists are interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

fn list<'a, T>(elements: &'a [T], ranges: &[ArenaRange], raw: u32) -> Option<&'a [T]> {
    let range = *ranges.get(raw as usize)?;
    let start = range.start as usize;
    let end = start + range.len as usize;
    elements.get(start..end)
}

/// Zero-allocation directory for the overwhelmingly common zero-or-one-list
/// case, with transparent promotion when a frontend interns more shapes.
#[derive(Default)]
enum RangeStorage {
    #[default]
    Empty,
    One(ArenaRange),
    Many(Vec<ArenaRange>),
}

impl RangeStorage {
    fn reserve(&mut self, additional: usize) {
        let required = self.len().saturating_add(additional);
        if required <= 1 {
            return;
        }
        match self {
            Self::Empty => *self = Self::Many(Vec::with_capacity(required)),
            Self::One(value) => {
                let first = *value;
                let mut values = Vec::with_capacity(required);
                values.push(first);
                *self = Self::Many(values);
            }
            Self::Many(values) => values.reserve(additional),
        }
    }

    fn push(&mut self, value: ArenaRange) {
        match self {
            Self::Empty => *self = Self::One(value),
            Self::One(first) => {
                let first = *first;
                *self = Self::Many(vec![first, value]);
            }
            Self::Many(values) => values.push(value),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::One(_) => 1,
            Self::Many(values) => values.len(),
        }
    }

    fn as_slice(&self) -> &[ArenaRange] {
        match self {
            Self::Empty => &[],
            Self::One(value) => core::slice::from_ref(value),
            Self::Many(values) => values,
        }
    }
}

impl core::ops::Deref for RangeStorage {
    type Target = [ArenaRange];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

fn coordinate(value: usize, space: CapacitySpace) -> Result<u32, CapacityError> {
    u32::try_from(value).map_err(|_| CapacityError {
        space,
        actual: value,
    })
}

pub(crate) fn hash(value: &impl Hash) -> u32 {
    let mut state = Fnv1a::default();
    value.hash(&mut state);
    let full = state.finish();
    (full as u32) ^ ((full >> 32) as u32)
}

struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}
