use std::{fmt, marker::PhantomData};

use nonmax::NonMaxUsize;

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
