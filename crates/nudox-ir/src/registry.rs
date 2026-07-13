use crate::{arena::EntryArena, entry::TypedEntry, idx::EntryIdx, kind::EntryKind};

// TODO: be able to implement std::ops::Index<EntryIdx<T>> for `impl Registry`
pub trait Registry {
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
