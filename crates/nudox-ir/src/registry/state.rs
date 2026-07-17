use elsa::sync::FrozenVec;
use iddqd::{BiHashItem, BiHashMap, bi_upcast};
use parking_lot::RwLock;
use rustc_hash::FxBuildHasher;

use crate::{
    List,
    entry::{Entry, TypedEntry},
    kind::EntryKind,
    module::Module,
    package::{PackageId, PackageMeta},
    symbol::Symbol,
};

use super::{
    DeferredEntry, DeferredIdx, EntryBuilder, EntryIdx, EntryLink, PackageIdx, RawEntryIdx,
    RegistryResolver, ScopeIdx,
};

// TODO: panic messages for invalid usage
const BAD_PACKAGE_INDEX_ERROR: &str = "";
const BAD_SCOPE_INDEX_ERROR: &str = "";
const BAD_DEFERRED_INDEX_ERROR: &str = "";

pub struct RegistryState {
    packages: RwLock<BiHashMap<RegistryPackage, FxBuildHasher>>,

    scopes: FrozenVec<List<Entry>>,
    links: FrozenVec<List<EntryLink>>,

    deferred: RwLock<Vec<DeferredEntry>>,
}

impl RegistryState {
    pub fn resolve<R: RegistryResolver>(&self, idx: RawEntryIdx, resolver: &R) -> &Entry {
        match idx.repr() {
            super::idx::Repr::Resolved {
                package_idx,
                scope_idx,
            } => self.index_resolved(package_idx, scope_idx),
            super::idx::Repr::Deferred(deferred_idx) => self.index_deferred(deferred_idx, resolver),
        }
    }

    pub fn resolve_typed<T: EntryKind, R: RegistryResolver>(
        &self,
        idx: EntryIdx<T>,
        resolver: &R,
    ) -> &TypedEntry<T> {
        TypedEntry::new(self.resolve(idx.raw(), resolver))
    }
}

impl RegistryState {
    pub(super) const fn new() -> Self {
        RegistryState {
            packages: RwLock::new(BiHashMap::with_hasher(FxBuildHasher)),

            scopes: FrozenVec::new(),
            links: FrozenVec::new(),
            deferred: RwLock::new(Vec::new()),
        }
    }

    pub(crate) fn build_package_ir(
        &self,
        pkg: PackageMeta,
        sym: Symbol,
        build: impl FnOnce(&mut EntryBuilder),
    ) -> EntryIdx<Module> {
        let package_idx = PackageIdx::new(self.scopes.len());
        let scope_idx = ScopeIdx::new(0);

        let idx = EntryIdx::new(package_idx, scope_idx);

        let (idx, entries, links, _deferred) = EntryBuilder::builder()
            .sym(sym)
            .entry_idx(idx)
            .deferred_start(DeferredIdx::new(0)) // TODO
            .build(|b| {
                build(b);
                Module
            });

        self.scopes.push(entries.into_boxed_slice());
        self.links.push(links.into_boxed_slice());

        let _ = self
            .packages
            .write()
            .insert_overwrite(RegistryPackage::new(package_idx, pkg)); // TODO: how to handle overwriting package?

        idx.typed()
    }

    fn index_resolved(&self, pkg: PackageIdx, entry: ScopeIdx) -> &Entry {
        self.scopes
            .get(pkg.index())
            .expect(BAD_PACKAGE_INDEX_ERROR)
            .get(entry.index())
            .expect(BAD_SCOPE_INDEX_ERROR)
    }

    fn index_deferred<R: RegistryResolver>(&self, idx: DeferredIdx, resolver: &R) -> &Entry {
        match self
            .deferred
            .read()
            .get(idx.index())
            .expect(BAD_DEFERRED_INDEX_ERROR)
        {
            DeferredEntry::Resolved(idx) => self.resolve(*idx, resolver),
            DeferredEntry::Deferred(_id) => {
                todo!("resolve deferred indices")
            }
        }
    }
}

struct RegistryPackage {
    idx: PackageIdx,
    meta: PackageMeta,
}

impl RegistryPackage {
    fn new(idx: PackageIdx, meta: PackageMeta) -> Self {
        Self { idx, meta }
    }
}

impl BiHashItem for RegistryPackage {
    type K1<'a> = &'a PackageId;
    type K2<'a> = PackageIdx;

    fn key1(&self) -> Self::K1<'_> {
        &self.meta.id
    }

    fn key2(&self) -> Self::K2<'_> {
        self.idx
    }

    bi_upcast!();
}
