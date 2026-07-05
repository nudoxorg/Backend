//! Deterministic id minting, salted with the graph instance.
//!
//! Wraps heart's deterministic derivers ([`PackageCoordinates::id`],
//! [`SymbolId::derive`]) behind a [`Minter`] that carries the
//! [`TerminusInstance`] token every symbol id is salted with, so a
//! [`SymbolId`] is recomputable offline against the same instance and two
//! instances of the same corpus never share ids. Package ids are
//! instance-independent (they fingerprint coordinates only) and delegate
//! straight through.

use heart::identity::{EntryUri, SymbolId, PackageId};
use crate::package::Coordinates as PackageCoordinates;

use crate::index::TerminusInstance;

/// The single id-minting choke point for the registry. Holds the instance token
/// so every symbol id it mints is salted consistently.
#[derive(Debug, Clone)]
pub struct Minter {
	instance: TerminusInstance,
}

impl Minter {
	/// Build a minter for a graph instance.
	pub fn new(instance: TerminusInstance) -> Self { Self { instance } }

	/// The deterministic package id for coordinates. Instance-independent —
	/// pure delegation to [`PackageCoordinates::id`].
	pub fn package_id(&self, coordinates: &PackageCoordinates) -> PackageId {
		coordinates.id()
	}

	/// The deterministic, instance-salted symbol id for an entry URI. Delegates
	/// to [`EntryUri::symbol_id`] with this minter's instance token.
	pub fn symbol_id(&self, uri: &EntryUri) -> SymbolId {
		uri.symbol_id(self.instance.token())
	}

	/// The instance this minter salts with.
	pub fn instance(&self) -> &TerminusInstance { &self.instance }
}
