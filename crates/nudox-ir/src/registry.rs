use crate::{arena::EntryArena, entry::TypedEntry, idx::EntryIdx, kind::EntryKind};

// TODO: be able to implement std::ops::Index<EntryIdx<T>> for `impl Registry`
pub trait Registry {
	/// A type that can be used to uniquely identify an Entry between different
	/// packages within a registry. Should be constructable based on information
	/// available within the IR of a package that is consuming an external
	/// package's entry as the target.
	type EntryId;

	/// resolves an EntryId to an actual EntryIdx that points to the given Entry.
	// TODO: determine error handling for bad usage: is a panic OK?
	fn idx_from_entry_id<T>(&self, id: Self::EntryId) -> EntryIdx<T>;

	fn resolve_package(&self, package_index: usize) -> &EntryArena;

	#[expect(private_bounds)]
	fn resolve<T>(&self, index: EntryIdx<T>) -> &TypedEntry<T>
	where
		T: EntryKind,
	{
		let package = self.resolve_package(index.package());
		let entry = package.resolve(index.index());

		TypedEntry::new(entry)
	}
}
