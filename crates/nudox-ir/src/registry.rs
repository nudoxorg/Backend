mod arena;
mod builder;
mod idx;
mod link;
mod resolver;
mod state;

#[cfg(test)]
mod tests;

use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::{PackageId, PackageMeta}, symbol::Symbol};

use self::{arena::EntryArena, idx::{ArenaIdx, DeferredIdx, PackageIdx}, resolver::DynRegistryResolver};

// allow test_helpers to create EntryIdx's
#[cfg(test)]
pub(crate) fn new_idx<T>(package: usize, arena: usize) -> EntryIdx<T> {
	EntryIdx::new(PackageIdx::new(package), ArenaIdx::new(arena))
}

pub use self::{builder::EntryBuilder, idx::{EntryIdx, RawEntryIdx}, link::EntryLink, resolver::{EntryId, RegistryResolver}, state::RegistryState};

pub struct Registry<R> {
	resolver: R,
	state:    RegistryState,
}

impl<R> Registry<R> {
	pub fn new(resolver: R) -> Self { Registry { resolver, state: RegistryState::new() } }
}

impl<R: RegistryResolver> Registry<R> {
	pub fn serialize<T, S>(&self, serializer: S, it: &T) -> Result<S::Ok, S::Error>
	where
		T: serde::Serialize,
		S: serde::Serializer,
	{
		serde_context::serialize_with_context(
			it,
			serializer,
			(&self.resolver as &dyn DynRegistryResolver, &self.state),
		)
	}

	pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
	where
		T: serde::Deserialize<'de>,
		D: serde::Deserializer<'de>,
	{
		serde_context::deserialize_with_context(
			deserializer,
			(&self.resolver as &dyn DynRegistryResolver, &self.state),
		)
	}

	pub fn resolve(&self, idx: RawEntryIdx) -> &Entry { self.state.resolve(idx, &self.resolver) }

	pub fn resolve_typed<T: EntryKind>(&self, idx: EntryIdx<T>) -> &TypedEntry<T> {
		self.state.resolve_typed(idx, &self.resolver)
	}

	pub fn build_package_ir(
		&mut self,
		pkg: PackageMeta,
		sym: Symbol,
		build: impl FnOnce(&mut EntryBuilder),
	) -> EntryIdx<Module> {
		self.state.build_package_ir(pkg, sym, build)
	}
}

impl<T: EntryKind, R: RegistryResolver> std::ops::Index<EntryIdx<T>> for Registry<R> {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output { self.resolve_typed(index) }
}
