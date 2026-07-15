use std::{fmt, hash, marker::PhantomData};

use super::DynRegistryResolver;
use crate::kind::EntryKind;

pub struct EntryIdx<T> {
	package_idx: PackageIdx,
	arena_idx:   ArenaIdx,
	_p:          PhantomData<fn() -> T>,
}

impl<T> EntryIdx<T> {
	#[cfg_attr(not(test), expect(unused))]
	pub(crate) fn new(package_idx: PackageIdx, arena_idx: ArenaIdx) -> Self {
		EntryIdx { package_idx, arena_idx, _p: PhantomData }
	}

	pub(crate) fn package_idx(self) -> PackageIdx { self.package_idx }
	pub(crate) fn arena_idx(self) -> ArenaIdx { self.arena_idx }

	pub(crate) fn raw(self) -> RawEntryIdx {
		EntryIdx {
			package_idx: self.package_idx,
			arena_idx:   self.arena_idx,
			_p:          PhantomData,
		}
	}

	pub(crate) fn typed<U>(self) -> EntryIdx<U> {
		EntryIdx {
			package_idx: self.package_idx,
			arena_idx:   self.arena_idx,
			_p:          PhantomData,
		}
	}
}

impl<T> serde::Serialize for EntryIdx<T> {
	fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		use serde::ser::Error;

		serde_context::context_scope(|cx| {
			let registry = cx.get::<dyn DynRegistryResolver>().map_err(S::Error::custom)?;

			let value = registry.raw_entry_id_from_idx(self.raw());

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
			let registry = cx.get::<dyn DynRegistryResolver>().map_err(D::Error::custom)?;

			// TODO: investigate if there's a better way to do this, so that we don't have
			// to use D::Error::custom.
			let idx = registry
				.deser_entry_id_to_idx(&mut <dyn erased_serde::Deserializer>::erase(deserializer))
				.map_err(D::Error::custom)?;

			Ok(idx.typed())
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
			.field("package", &self.package_idx)
			.field("index", &self.arena_idx)
			.finish()
	}
}

impl<T> PartialEq for EntryIdx<T> {
	fn eq(&self, other: &Self) -> bool {
		self.package_idx == other.package_idx && self.arena_idx == other.arena_idx
	}
}

impl<T> Eq for EntryIdx<T> {}

impl<T> PartialOrd for EntryIdx<T> {
	fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

impl<T> Ord for EntryIdx<T> {
	fn cmp(&self, other: &Self) -> std::cmp::Ordering {
		self.package_idx.cmp(&other.package_idx).then(self.arena_idx.cmp(&other.arena_idx))
	}
}

impl<T> hash::Hash for EntryIdx<T> {
	fn hash<H: hash::Hasher>(&self, state: &mut H) {
		self.package_idx.hash(state);
		self.arena_idx.hash(state);
	}
}

pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;

impl RawEntryIdx {
	pub(super) fn inc_arena_idx(self, amount: usize) -> Self {
		let arena_idx = ArenaIdx::new(self.arena_idx.index() + amount);

		RawEntryIdx { arena_idx, ..self }
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
