use std::{fmt, hash, marker::PhantomData};

use crate::kind::EntryKind;

pub struct EntryIdx<T> {
	package: u32,
	index:   u32,
	_p:      PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
	pub(crate) fn new(package: usize, index: usize) -> Self {
		debug_assert!(u32::try_from(package).is_ok());
		debug_assert!(u32::try_from(index).is_ok());

		EntryIdx { package: package as u32, index: index as u32, _p: PhantomData }
	}

	pub(crate) fn package(self) -> usize { self.package as usize }
	pub(crate) fn index(self) -> usize { self.index as usize }
}

impl<T> Clone for EntryIdx<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> Copy for EntryIdx<T> {}

impl<T> fmt::Debug for EntryIdx<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("EntryIdx").field("package", &self.package).field("index", &self.index).finish()
	}
}

impl<T> PartialEq for EntryIdx<T> {
	fn eq(&self, other: &Self) -> bool { self.package == other.package && self.index == other.index }
}

impl<T> Eq for EntryIdx<T> {}

impl<T> hash::Hash for EntryIdx<T> {
	fn hash<H: hash::Hasher>(&self, state: &mut H) {
		self.package.hash(state);
		self.index.hash(state);
	}
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
	fn from(idx: EntryIdx<T>) -> Self {
		EntryIdx { package: idx.package, index: idx.index, _p: PhantomData }
	}
}

impl RawEntryIdx {
	pub(crate) fn typed<T>(self) -> EntryIdx<T>
	where
		T: EntryKind,
	{
		EntryIdx { package: self.package, index: self.index, _p: PhantomData }
	}
}

mod private {
	pub struct UntypedMarker;
}
