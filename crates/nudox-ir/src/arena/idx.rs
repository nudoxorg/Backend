use std::{fmt, hash, marker::PhantomData};

use nonmax::NonMaxUsize;

use crate::kind::EntryKind;

pub struct EntryIdx<T> {
	index: NonMaxUsize,
	_p:    PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
	pub(super) fn new(index: usize) -> Self {
		EntryIdx { index: NonMaxUsize::new(index).unwrap(), _p: PhantomData }
	}

	pub(super) fn index(self) -> usize { self.index.get() }
}

impl<T> Clone for EntryIdx<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> Copy for EntryIdx<T> {}

impl<T> fmt::Debug for EntryIdx<T> {
	fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result { todo!() }
}

impl<T> PartialEq for EntryIdx<T> {
	fn eq(&self, other: &Self) -> bool { self.index == other.index }
}

impl<T> Eq for EntryIdx<T> {}

impl<T> hash::Hash for EntryIdx<T> {
	fn hash<H: hash::Hasher>(&self, state: &mut H) { self.index.hash(state); }
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
	fn from(idx: EntryIdx<T>) -> Self { EntryIdx { index: idx.index, _p: PhantomData } }
}

mod private {
	pub struct UntypedMarker;
}
