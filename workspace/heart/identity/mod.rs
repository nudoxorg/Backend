//! Identity — the single home for *how we name things*.
//!
//! One constructor, [`Id<T>`], underlies every identifier; domain ids are just
//! branded aliases ([`PackageId`] = `Id<Package>`, [`SymbolId`] = `Id<Symbol>`)
//! with their derivation exposed as constructors ([`PackageCoordinates::id`],
//! [`EntryUri::symbol_id`]) rather than as per-type newtypes.

pub mod id;
pub mod namespace;
pub mod package;
pub mod symbol;

pub use id::Id;
pub use package::{
	CoordinateError, NameError, Package, PackageCoordinates, PackageId, PackageName, PackageVersion,
	RegistryOrigin, VersionError,
};
pub use symbol::{EntryUri, SymbolId};
