//! Deterministic global identification.
//!
//! Every library symbol gets a version-agnostic id derived (UUIDv5, fixed
//! namespace) from its terminus instance and entry URI, so any system can
//! recompute the same id offline — random ids are never minted for library
//! symbols. All derivation delegates to heart's canonical implementations
//! ([`SymbolId::derive`], [`PackageCoordinates::id`]); this module only
//! re-exports the vocabulary and pins the higher-level [`Minter`] as the
//! blessed entry point.

pub use heart::identity::{
	EntryUri, SymbolId, PackageCoordinates, PackageId, namespace,
};

pub use crate::metadata::guid::Minter;
