use elsa::sync::FrozenVec;
use parking_lot::RwLock;
use rustc_hash::FxHashSet;

use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::{PackageId, PackageMeta}, symbol::Symbol};

use super::{ArenaIdx, EntryArena, EntryBuilder, EntryIdx, EntryLink, PackageIdx, RawEntryIdx, RegistryResolver};

type FxBiHashMap<T> = iddqd::BiHashMap<T, rustc_hash::FxBuildHasher>;

pub struct RegistryState {
	arenas: FrozenVec<Box<EntryArena>>,

	packages: RwLock<FxBiHashMap<RegistryPackage>>,
	links:    RwLock<FxHashSet<EntryLink>>,
}

impl RegistryState {
	pub fn resolve<R: RegistryResolver>(&self, idx: RawEntryIdx, _resolver: &R) -> &Entry {
		match idx.repr() {
			super::idx::Repr::Resolved { package_idx, arena_idx } => {
				self.arena(package_idx).entry(arena_idx)
			}
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
			arenas:   FrozenVec::new(),
			packages: RwLock::new(FxBiHashMap::default()),
			links:    RwLock::new(FxHashSet::default()),
		}
	}

	pub(crate) fn build_package_ir(
		&self,
		pkg: PackageMeta,
		sym: Symbol,
		build: impl FnOnce(&mut EntryBuilder),
	) -> EntryIdx<Module> {
		let package_idx = PackageIdx::new(self.arenas.len());
		let arena_idx = ArenaIdx::new(0);

		let idx = EntryIdx::new(package_idx, arena_idx);

		let (idx, entries, links, _deferred) = EntryBuilder::builder()
			.sym(sym)
			.entry_idx(idx)
			.deferred_start(super::DeferredIdx::new(0)) // TODO
			.build(|b| {
				build(b);
				Module
			});

		self.arenas.push(Box::new(EntryArena::new(entries)));

		self.links.write().extend(links);
		let _ = self.packages.write().insert_unique(RegistryPackage::new(package_idx, pkg)); // TODO: how to handle overwriting package?

		idx.typed()
	}

	fn arena(&self, idx: PackageIdx) -> &EntryArena {
		self.arenas.get(idx.index()).expect("") // TODO: error message
	}
}

struct RegistryPackage {
	idx:  PackageIdx,
	meta: PackageMeta,
}

impl RegistryPackage {
	fn new(idx: PackageIdx, meta: PackageMeta) -> Self { Self { idx, meta } }
}

impl iddqd::BiHashItem for RegistryPackage {
	type K1<'a> = &'a PackageId;
	type K2<'a> = PackageIdx;

	fn key1(&self) -> Self::K1<'_> { &self.meta.id }

	fn key2(&self) -> Self::K2<'_> { self.idx }

	iddqd::bi_upcast!();
}
