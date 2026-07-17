use elsa::sync::FrozenVec;
use parking_lot::RwLock;

use crate::{
    List,
    entry::{Entry, TypedEntry},
    kind::EntryKind,
    module::Module,
    package::{PackageId, PackageMeta},
    symbol::Symbol,
};

use super::{
    EntryBuilder, EntryIdx, EntryLink, PackageIdx, RawEntryIdx, RegistryResolver, ScopeIdx,
};

type FxBiHashMap<T> = iddqd::BiHashMap<T, rustc_hash::FxBuildHasher>;

const BAD_PACKAGE_INDEX_ERROR: &str = "";
const BAD_SCOPE_INDEX_ERROR: &str = "";

pub struct RegistryState {
    packages: RwLock<FxBiHashMap<RegistryPackage>>,

    scopes: FrozenVec<List<Entry>>,
    links: FrozenVec<List<EntryLink>>,
}

impl RegistryState {
    pub fn resolve<R: RegistryResolver>(&self, idx: RawEntryIdx, _resolver: &R) -> &Entry {
        match idx.repr() {
            super::idx::Repr::Resolved {
                package_idx,
                scope_idx,
            } => self.index_resolved(package_idx, scope_idx),
            super::idx::Repr::Deferred(_idx) => {
                todo!("handle Deferred EntryIdx")
            }
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
    pub(super) fn new() -> Self {
        RegistryState {
            packages: RwLock::new(FxBiHashMap::default()),

            scopes: FrozenVec::new(),
            links: FrozenVec::new(),
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
            .deferred_start(super::DeferredIdx::new(0)) // TODO
            .build(|b| {
                build(b);
                Module
            });

        self.scopes.push(entries.into_boxed_slice());
        self.links.push(links.into_boxed_slice());

        let _ = self
            .packages
            .write()
            .insert_unique(RegistryPackage::new(package_idx, pkg)); // TODO: how to handle overwriting package?

        idx.typed()
    }

    fn index_resolved(&self, pkg: PackageIdx, entry: ScopeIdx) -> &Entry {
        self.scopes
            .get(pkg.index())
            .expect(BAD_PACKAGE_INDEX_ERROR)
            .get(entry.index())
            .expect(BAD_SCOPE_INDEX_ERROR)
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

impl iddqd::BiHashItem for RegistryPackage {
    type K1<'a> = &'a PackageId;
    type K2<'a> = PackageIdx;

    fn key1(&self) -> Self::K1<'_> {
        &self.meta.id
    }

    fn key2(&self) -> Self::K2<'_> {
        self.idx
    }

    iddqd::bi_upcast!();
}
