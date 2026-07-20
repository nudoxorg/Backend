use dashmap::DashMap;
use elsa::sync::FrozenVec;
use rustc_hash::FxBuildHasher;
use tokio::sync::OnceCell;

use crate::{
    entry::Entry, kinds::Module, package::PackageMeta, registry::DeserContext, symbol::Symbol,
};

use super::{EntryBuilder, EntryIdx, ErasedUniqueId, RawEntryIdx, RegistryResolver, UniqueId};

const INVALID_ENTRY_IDX_MESSAGE: &str = ""; // TODO

#[repr(transparent)]
pub(super) struct RegistryState {
    inner: EntriesState,
}

impl Default for RegistryState {
    fn default() -> Self {
        RegistryState::new()
    }
}

impl RegistryState {
    pub(super) fn build_package_ir<R>(
        &self,
        pkg: PackageMeta,
        sym: Symbol,
        build: impl Fn(&mut EntryBuilder<R>),
    ) where
        R: RegistryResolver,
    {
        let idx = self.inner.with_entries(|e| EntryIdx::new(e.len()));

        let built = EntryBuilder::builder()
            .id(UniqueId::root(pkg.id))
            .idx(idx)
            .build::<Module>(sym, None, |b| {
                build(b);
                Module
            });

        for entry in built.tree.iter() {
            self.insert_entry(entry);
        }

        // TODO: how to handle IR links?
        let _links = built.links;
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = RawEntryIdx> {
        self.inner.with_entries(|entries| {
            // FIXME: this is lazy. do we want to go to the trouble
            // of writing a proper iterator that stores a reference
            // to entries (can we even do that with ouroboros) so
            // that new entries are appended to the iterator
            (0..entries.len()).map(RawEntryIdx::new)
        })
    }

    pub(super) fn resolve_id<R: RegistryResolver>(
        &self,
        idx: RawEntryIdx,
    ) -> &UniqueId<R::EntryId> {
        self.inner.with_entries(|entries| {
            entries
                .get(idx.index())
                .expect(INVALID_ENTRY_IDX_MESSAGE)
                .id
                .downcast_ref::<R>()
        })
    }

    pub(super) fn resolve_idx<R: RegistryResolver>(&self, id: UniqueId<R::EntryId>) -> RawEntryIdx {
        self.unique_id_to_entry_idx(id.upcast())
    }

    pub(super) async fn resolve_entry<R>(
        &self,
        idx: RawEntryIdx,
        resolver: &R,
    ) -> Result<&Entry, R::Error>
    where
        R: RegistryResolver,
    {
        let entry = self.inner.with(|it| {
            it.entries
                .get(idx.index())
                .expect(INVALID_ENTRY_IDX_MESSAGE)
        });

        entry
            .entry
            .get_or_try_init(|| {
                resolver.load_unique_id(entry.id.downcast_ref::<R>(), DeserContext::new::<R>(self))
            })
            .await
    }
}

impl RegistryState {
    pub(super) fn new() -> Self {
        RegistryState {
            inner: EntriesState::new(FrozenVec::new(), |_| DashMap::default()),
        }
    }

    pub(super) fn unique_id_to_entry_idx(&self, id: ErasedUniqueId) -> RawEntryIdx {
        self.inner.with(|it| match it.lookup.get(&id) {
            Some(idx) => *idx,
            None => self.insert_entry(StoredEntry::deferred(id)),
        })
    }

    pub(super) fn entry_idx_to_unique_id(&self, idx: RawEntryIdx) -> &ErasedUniqueId {
        &self
            .inner
            .with_entries(|entries| entries.get(idx.index()))
            .expect(INVALID_ENTRY_IDX_MESSAGE)
            .id
    }
}

impl RegistryState {
    fn insert_entry(&self, entry: StoredEntry) -> RawEntryIdx {
        self.inner.with(|it| {
            let idx = RawEntryIdx::new(it.entries.len());

            let id = &it.entries.push_get(Box::new(entry)).id;

            let _old = it.lookup.insert(id, idx);
            debug_assert!(_old.is_none(), "overwrote existing UniqueId");

            idx
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct StoredEntry {
    id: ErasedUniqueId,
    entry: OnceCell<Entry>,
}

impl StoredEntry {
    pub(super) fn resolved(id: ErasedUniqueId, entry: Entry) -> Self {
        StoredEntry {
            id,
            entry: OnceCell::from(entry),
        }
    }

    pub(super) fn deferred(id: ErasedUniqueId) -> Self {
        StoredEntry {
            id,
            entry: OnceCell::new(),
        }
    }

    pub(super) fn id(&self) -> &ErasedUniqueId {
        &self.id
    }

    pub(super) fn init(&self, entry: Entry) {
        let _r = self.entry.set(entry);

        debug_assert!(
            _r.is_ok(),
            "called StoredEntry::init on already-initialized entry"
        )
    }
}

#[ouroboros::self_referencing]
struct EntriesState {
    entries: FrozenVec<Box<StoredEntry>>,

    #[borrows(entries)]
    #[not_covariant]
    lookup: DashMap<&'this ErasedUniqueId, RawEntryIdx, FxBuildHasher>,
}
