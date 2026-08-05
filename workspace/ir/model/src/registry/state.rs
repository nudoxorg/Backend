use elsa::sync::FrozenVec;
use papaya::HashMap;
use rustc_hash::FxBuildHasher;
use tokio::sync::{Mutex, OnceCell};

use crate::{
    entry::Entry,
    id::{PackageId, UniqueId},
    index::{Ref, UntypedEntryIndex},
    package::PackageInfo,
    visitor::Visitor,
};

use super::RegistryResolver;

// TODO: better error message
const INVALID_ENTRY_IDX_MESSAGE: &str = "got invalid EntryIndex (out of bounds index)";

pub(super) struct RegistryState<R: RegistryResolver> {
    // TODO: papaya or dashmap?
    packages: HashMap<PackageId, StoredPackage>,
    inner: StateInner<R::EntryId>,
}

struct StoredEntry<Id> {
    id: UniqueId<Id>,
    entry: OnceCell<Entry>,
}

impl<Id> StoredEntry<Id> {
    fn new(id: UniqueId<Id>) -> Self {
        Self {
            id,
            entry: OnceCell::new(),
        }
    }
}

impl<R: RegistryResolver> Default for RegistryState<R> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: RegistryResolver> RegistryState<R> {
    pub(super) fn new() -> Self {
        RegistryState {
            packages: HashMap::new(),
            inner: StateInner::new(FrozenVec::new(), |_| HashMap::default()),
        }
    }

    pub(super) fn resolve_idx_to_id(&self, idx: UntypedEntryIndex) -> &UniqueId<R::EntryId> {
        &self.idx_to_stored_entry(idx).id
    }

    pub(super) fn resolve_loaded_id(&self, id: &UniqueId<R::EntryId>) -> Option<UntypedEntryIndex> {
        self.inner
            .with_lookup(|lookup| lookup.pin().get(id).copied())
    }

    pub(super) fn resolve_id_to_idx(&self, id: UniqueId<R::EntryId>) -> UntypedEntryIndex {
        self.inner.with(|it| {
            let lookup = it.lookup.pin();

            if let Some(idx) = lookup.get(&id) {
                *idx
            } else {
                let idx = UntypedEntryIndex::resolved(it.entries.len());
                let id = &it.entries.push_get(Box::new(StoredEntry::new(id))).id;

                let _old = lookup.insert(id, idx);
                debug_assert_eq!(_old, None);

                idx
            }
        })
    }

    pub(super) fn resolve_loaded_entry(&self, idx: UntypedEntryIndex) -> Option<&Entry> {
        self.idx_to_stored_entry(idx).entry.get()
    }

    pub(super) async fn resolve_entry(
        &self,
        idx: UntypedEntryIndex,
        resolver: &Mutex<R>,
    ) -> Result<&Entry, R::Error> {
        let entry = self
            .inner
            .with_entries(|entries| entries.get(idx.resolved_index()))
            .expect(INVALID_ENTRY_IDX_MESSAGE);

        entry
            .entry
            .get_or_try_init(async || {
                let packages = self.packages.pin_owned();

                let package =
                    packages.get_or_insert_with(entry.id.package(), StoredPackage::default);

                let package = package
                    .package
                    .get_or_try_init(async || {
                        resolver
                            .lock()
                            .await
                            .load_package_info(entry.id.package())
                            .await
                            .map(|info| Package::new(info, self))
                    })
                    .await?;

                let mut entry = resolver.lock().await.load_unique_id(&entry.id).await?;

                entry.visit_mut(&|r| {
                    if let Ref::Local(idx) = r {
                        *idx = package.resolve(*idx);
                    }
                });

                Ok(entry)
            })
            .await
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = UntypedEntryIndex> {
        // FIXME: this is lazy. do we want to go to the trouble
        // of writing a proper iterator that stores a reference
        // to entries (can we even do that with ouroboros) so
        // that new entries are appended to the iterator?
        self.inner
            .with_entries(|entries| (0..entries.len()).map(UntypedEntryIndex::resolved))
    }

    fn idx_to_stored_entry(&self, idx: UntypedEntryIndex) -> &StoredEntry<R::EntryId> {
        self.inner
            .with_entries(|e| e.get(idx.resolved_index()))
            .expect(INVALID_ENTRY_IDX_MESSAGE)
    }
}

#[ouroboros::self_referencing]
struct StateInner<Id: 'static> {
    entries: FrozenVec<Box<StoredEntry<Id>>>,

    #[borrows(entries)]
    #[not_covariant]
    lookup: HashMap<&'this UniqueId<Id>, UntypedEntryIndex, FxBuildHasher>,
}

#[derive(Default)]
struct StoredPackage {
    package: OnceCell<Package>,
}

struct Package {
    imports: Vec<UntypedEntryIndex>,
    exports: Vec<UntypedEntryIndex>,
}

impl Package {
    fn new<R: RegistryResolver>(info: PackageInfo<R::EntryId>, state: &RegistryState<R>) -> Self {
        let package = info.id();

        let (imports, exports) = info.iters();

        Package {
            imports: imports.map(|id| state.resolve_id_to_idx(id)).collect(),
            exports: exports
                .map(|id| UniqueId::build(PackageId::clone(&package), id))
                .map(|id| state.resolve_id_to_idx(id))
                .collect(),
        }
    }

    fn resolve(&self, idx: UntypedEntryIndex) -> UntypedEntryIndex {
        if idx.is_import() {
            self.imports[idx.import_index()]
        } else {
            self.exports[idx.export_index()]
        }
    }
}
