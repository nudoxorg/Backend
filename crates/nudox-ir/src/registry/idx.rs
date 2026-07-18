use std::{fmt, hash, marker::PhantomData};

use crate::kind::EntryKind;

pub struct EntryIdx<T> {
    index: usize,
    _p: PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
    pub(super) fn new(index: usize) -> Self {
        EntryIdx {
            index,
            _p: PhantomData,
        }
    }

    pub(super) fn index(self) -> usize {
        self.index
    }

    pub(super) fn raw(self) -> RawEntryIdx {
        self.cast()
    }

    pub(super) fn typed<U>(self) -> EntryIdx<U>
    where
        U: EntryKind,
    {
        self.cast()
    }

    pub(super) fn cast<U>(self) -> EntryIdx<U> {
        EntryIdx {
            index: self.index,
            _p: PhantomData,
        }
    }
}

impl<T> Clone for EntryIdx<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for EntryIdx<T> {}

impl<T> fmt::Debug for EntryIdx<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("EntryIdx").field(&self.index).finish()
    }
}

impl<T> PartialEq for EntryIdx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<T> Eq for EntryIdx<T> {}

impl<T> PartialOrd for EntryIdx<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for EntryIdx<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        Ord::cmp(&self.index, &other.index)
    }
}

impl<T> hash::Hash for EntryIdx<T> {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state)
    }
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

impl RawEntryIdx {
    pub(super) fn increment(self, amount: usize) -> Self {
        RawEntryIdx::new(self.index + amount)
    }
}

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
    fn from(idx: EntryIdx<T>) -> Self {
        idx.raw()
    }
}

mod private {
    pub struct UntypedMarker;
}
