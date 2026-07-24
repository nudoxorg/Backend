mod resolver;
mod state;

#[cfg(test)]
mod tests;

use parking_lot::Mutex;

use crate::{
    entry::{Entry, TypedEntry},
    id::UniqueId,
    index::{EntryIndex, UntypedEntryIndex},
    kind::EntryKind,
};

use self::state::RegistryState;

pub use self::resolver::RegistryResolver;

#[derive(Default)]
pub struct Registry<R: RegistryResolver> {
    resolver: Mutex<R>,
    state: RegistryState<R>,
}

impl<R: RegistryResolver> Registry<R> {
    pub fn new(resolver: R) -> Self {
        Registry {
            resolver: Mutex::new(resolver),
            state: RegistryState::new(),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = UntypedEntryIndex> {
        self.state.iter()
    }

    pub fn resolve_idx_to_id(&self, idx: UntypedEntryIndex) -> &UniqueId<R::EntryId> {
        self.state.resolve_idx_to_id(idx)
    }

    pub fn resolved_loaded_id(&self, id: &UniqueId<R::EntryId>) -> Option<UntypedEntryIndex> {
        self.state.resolve_loaded_id(id)
    }

    pub fn resolve_id_to_idx(&self, id: UniqueId<R::EntryId>) -> UntypedEntryIndex {
        self.state.resolve_id_to_idx(id)
    }

    pub fn resolve_loaded_entry(&self, idx: UntypedEntryIndex) -> Option<&Entry> {
        self.state.resolve_loaded_entry(idx)
    }

    pub async fn resolve_entry(&self, idx: UntypedEntryIndex) -> Result<&Entry, R::Error> {
        self.state
            .resolve_entry(idx, &mut self.resolver.lock())
            .await
    }

    pub async fn resolve_typed_entry<T: EntryKind>(
        &self,
        idx: EntryIndex<T>,
    ) -> Result<&TypedEntry<T>, R::Error> {
        self.state
            .resolve_entry(idx.raw(), &mut self.resolver.lock())
            .await
            .map(TypedEntry::new)
    }
}
