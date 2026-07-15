use std::any::Any;

pub(super) use self::private::DynRegistryResolver;
use super::{EntryArena, EntryIdx, RawEntryIdx, TypedEntry};
use crate::kind::EntryKind;

pub trait RegistryResolver: DynRegistryResolver {
	/// A type that can be used to uniquely identify an Entry between different
	/// packages within a registry. Should be constructable based on information
	/// available within the IR of a package that is consuming an external
	/// package's entry as the target.
	type EntryId: serde::Serialize + serde::de::DeserializeOwned + Any;

	/// resolves an `EntryId` to an actual `EntryIdx` that points to the given
	/// Entry.
	// TODO: determine error handling for bad usage: is a panic OK?
	fn idx_from_entry_id(&self, id: Self::EntryId) -> RawEntryIdx;

	/// resolves an EntryIdx to it's unique internal `EntryId`
	fn entry_id_from_idx(&self, idx: RawEntryIdx) -> Self::EntryId;

	// TODO: unique package identifier too?
	fn resolve_package(&self, package_index: usize) -> &EntryArena;

	fn resolve<T>(&self, index: EntryIdx<T>) -> &TypedEntry<T>
	where
		T: EntryKind,
	{
		// TODO
		let package = self.resolve_package(index.package_idx().index());
		let entry = package.resolve(index.arena_idx());

		TypedEntry::new(entry)
	}
}

impl<T: RegistryResolver> DynRegistryResolver for T {
	fn raw_entry_id_from_idx(&self, idx: RawEntryIdx) -> Box<dyn erased_serde::Serialize> {
		Box::new(self.entry_id_from_idx(idx))
	}

	fn deser_entry_id_to_idx(
		&self,
		deserializer: &mut dyn erased_serde::Deserializer,
	) -> erased_serde::Result<RawEntryIdx> {
		erased_serde::deserialize(deserializer).map(|id| self.idx_from_entry_id(id))
	}
}

mod private {
	use super::RawEntryIdx;

	/// An internal-only auto-implemented subtrait of `Registry` that's used to do
	/// type-erased shenanigans to allow (de)serializing `EntryIdx`s when using
	/// our context helpers
	///
	/// essentially, it's the backing behind the mapping between `EntryIdx` that
	/// exists in-memory and the `EntryId` that's actually (de)serialized.
	pub trait DynRegistryResolver {
		/// gets the `EntryId` for the corresponding `RawEntryIdx` and converts it
		/// to a type-erased serializable type
		fn raw_entry_id_from_idx(&self, idx: RawEntryIdx) -> Box<dyn erased_serde::Serialize>;

		fn deser_entry_id_to_idx(
			&self,
			deserializer: &mut dyn erased_serde::Deserializer,
		) -> erased_serde::Result<RawEntryIdx>;
	}
}
