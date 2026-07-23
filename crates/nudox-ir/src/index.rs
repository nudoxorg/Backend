use std::{fmt, hash, marker::PhantomData, num::NonZeroUsize};

use crate::kind::EntryKind;

// FIXME: NonZeroUsize handling needs to be carefully considered wrt the bit
// checks we do, and needs plenty of tests
//
// FIXME(reserved-bits): the first end-to-end exercise of the builder export path
// (`package::tests::builds_and_wires_every_kind`, currently `#[ignore]`d) surfaces
// three concrete bugs in this module that no prior code hit:
//   1. The "no reserved bits set" asserts use `index | MASK == index`, which is
//      only ever true when *all* available bits are already set. It should be
//      `index & !MASK == 0` (equivalently `index & MASK == index`). Affects
//      `resolved`, `export`, and `import`.
//   2. `index()` `debug_assert!`s `is_resolved()`, but `is_resolved()`,
//      `is_serialize()`, and `is_import()` all call `index()` — infinite
//      recursion in debug builds, and the assert also wrongly fires for
//      legitimately-serialized (export/import) indices.
//   3. `index()` conflates the raw stored logical value with a resolved arena
//      position; the raw-bit accessors need to read `self.index.get() - 1`
//      directly rather than through the resolved-only `index()`.
// Fixing these is a focused index-module change with its own tests, deliberately
// kept out of the Kind-variants work.

#[repr(transparent)]
pub struct EntryIndex<T> {
    index: NonZeroUsize,
    _p: PhantomData<fn() -> T>,
}

pub type UntypedEntryIndex = EntryIndex<private::UntypedMarker>;

mod private {
    pub struct UntypedMarker;
}

// we reserve 2 bits for serialized indices.
// - the MSB is always set to 1 to indicate that we are a serializable index
// - the second MSB is set to 0 for a local index and 1 for an import index
const INDEX_SER_RESERVED_BITS: u32 = 2;
const INDEX_SER_AVAILABLE_MASK: usize = !(usize::MAX << (usize::BITS - INDEX_SER_RESERVED_BITS));

// MSB indicates if we are a serialized index
const IS_SERIALIZED_MASK: usize = 1 << (usize::BITS - 1);

// second MSB indicates an import if it is set
const IS_IMPORT_MASK: usize = 1 << (usize::BITS - 2);

// we reserve the MSB for resolved indices. it should always be 0 for resolved
// indices, and a value of 1 indicates an unresolved index and should panic
const INDEX_RES_RESERVED_BITS: u32 = 1;
const INDEX_RES_AVAILABLE_MASK: usize = !(usize::MAX << (usize::BITS - INDEX_RES_RESERVED_BITS));

impl<T> EntryIndex<T> {
    pub(super) fn resolved(index: usize) -> Self {
        debug_assert_eq!(
            index | INDEX_RES_AVAILABLE_MASK,
            index,
            "creating resolved index with unavailable bits"
        );

        Self::build(index)
    }

    pub(super) fn export(index: usize) -> Self {
        debug_assert_eq!(
            index | INDEX_SER_AVAILABLE_MASK,
            index,
            "creating export index with unavailable bits"
        );

        Self::build(index | IS_SERIALIZED_MASK)
    }

    pub(super) fn import(index: usize) -> Self {
        debug_assert_eq!(
            index | INDEX_SER_AVAILABLE_MASK,
            index,
            "creating import index with unavailable bits"
        );

        Self::build(index | IS_SERIALIZED_MASK | IS_IMPORT_MASK)
    }

    fn build(index: usize) -> Self {
        EntryIndex {
            index: NonZeroUsize::new(index + 1).unwrap(),
            _p: PhantomData,
        }
    }

    pub(super) fn index(self) -> usize {
        debug_assert!(self.is_resolved(), "using unresolved EntryIdx");

        self.index.get() - 1
    }

    pub(super) fn is_resolved(self) -> bool {
        // return if the serialized bit is _NOT_ set
        self.index() & IS_SERIALIZED_MASK == 0
    }

    pub(super) fn is_serialize(self) -> bool {
        // return if the serialized bit IS set
        self.index() & IS_SERIALIZED_MASK == IS_SERIALIZED_MASK
    }

    pub(super) fn is_export(self) -> bool {
        debug_assert!(self.is_serialize());

        self.index() & IS_IMPORT_MASK == 0
    }

    pub(super) fn is_import(self) -> bool {
        debug_assert!(self.is_serialize());

        self.index() & IS_IMPORT_MASK == IS_IMPORT_MASK
    }

    pub fn raw(self) -> UntypedEntryIndex {
        self.cast()
    }

    pub fn typed<U>(self) -> EntryIndex<U>
    where
        U: EntryKind,
    {
        self.cast()
    }

    pub(crate) fn cast_mut<U>(&mut self) -> &mut EntryIndex<U> {
        // Safety: repr(transparent)
        unsafe { &mut *std::ptr::from_mut(self).cast() }
    }

    pub(super) fn cast<U>(self) -> EntryIndex<U> {
        EntryIndex {
            index: self.index,
            _p: PhantomData,
        }
    }
}

impl<T> serde::Serialize for EntryIndex<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::Error;

        if !self.is_serialize() {
            return Err(S::Error::custom(
                "tried to serialize an EntryIdx that is not marked for serialization",
            ));
        }

        self.index.serialize(serializer)
    }
}

impl<'de, T> serde::Deserialize<'de> for EntryIndex<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let index = usize::deserialize(deserializer)?;

        let index = EntryIndex::build(index);

        if !index.is_serialize() {
            return Err(D::Error::custom(
                "tried to deserialize an EntryIdx that is not marked for serialization",
            ));
        }

        Ok(index)
    }
}

impl<T> Clone for EntryIndex<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for EntryIndex<T> {}

impl<T> fmt::Debug for EntryIndex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EntryIdx").field(&self.index).finish()
    }
}

impl<T> PartialEq for EntryIndex<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T> Eq for EntryIndex<T> {}

impl<T> PartialOrd for EntryIndex<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for EntryIndex<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(&self.index, &other.index)
    }
}

impl<T> hash::Hash for EntryIndex<T> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}
