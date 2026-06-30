//! A registry catalog handle that is *typed* in three dimensions at once:
//!
//! - brand, a special invariant lifetime, ensuring that the package returned
//!   can only ever be relevant against this particular catalog, no other.
//!
//! - language, so we can't incidentally mix up language, like TS and Rust.
//!
//! - capibility, so interfaces like server literally can't ever write back.

use std::marker::PhantomData;

use generativity::{Guard, Id};
use heart::LanguageTag;

use crate::Package;

mod sealed {
	pub trait Sealed {}
}

/// A read/write capability plane for a [`Catalog`] handle.
pub trait Plane: sealed::Sealed {}

/// A plane that additionally permits mutation. Only [`ReadWrite`] implements
/// it, so `publish` is unreachable from a [`ReadOnly`] handle.
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
/// catalog's invariant lifetime.
#[derive(Clone, Copy)]
pub struct PackageHandle<'brand> {
	index: usize,
	brand: Id<'brand>,
}

/// A registry catalog: branded `'brand`, typed to one language `L` and one
/// capability plane `P`.
pub struct Catalog<'brand, const L: LanguageTag, P: Plane> {
	brand:  Id<'brand>,
	_plane: PhantomData<P>,
}

impl<'brand, const L: LanguageTag, P: Plane> Catalog<'brand, L, P> {
	/// Open a catalog under a fresh brand (obtain the guard via
	/// `generativity::make_guard!`, which mints a unique lifetime per call site).
	pub fn open(guard: Guard<'brand>) -> Self { Self { brand: guard.into(), _plane: PhantomData } }

	/// Look up a package by name, returning a handle valid only against THIS
	/// catalog.
	pub fn get(&self, _name: &str) -> Option<PackageHandle<'brand>> {
		todo!("find the package and return a branded handle")
	}

	/// Search the catalog, returning branded handles. Available on every plane.
	pub fn search(&self, _query: &str) -> Vec<PackageHandle<'brand>> {
		todo!("search and return branded handles")
	}

	/// Resolve a handle THIS catalog issued.
	pub fn resolve(&self, handle: PackageHandle<'brand>) -> &Package {
		let _ = (self.brand, handle.index, handle.brand);
		todo!("index straight into this catalog's arena")
	}
}

impl<'brand, const L: LanguageTag, P: Mutable> Catalog<'brand, L, P> {
	/// Publish a package
	pub fn publish(&self, _package: Package) -> PackageHandle<'brand> {
		todo!("publish into this language's catalog and return a branded handle")
	}
}

// We can maybe do stuff like this?
// impl<'brand, P: Plane> Catalog<'brand, { LanguageTag::Rust }, P> {}
