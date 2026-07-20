mod builder;
mod id;
mod idx;
mod link;
mod resolver;
mod serde_impl;
mod state;

#[cfg(test)]
mod tests;

use crate::{
    entry::{Entry, TypedEntry},
    kind::EntryKind,
    package::PackageMeta,
    symbol::Symbol,
};

use self::{
    id::ErasedUniqueId,
    state::{RegistryState, StoredEntry},
};

pub use self::{
    builder::EntryBuilder,
    id::{EntryId, UniqueId},
    idx::{EntryIdx, RawEntryIdx},
    link::EntryLink,
    resolver::RegistryResolver,
    serde_impl::DeserContext,
};

#[derive(Default)]
pub struct Registry<R> {
    resolver: R,
    state: RegistryState,
}

impl<R: RegistryResolver> Registry<R> {
    pub fn build_package_ir(
        &self,
        pkg: PackageMeta,
        sym: Symbol,
        build: impl Fn(&mut EntryBuilder<R>),
    ) {
        self.state.build_package_ir(pkg, sym, build)
    }

    pub fn iter(&self) -> impl Iterator<Item = RawEntryIdx> {
        self.state.iter()
    }

    pub fn resolve_id(&self, idx: RawEntryIdx) -> &UniqueId<R::EntryId> {
        self.state.resolve_id::<R>(idx)
    }

    pub fn resolve_idx(&self, id: UniqueId<R::EntryId>) -> RawEntryIdx {
        self.state.resolve_idx::<R>(id)
    }

    pub async fn resolve_entry(&self, idx: RawEntryIdx) -> Result<&Entry, R::Error> {
        self.state.resolve_entry(idx, &self.resolver).await
    }

    pub async fn resolve_typed_entry<T: EntryKind>(
        &self,
        idx: EntryIdx<T>,
    ) -> Result<&TypedEntry<T>, R::Error> {
        self.state
            .resolve_entry(idx.raw(), &self.resolver)
            .await
            .map(TypedEntry::new)
    }
}

impl<R> Registry<R> {
    pub fn new(resolver: R) -> Self {
        Registry {
            resolver,
            state: RegistryState::new(),
        }
    }

    pub fn resolver(&self) -> &R {
        &self.resolver
    }

    pub fn resolver_mut(&mut self) -> &mut R {
        &mut self.resolver
    }
}
