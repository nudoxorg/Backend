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

// Do note that the expectation is that for the individual registries, like
// Rust/Typescript/whatever, they'll add anything platform specific, I don't
// think the generics and patterns for gating on Language would be that useful
// there but we will see I suppose :)

/// Read-only registry — query, list, and search packages.
pub trait Registry<P: Plane> {
	/// The particular language that this is working within
	const LANGUAGE: Language;

	/// The package that the registry is in charge of holding/indexing over
	type Package;

	/// The failure mode for this registry
	type Error: std::error::Error + Send + Sync + 'static;

	/// An enum that represents various supported filters/conditions for search
	/// and listing
	type Condition;

	/// List the packages in this registry
	async fn list(&self, conditions: &[Self::Condition]);

	async fn get(&self, package: &Self::Package) -> Result<Option<Package>, Self::Error>;

	/// Search through the packages in this registry
	async fn search(&self, conditions: &[Self::Condition]);
}

/// Read-write registry — publish, rank, and modify packages.
pub trait RegistryWrite<P: Mutable>: Registry<P> {
	/// The payload type for publishing and modifying packages
	type Payload;

	/// The structure that defines how popularity should be decided
	type RankingPolicy;

	/// Publish a package to the registry.
	/// Returns the final, global package object, for you to syndicate out to the
	/// global store.
	async fn publish(&self, payload: Versioned<Self::Payload>) -> Result<GlobalPackage, Self::Error>;

	/// Alter the engine's ranking system for a registry
	async fn rank(&self, policy: Self::RankingPolicy) -> Result<(), Self::Error>;

	/// Change the state of an already-published package, name, description, yank
	/// status, etc. Not expected to be called often, but should also signify any
	/// of the dependent infra. Returns the newly minted global package object.
	async fn modify(
		&self,
		payload: Versioned<Self::Payload>,
		package: Id<Self::Package>,
	) -> Result<GlobalPackage, Self::Error>;
}

/// A store, usually a git repository, which stores multiple versions of a
/// desired piece of information. Agnostic to systems like branches, it's
/// expected that the object itself stores the branch it is on, and if you would
/// like to explore a different branch (or tag), mutate first. Different history
/// mechanisms (like tags) should likely implement historical themselves.
pub trait Historical {
	/// The object whose history is tracked.
	type Item;
	type Error: StoreError;

	/// The version before `current`, if any (`None` at the root).
	async fn past(&self, current: &Version) -> Result<Option<Version>, Self::Error>;

	/// The version after `current`, if any (`None` at HEAD).
	async fn future(&self, current: &Version) -> Result<Option<Version>, Self::Error>;

	/// The object as it was at `version`.
	async fn at(&self, version: &Version) -> Result<Versioned<Self::Item>, Self::Error>;
}
