//! Deterministic id minting, salted with the graph instance.
//!
//! Wraps heart's deterministic derivers ([`PackageCoordinates::id`],
//! [`GlobalSymbolId::derive`]) behind a [`Minter`] that carries the
//! [`TerminusInstance`] token every symbol id is salted with, so a
//! [`GlobalSymbolId`] is recomputable offline against the same instance and two
//! instances of the same corpus never share ids. Package ids are
//! instance-independent (they fingerprint coordinates only) and delegate
//! straight through.

use heart::package::{EntryUri, GlobalSymbolId, PackageCoordinates, PackageId};

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
	/// to [`GlobalSymbolId::derive`] with this minter's instance token.
	pub fn symbol_id(&self, uri: &EntryUri) -> GlobalSymbolId {
		GlobalSymbolId::derive(self.instance.token(), uri)
	}

	/// The instance this minter salts with.
	pub fn instance(&self) -> &TerminusInstance { &self.instance }
}
