//! Registry — the interface for all registry actions and methods: storing and
//! retrieving packages after computation, durable search, and the global index.

pub mod blob;
pub mod cordination;
pub mod identity;
pub mod index;
pub mod persist;
pub mod resolve;

use heart::{Id, Language, Versioned};

use crate::store::StoreError;

/// A registry that holds all of the packages and metadata for a particular language, including their code.
pub trait Registry {
	/// The particular language that this is oworking within
	const LANGUAGE: Language;

	/// The package that the registry is in charge of holding/indexing over
	type Package;

	// TODO: Find some way to avoid this annoying error repition

	/// The failure mode for this registry
    type Error: std::error::Error + Send + Sync + 'static;
}

/// A registry we're just pulling from, typically for providing search functionality
pub trait ReadRegistry: Registry {
	/// An enum that represents various supported filters/conditions for search and listing
	type Condition;

	const Language: Language;

	// TODO: Add some kind of mechanism for abstracting over how ranking should be handled? I wonder about splitting this into two traits, one for frontend/read-only and one for backend/mutability?

	/// List the packages in this registry
	async fn list(&self, conditions: &[Self::Condition]);

	async fn get(&self, package: &Self::Package) -> Result<Option<Package>>;

	/// Search through the packages in this registry
	async fn search(&self, conditions: &[Self::Condition]);
}

/// This is for registries we own and are actively attempting to write to/from
pub trait WriteRegistry: Registry {
	/// The package that the registry is in charge of holding/indexing over
	type Payload;

	/// The structure that defines how popularity should be decided
	// TODO: Should by closure, maybe? Idk what this is serializing against
	type RankingPolicy;

	/// Publish a package to the registry.
	/// Returns the final, global package object, for you to syndicate out to the global store
	async fn publish(
        &self,
        payload: Versioned<Self::Payload>,
    ) -> Result<GlobalPackage, Self::Error>;

	/// Alter the engines ranking system for a registry
	async fn rank(&self, policy: Self::RankingPolicy) -> Result<(), Self::Error>;

	/// Change the state of an already-published package, name, description, yank status, etc.
	/// Not expected to be called often, but should also signify any of the dependent infra
	/// Returns the newly minted global package object
	    async fn modify(
        &self,
        payload: Versioned<Self::Payload>,
        package: Id<Self::Package>,
    ) -> Result<GlobalPackage, Self::Error>;

	// Will likely sit on top of: https://lib.rs/crates/object_store
}

/// A store, usually a git repository, which stores multiple versions of a desired piece of information.
/// Agnostic to systems like branches, it's expected that the object itself stores the branch it is on, and if you would like to explore a different branch (or tag), mutate first. Different history mechanisms (like tags) should likely implement historical themselves.
pub trait Historical {
    /// The object whose history is tracked.
    type Item;
    type Error: StoreError;

    /// The version before `current`, if any (`None` at the root).
    async fn previous(&self, current: &Version) -> Result<Option<Version>, Self::Error>;

    /// The version after `current`, if any (`None` at HEAD).
    async fn next(&self, current: &Version) -> Result<Option<Version>, Self::Error>;

    /// The object as it was at `version`.
    async fn at(&self, version: &Version) -> Result<Versioned<Self::Item>, Self::Error>;
}
