use std::{fmt, hash, marker::PhantomData};

use crate::kind::EntryKind;

use super::{DynRegistryResolver, RegistryState};

pub struct EntryIdx<T> {
	repr: Repr,
	_p:   PhantomData<fn() -> T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Repr {
	Resolved { package_idx: PackageIdx, arena_idx: ArenaIdx },
	Deferred(DeferredIdx),
}

// TODO
// const _: () = assert!(size_of::<Repr>() == size_of::<u64>());

impl<T> EntryIdx<T> {
	pub(super) fn new(package_idx: PackageIdx, arena_idx: ArenaIdx) -> Self {
		EntryIdx { repr: Repr::Resolved { package_idx, arena_idx }, _p: PhantomData }
	}

	pub(super) fn raw(self) -> RawEntryIdx { self.cast() }

	pub(super) fn typed<U>(self) -> EntryIdx<U>
	where
		U: EntryKind,
	{
		self.cast()
	}

	pub(super) fn repr(self) -> Repr { self.repr }

	fn cast<U>(self) -> EntryIdx<U> { EntryIdx { repr: self.repr, _p: PhantomData } }
}

impl<T> serde::Serialize for EntryIdx<T> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		use serde::ser::Error;

		serde_context::context_scope(|cx| {
			let registry = cx.get::<dyn DynRegistryResolver>().map_err(S::Error::custom)?;
			let state = cx.get::<RegistryState>().map_err(S::Error::custom)?;

			let value = registry.__idx_to_entry_id(self.raw(), state);

			erased_serde::serialize(&value, serializer)
		})
	}
}

impl<'de, T> serde::Deserialize<'de> for EntryIdx<T> {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		use serde::de::Error;

		serde_context::context_scope(|cx| {
			let resolver = cx.get::<dyn DynRegistryResolver>().map_err(D::Error::custom)?;
			let state = cx.get::<RegistryState>().map_err(D::Error::custom)?;

			// TODO: investigate if there's a better way to do this, so that we don't have
			// to use D::Error::custom.
			let idx = resolver
				.__deser_entry_id_to_idx(&mut <dyn erased_serde::Deserializer>::erase(deserializer), state)
				.map_err(D::Error::custom)?;

			Ok(idx.cast())
		})
	}
}

impl<T> Clone for EntryIdx<T> {
	fn clone(&self) -> Self { *self }
}

impl<T> Copy for EntryIdx<T> {}

impl<T> fmt::Debug for EntryIdx<T> {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("EntryIdx")
			// .field("package", &self.package_idx)
			// .field("index", &self.arena_idx)
			.finish()
	}
}

impl<T> PartialEq for EntryIdx<T> {
	fn eq(&self, other: &Self) -> bool { self.repr == other.repr }
}

impl<T> Eq for EntryIdx<T> {}

impl<T> PartialOrd for EntryIdx<T> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

impl<T> Ord for EntryIdx<T> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering { Ord::cmp(&self.repr, &other.repr) }
}

impl<T> hash::Hash for EntryIdx<T> {
	fn hash<H: hash::Hasher>(&self, state: &mut H) { self.repr.hash(state) }
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

impl RawEntryIdx {
	pub(super) fn inc_arena_idx(self, amount: usize) -> Self {
		let Repr::Resolved { package_idx, arena_idx } = self.repr else {
			panic!("called inc_arena_idx on deferred EntryIdx");
		};

		let arena_idx = ArenaIdx::new(arena_idx.index() + amount);

		RawEntryIdx::new(package_idx, arena_idx)
	}
}

// allow converting to a RawEntryIdx from any typed EntryIdx
impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
	fn from(idx: EntryIdx<T>) -> Self { idx.raw() }
}

mod private {
	pub struct UntypedMarker;
}

index_newtype!(PackageIdx);
index_newtype!(ArenaIdx);
index_newtype!(DeferredIdx);

macro_rules! index_newtype {
	($index:ident) => {
		#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
		pub(crate) struct $index {
			index: u32,
		}

		impl $index {
			#[cfg_attr(not(test), allow(unused))]
			pub(crate) fn new(index: usize) -> Self {
				debug_assert!(u32::try_from(index).is_ok());
				Self { index: index as u32 }
			}

			pub(crate) fn index(self) -> usize { self.index as usize }
		}

		impl std::fmt::Debug for $index {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.debug_tuple(stringify!($index)).field(&self.index).finish()
			}
		}
	};
}

use index_newtype;
