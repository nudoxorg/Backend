use std::{fmt, hash, marker::PhantomData, num::NonZeroUsize};

use crate::kind::EntryKind;

// FIXME: NonZeroUsize handling needs to be carefully considered wrt the bit
// checks we do, and needs plenty of tests

#[repr(transparent)]
pub struct EntryIndex<T: Indexable> {
    index: NonZeroUsize,
    _p: PhantomData<fn() -> T>,
}

pub type UntypedEntryIndex = EntryIndex<private::UntypedMarker>;

pub trait Indexable: private::Sealed {}

impl<T: EntryKind> Indexable for T {}
impl Indexable for private::UntypedMarker {}

mod private {
    pub struct UntypedMarker;

    pub trait Sealed {}

    impl<T: super::EntryKind> Sealed for T {}

    impl Sealed for UntypedMarker {}
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

impl<T: Indexable> EntryIndex<T> {
    pub(super) fn resolved(index: usize) -> Self {
        debug_assert_eq!(
            index & INDEX_RES_AVAILABLE_MASK,
            index,
            "creating resolved index with unavailable bits"
        );

        Self::build(index)
    }

    pub(super) fn export(index: usize) -> Self {
        debug_assert_eq!(
            index & INDEX_SER_AVAILABLE_MASK,
            index,
            "creating export index with unavailable bits"
        );

        Self::build(index | IS_SERIALIZED_MASK)
    }

    pub(super) fn import(index: usize) -> Self {
        debug_assert_eq!(
            index & INDEX_SER_AVAILABLE_MASK,
            index,
            "creating import index with unavailable bits"
        );

        Self::build(index | IS_SERIALIZED_MASK | IS_IMPORT_MASK)
    }

    fn build(index: usize) -> Self {
        assert_ne!(index, usize::MAX);

        EntryIndex {
            index: NonZeroUsize::new(index + 1).unwrap(),
            _p: PhantomData,
        }
    }

    pub(super) fn resolved_index(self) -> usize {
        debug_assert!(self.is_resolved(), "using unresolved EntryIndex");

        self.raw_index()
    }

    #[expect(dead_code)]
    pub(super) fn import_index(self) -> usize {
        debug_assert!(self.is_import(), "using non-import EntryIndex");

        self.raw_index() & !(IS_SERIALIZED_MASK & IS_IMPORT_MASK)
    }

    pub(super) fn export_index(self) -> usize {
        debug_assert!(self.is_export(), "using non-export EntryIndex");

        self.raw_index() & !IS_SERIALIZED_MASK
    }

    pub(super) fn is_resolved(self) -> bool {
        // return if the serialized bit is _NOT_ set
        self.raw_index() & IS_SERIALIZED_MASK == 0
    }

    pub(super) fn is_serialize(self) -> bool {
        // return if the serialized bit IS set
        self.raw_index() & IS_SERIALIZED_MASK == IS_SERIALIZED_MASK
    }

    pub(super) fn is_export(self) -> bool {
        debug_assert!(self.is_serialize());

        self.raw_index() & IS_IMPORT_MASK == 0
    }

    pub(super) fn is_import(self) -> bool {
        debug_assert!(self.is_serialize());

        self.raw_index() & IS_IMPORT_MASK == IS_IMPORT_MASK
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

    pub(crate) fn cast_mut<U: Indexable>(&mut self) -> &mut EntryIndex<U> {
        // Safety: repr(transparent)
        unsafe { &mut *std::ptr::from_mut(self).cast() }
    }

    pub(super) fn cast<U: Indexable>(self) -> EntryIndex<U> {
        EntryIndex {
            index: self.index,
            _p: PhantomData,
        }
    }

    fn raw_index(self) -> usize {
        self.index.get() - 1
    }
}

impl<T: Indexable> serde::Serialize for EntryIndex<T> {
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

impl<'de, T: Indexable> serde::Deserialize<'de> for EntryIndex<T> {
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

impl<T: Indexable> Clone for EntryIndex<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Indexable> Copy for EntryIndex<T> {}

impl<T: Indexable> fmt::Debug for EntryIndex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EntryIdx").field(&self.index).finish()
    }
}

impl<T: Indexable> PartialEq for EntryIndex<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T: Indexable> Eq for EntryIndex<T> {}

impl<T: Indexable> PartialOrd for EntryIndex<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T: Indexable> Ord for EntryIndex<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(&self.index, &other.index)
    }
}

impl<T: Indexable> hash::Hash for EntryIndex<T> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
    }
}
