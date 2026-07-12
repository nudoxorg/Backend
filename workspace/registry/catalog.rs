//! A registry catalog handle that is *typed* in three dimensions at once:
//!
//! - **brand** — a `generativity` invariant lifetime, so a [`PackageHandle`]
//!   this catalog issues can only ever be resolved against *this* catalog, never
//!   another. The brand exists purely at the type level.
//!
//! - **ecosystem** — lifted into a const generic ([`Language`] is `ConstParamTy`)
//!   so a Rust catalog and a TypeScript catalog are distinct types and can never
//!   be incidentally mixed.
//!
//! - **capability plane** — [`ReadOnly`] vs [`ReadWrite`], so an interface like
//!   the server's read surface literally cannot call `publish`.
//!
//! ## Brand discipline (important)
//! The `'brand` lifetime and [`PackageHandle`]s must **never** cross an `.await`
//! into stored state or onto the wire. The brand is a compile-time proof, not a
//! serializable value; persisting or sending one would be meaningless (its
//! guarantee doesn't survive the type boundary). Handles are strictly
//! same-call-site, same-stack ephemera.

use std::{cell::RefCell, marker::PhantomData};

use generativity::{Guard, Id};
use heart::Language;

use crate::{Package, package::PackageName};

mod sealed {
	pub trait Sealed {}
}

/// A read/write capability plane for a [`Catalog`] handle.
pub trait Plane: sealed::Sealed {}

/// A plane that additionally permits mutation. Only [`ReadWrite`] implements it,
/// so `publish` is unreachable from a [`ReadOnly`] handle.
pub trait Mutable: Plane {}

/// Read-only capability.
pub struct ReadOnly;

/// Read + write capability.
pub struct ReadWrite;

impl sealed::Sealed for ReadOnly {}
impl sealed::Sealed for ReadWrite {}
impl Plane for ReadOnly {}
impl Plane for ReadWrite {}
impl Mutable for ReadWrite {}

/// A handle to a package *within a specific catalog*, branded with that
/// catalog's invariant lifetime. Never persist or transmit one — see the module
/// docs on brand discipline.
#[derive(Clone, Copy)]
pub struct PackageHandle<'brand> {
	index: usize,
	brand: Id<'brand>,
}

/// The append-only package arena backing a catalog.
///
/// Each entry is boxed so its address is stable, and entries are never removed
/// or replaced while the catalog lives — together those two facts are what let
/// [`Catalog::resolve`] hand out a plain `&Package` borrowed from `&self`.
#[derive(Default)]
struct Arena {
	slots: RefCell<Vec<Box<Package>>>,
}

impl Arena {
	/// Append a package, returning its permanent slot index.
	fn push(&self, package: Package) -> usize {
		let mut slots = self.slots.borrow_mut();
		slots.push(Box::new(package));
		slots.len() - 1
	}

	/// A stable shared borrow of the package in `index`'s slot.
	///
	/// Panics on an out-of-range index — unreachable for handles this catalog
	/// minted, which is the only way an index reaches here (brand discipline).
	fn get(&self, index: usize) -> &Package {
		let slots = self.slots.borrow();
		let package: *const Package = &*slots[index];
		// SAFETY: the arena is append-only and every entry is boxed, so the
		// pointee address is stable for as long as `self` lives; the returned
		// borrow is tied to `&self` and can never outlive the arena. The
		// `RefCell` guard only protects the outer `Vec`, which we are done
		// touching before this borrow escapes.
		unsafe { &*package }
	}

	/// The slot indices whose packages satisfy `predicate`, in publish order.
	fn matching(&self, predicate: impl Fn(&Package) -> bool) -> Vec<usize> {
		self.slots
			.borrow()
			.iter()
			.enumerate()
			.filter(|(_, package)| predicate(package))
			.map(|(index, _)| index)
			.collect()
	}
}

/// A registry catalog: a specialized selection of packages from the backing
/// registry, for a single ecosystem, under one capability plane. Typically held
/// by a client for the duration of a request.
pub struct Catalog<'brand, const L: Language, P: Plane> {
	brand: Id<'brand>,
	arena: Arena,
	_plane: PhantomData<P>,
}

impl<'brand, const L: Language, P: Plane> Catalog<'brand, L, P> {
	/// Open a catalog under a fresh brand (obtain the guard via
	/// `generativity::make_guard!`, which mints a unique lifetime per call site).
	pub fn open(guard: Guard<'brand>) -> Self {
		Self { brand: guard.into(), arena: Arena::default(), _plane: PhantomData }
	}

	/// Look up a package by name, returning a handle valid only against THIS
	/// catalog. The name is normalized under this catalog's ecosystem before
	/// comparison, so `serde_json` finds `serde-json`. The newest publication
	/// wins when several versions share the name. Threaded through the access
	/// layer.
	pub fn get(&self, name: &str) -> Option<PackageHandle<'brand>> {
		let wanted = PackageName::new(L, name).ok()?;
		self.arena
			.matching(|package| {
				package.coordinates.ecosystem() == L
					&& package.coordinates.name.canonical() == wanted.canonical()
			})
			.pop()
			.map(|index| PackageHandle { index, brand: self.brand })
	}

	/// Search the catalog (case-insensitive substring over canonical + display
	/// names), returning branded handles. Available on every plane.
	pub fn search(&self, query: &str) -> Vec<PackageHandle<'brand>> {
		let needle = query.to_ascii_lowercase();
		self.arena
			.matching(|package| {
				let name = &package.coordinates.name;
				package.coordinates.ecosystem() == L
					&& (name.canonical().contains(&needle)
						|| name.original().to_ascii_lowercase().contains(&needle))
			})
			.into_iter()
			.map(|index| PackageHandle { index, brand: self.brand })
			.collect()
	}

	/// Resolve a handle THIS catalog issued into the package it points at.
	pub fn resolve(&self, handle: PackageHandle<'brand>) -> &Package {
		// The brand proves the handle came from this catalog, so the index is
		// in range by construction.
		let _ = handle.brand;
		self.arena.get(handle.index)
	}
}

impl<'brand, const L: Language, P: Mutable> Catalog<'brand, L, P> {
	/// Publish a package into this ecosystem's catalog, returning a branded
	/// handle. Only reachable on a [`Mutable`] plane. Threaded through the access
	/// layer.
	pub fn publish(&self, package: Package) -> PackageHandle<'brand> {
		tracing::debug!(package = %package.id(), "package published into catalog");
		PackageHandle { index: self.arena.push(package), brand: self.brand }
	}
}
