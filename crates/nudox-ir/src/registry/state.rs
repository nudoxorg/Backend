use std::sync::OnceLock;

use dashmap::DashMap;
use elsa::sync::FrozenVec;
use rustc_hash::FxBuildHasher;

use crate::{entry::Entry, kinds::Module, package::PackageMeta, symbol::Symbol};

use super::{EntryBuilder, EntryIdx, ErasedUniqueId, RawEntryIdx, UniqueId};

const INVALID_ENTRY_IDX_MESSAGE: &str = ""; // TODO

pub struct RegistryState {
    inner: EntriesState,
}

impl RegistryState {
    // TODO: public API for RegistryResolver to use
}

impl RegistryState {
    pub(super) fn new() -> Self {
        RegistryState {
            inner: EntriesState::new(FrozenVec::new(), |_| DashMap::default()),
        }
    }

    pub(super) fn build_package_ir(
        &self,
        pkg: PackageMeta,
        sym: Symbol,
        build: impl Fn(&mut EntryBuilder),
    ) {
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
    entry: OnceLock<Entry>,
}

impl StoredEntry {
    pub(super) fn resolved(id: ErasedUniqueId, entry: Entry) -> Self {
        StoredEntry {
            id,
            entry: OnceLock::from(entry),
        }
    }

    pub(super) fn deferred(id: ErasedUniqueId) -> Self {
        StoredEntry {
            id,
            entry: OnceLock::new(),
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
