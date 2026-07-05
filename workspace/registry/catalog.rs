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

use std::marker::PhantomData;

use generativity::{Guard, Id};
use heart::Language;

use crate::Package;

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

/// A registry catalog: a specialized selection of packages from the backing
/// registry, for a single ecosystem, under one capability plane. Typically held
/// by a client for the duration of a request.
pub struct Catalog<'brand, const L: Language, P: Plane> {
	brand: Id<'brand>,
	_plane: PhantomData<P>,
}

impl<'brand, const L: Language, P: Plane> Catalog<'brand, L, P> {
	/// Open a catalog under a fresh brand (obtain the guard via
	/// `generativity::make_guard!`, which mints a unique lifetime per call site).
	pub fn open(guard: Guard<'brand>) -> Self { Self { brand: guard.into(), _plane: PhantomData } }

	/// Look up a package by name, returning a handle valid only against THIS
	/// catalog. Threaded through the access layer.
	pub fn get(&self, name: &str) -> Option<PackageHandle<'brand>> {
		let _ = (self.brand, name);
		todo!("find the package, returning a branded handle")
	}

	/// Search the catalog, returning branded handles. Available on every plane.
	pub fn search(&self, query: &str) -> Vec<PackageHandle<'brand>> {
		let _ = (self.brand, query);
		todo!("search, returning branded handles")
	}

	/// Resolve a handle THIS catalog issued into the package it points at.
	pub fn resolve(&self, handle: PackageHandle<'brand>) -> &Package {
		let _ = (self.brand, handle.index, handle.brand);
		todo!("index straight into this catalog's arena")
	}
}

impl<'brand, const L: Language, P: Mutable> Catalog<'brand, L, P> {
	/// Publish a package into this ecosystem's catalog, returning a branded
	/// handle. Only reachable on a [`Mutable`] plane. Threaded through the access
	/// layer.
	pub fn publish(&self, package: Package) -> PackageHandle<'brand> {
		let _ = (self.brand, package);
		todo!("publish into this ecosystem's catalog, return branded handle")
	}
}
