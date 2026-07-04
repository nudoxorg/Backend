use std::{fmt, marker::PhantomData};

use nonmax::NonMaxUsize;

use crate::kind::EntryKind;

pub struct EntryIdx<T> {
	index: NonMaxUsize,
	_p:    PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
	#[expect(unused)]
	pub(super) fn new(index: NonMaxUsize) -> Self { EntryIdx { index, _p: PhantomData } }

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

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
	fn from(idx: EntryIdx<T>) -> Self { EntryIdx { index: idx.index, _p: PhantomData } }
}

mod private {
	pub struct UntypedMarker;
}
