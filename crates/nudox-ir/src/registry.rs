mod builder;
mod id;
mod idx;
mod link;
mod resolver;
mod serde_impl;
mod state;

#[cfg(test)]
mod tests;

use crate::{package::PackageMeta, symbol::Symbol};

use self::{id::ErasedUniqueId, state::StoredEntry};

pub use self::{
    builder::EntryBuilder,
    id::{EntryId, UniqueId},
    idx::{EntryIdx, RawEntryIdx},
    link::EntryLink,
    resolver::RegistryResolver,
    state::RegistryState,
};

pub struct Registry<R> {
    resolver: R,
    state: RegistryState,
}

impl<R> Registry<R> {
    pub fn new(resolver: R) -> Self {
        Registry {
            resolver,
            state: RegistryState::new(),
        }
    }

    pub fn build_package_ir(
        &self,
        pkg: PackageMeta,
        sym: Symbol,
        build: impl Fn(&mut EntryBuilder),
    ) {
        self.state.build_package_ir(pkg, sym, build)
    }
}

// allow test_helpers to create EntryIdx's
#[cfg(test)]
pub(crate) fn new_idx<T>(index: usize) -> EntryIdx<T> {
    EntryIdx::new(index)
}
