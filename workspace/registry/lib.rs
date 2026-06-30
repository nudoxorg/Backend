//! Registry — the interface for all registry actions and methods: storing and
//! retrieving packages after computation, durable search, and the global index.
#![feature(adt_const_params)]

pub mod blob;
pub mod catalog;
pub mod cordination;
pub mod health;
pub mod identity;
pub mod index;
pub mod metadata;
pub mod persist;
pub mod queue;
pub mod reproducibility;
pub mod resolve;
pub mod search;
pub mod store;

pub use catalog::{Mutable, Plane, ReadOnly, ReadWrite};
use heart::{Guid, Id, Language, StoreError, Versioned};
use semver::Version;
pub use store::Store;

/// A package as it lives in a single registry, before global syndication.
pub struct Package {
	/// The registry name of the package (e.g. `axum`, `@types/node`).
	pub name: String,

	/// The exact version this record describes.
	pub version: Version,

	/// The language/ecosystem this package belongs to.
	pub language: Language,
}

/// The final, globally-syndicated package object handed back to the global
/// store.
pub struct GlobalPackage {
	/// The canonical, version-agnostic global identity of the package.
	pub id: Guid,

	/// The per-registry record this global package was minted from.
	pub package: Package,
}

