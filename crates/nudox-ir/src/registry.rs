mod resolver;
mod state;

#[cfg(test)]
mod tests;

use tokio::sync::Mutex;

use crate::{
    entry::{Entry, TypedEntry},
    id::UniqueId,
    index::{EntryIndex, UntypedEntryIndex},
    kind::EntryKind,
};

use self::state::RegistryState;

pub use self::resolver::RegistryResolver;

/// A lazy-loading registry of [`Entry`] values, indexed by [`EntryIndex`].
///
/// # Resolution model
///
/// Resolution works in two tiers:
///
/// 1. **Index allocation** (`resolve_id_to_idx`): synchronously assigns an
///    `EntryIndex` to a [`UniqueId`]. This is O(1) and never blocks on I/O. The
///    entry's data may not be loaded yet.
///
/// 2. **Entry loading** (`resolve_entry`): asynchronously fetches the entry's
///    data via the [`RegistryResolver`] on first access. Subsequent calls
///    return a cached reference.
///
/// This means you can cheaply allocate indices for hundreds of thousands of
/// entries without blocking, and only pay the I/O cost for entries that are
/// actually visited.
///
/// # Thread safety
///
/// `Registry` is `Send + Sync`. The resolver is wrapped in a `Mutex` (because
/// resolvers are typically stateful), while the internal index tables use
/// lock-free concurrent data structures.
#[derive(Default)]
pub struct Registry<R: RegistryResolver> {
    // TODO: should we let the RegistryResolver provider handle internal mutation itself?
    // that would add flexibilty, at the cost of implementors having to manage internal mutabilty
    // themselves if they need to do mutation
    resolver: Mutex<R>,

    state: RegistryState<R>,
}

impl<R: RegistryResolver> Registry<R> {
    /// Create a new registry with the given resolver.
    pub fn new(resolver: R) -> Self {
        Registry {
            resolver: Mutex::new(resolver),
            state: RegistryState::new(),
        }
    }

    /// Iterate over every known entry index (loaded or not).
    ///
    /// The returned iterator yields indices in allocation order
    pub fn iter(&self) -> impl Iterator<Item = UntypedEntryIndex> {
        self.state.iter()
    }

    /// Reverse-lookup: convert a resolved index back to its [`UniqueId`].
    ///
    /// # Panics
    ///
    /// Panics if `idx` has not been allocated by this registry.
    pub fn resolve_idx_to_id(&self, idx: UntypedEntryIndex) -> &UniqueId<R::EntryId> {
        self.state.resolve_idx_to_id(idx)
    }

    /// Check if a [`UniqueId`] has already been allocated an index.
    ///
    /// Returns `None` if the id has never been registered.
    pub fn resolved_loaded_id(&self, id: &UniqueId<R::EntryId>) -> Option<UntypedEntryIndex> {
        self.state.resolve_loaded_id(id)
    }

    /// Allocate (or retrieve) an index for the given [`UniqueId`].
    ///
    /// This is synchronous and does not trigger I/O. The returned index can be
    /// passed to [`resolve_entry`] to load the actual data.
    pub fn resolve_id_to_idx(&self, id: UniqueId<R::EntryId>) -> UntypedEntryIndex {
        self.state.resolve_id_to_idx(id)
    }

    /// Return the cached entry for `idx`, if it has already been loaded.
    pub fn resolve_loaded_entry(&self, idx: UntypedEntryIndex) -> Option<&Entry> {
        self.state.resolve_loaded_entry(idx)
    }

    /// Resolve an entry by index, loading it via the [`RegistryResolver`] if
    /// needed.
    ///
    /// On first call for a given index, this will:
    /// 1. Lock the resolver.
    /// 2. Call [`RegistryResolver::load_package_info`] for the entry's package
    ///    (if not already cached).
    /// 3. Call [`RegistryResolver::load_unique_id`] to fetch the entry.
    /// 4. Remap any serialized (export/import) indices within the entry to
    ///    resolved indices via the package's import/export tables.
    /// 5. Cache the entry and return a reference.
    ///
    /// Subsequent calls return the cached reference instantly.
    pub async fn resolve_entry(&self, idx: UntypedEntryIndex) -> Result<&Entry, R::Error> {
        self.state.resolve_entry(idx, &self.resolver).await
    }

    /// Convenience wrapper around [`resolve_entry`] that re-types the index.
    pub async fn resolve_typed_entry<T: EntryKind>(
        &self,
        idx: EntryIndex<T>,
    ) -> Result<&TypedEntry<T>, R::Error> {
        self.resolve_entry(idx.raw()).await.map(TypedEntry::new)
    }
}
