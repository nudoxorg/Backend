//! Deterministic global identification.
//!
//! Every library symbol gets a version-agnostic id derived (UUIDv5, fixed
//! namespace) from its terminus instance and entry URI, so any system can
//! recompute the same id offline — random ids are never minted for library
//! symbols. All derivation delegates to heart's canonical implementations
//! ([`SymbolId::derive`], [`heart::package::Coordinates::id`]); this module only
//! re-exports the vocabulary and pins the higher-level [`Minter`] as the
//! blessed entry point.

pub use heart::identity::{EntryUri, PackageId, SymbolId, namespace};
// PackageCoordinates now lives in heart::package; re-export under the same
// alias so existing `registry::identity::PackageCoordinates` references compile.
pub use heart::package::Coordinates as PackageCoordinates;
