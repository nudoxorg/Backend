mod arena;
mod builder;
mod idx;
mod link;
mod resolver;
mod state;

#[cfg(test)]
mod tests;

use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::PackageId, symbol::Symbol};

use self::{arena::EntryArena, resolver::DynRegistryResolver};

// reexport at pub(crate) level to allow test_helpers to use
#[cfg(test)]
pub(crate) use self::idx::{ArenaIdx, PackageIdx};

#[cfg(not(test))]
use self::idx::{ArenaIdx, PackageIdx};

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

	pub fn resolve(&self, idx: RawEntryIdx) -> &Entry { self.state.resolve(idx) }

	pub fn resolve_typed<T: EntryKind>(&self, idx: EntryIdx<T>) -> &TypedEntry<T> {
		self.state.resolve_typed(idx)
	}

	pub fn build_package_ir(
		&mut self,
		package: PackageId,
		sym: Symbol,
		build: impl FnOnce(&mut EntryBuilder),
	) -> EntryIdx<Module> {
		self.state.build_package_ir::<R>(package, sym, build)
	}
}

impl<T: EntryKind, R: RegistryResolver> std::ops::Index<EntryIdx<T>> for Registry<R> {
	type Output = TypedEntry<T>;

	fn index(&self, index: EntryIdx<T>) -> &Self::Output { self.resolve_typed(index) }
}
