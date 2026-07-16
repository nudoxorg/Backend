use bimap::BiHashMap;
use elsa::sync::FrozenVec;
use parking_lot::RwLock;
use rustc_hash::FxHashSet;

use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::{PackageId, PackageMeta}, symbol::Symbol};

use super::{ArenaIdx, EntryArena, EntryBuilder, EntryIdx, EntryLink, PackageIdx, RawEntryIdx, RegistryResolver};

type BiFxHashMap<L, R> = BiHashMap<L, R, rustc_hash::FxBuildHasher, rustc_hash::FxBuildHasher>;

pub struct RegistryState {
	arenas: FrozenVec<Box<EntryArena>>,

	packages: RwLock<BiFxHashMap<PackageId, PackageIdx>>,
	links:    RwLock<FxHashSet<EntryLink>>,
}

// TODO: make this properly
#[derive(Clone, Copy)]
pub struct PackageIRView<'a> {
	package_idx: PackageIdx,
	arena:       &'a EntryArena,
}

impl<'a> PackageIRView<'a> {
	pub fn enumerate(self) -> impl Iterator<Item = (RawEntryIdx, &'a Entry)> {
		self.arena.iter().enumerate().map(move |(arena_idx, entry)| {
			(RawEntryIdx::new(self.package_idx, ArenaIdx::new(arena_idx)), entry)
		})
	}
}

impl RegistryState {
	pub fn resolve_package<R: RegistryResolver>(
		&self,
		id: &PackageId,
		_fetch: impl FnOnce(&Self) -> Vec<Entry>,
	) -> PackageIRView<'_> {
		match self.packages.read().get_by_left(&id) {
			Some(&idx) => PackageIRView { package_idx: idx, arena: self.arena(idx) },
			None => {
				// TODO: use fetch to resolve the IR to be loaded and insert it into the
				// registry state
				todo!()
			}
		}
	}

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

	pub fn package_id_of(&self, _idx: RawEntryIdx) -> PackageId {
		todo!()
		// self.packages.read().get_by_right(&idx.package_idx()).cloned().expect("")
		// // TODO: error message
	}
}

impl RegistryState {
	pub(super) fn new() -> Self {
		RegistryState {
			arenas:   FrozenVec::new(),
			packages: RwLock::new(BiFxHashMap::default()),
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

		let (entries, links) = EntryBuilder::builder().sym(sym).entry_idx(idx).build(|b| {
			build(b);
			Module
		});

		self.arenas.push(Box::new(EntryArena::new(entries)));

		self.links.write().extend(links);
		self.packages.write().insert(pkg.id, package_idx);

		idx.typed()
	}

	fn arena(&self, idx: PackageIdx) -> &EntryArena {
		self.arenas.get(idx.index()).expect("") // TODO: error message
	}
}
