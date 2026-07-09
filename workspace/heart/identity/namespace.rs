//! UUIDv5 namespaces for deterministic id derivation. Each identifiable kind gets
//! its own namespace so, e.g., a package and a symbol deriving from the same bytes
//! still land on distinct ids.
use uuid::Uuid;

/// Namespace for [`super::PackageId`] (derived from [`super::PackageCoordinates`]).
pub const PACKAGE: Uuid = Uuid::from_u128(0x6e75_646f_785f_706b_675f_6e73_0000_0001);

/// Namespace for [`super::SymbolId`] (derived from an [`super::EntryUri`]).
pub const SYMBOL: Uuid = Uuid::from_u128(0x6e75_646f_785f_7379_6d5f_6e73_0000_0002);
