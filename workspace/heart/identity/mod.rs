pub mod id;
pub mod namespace;
pub mod package;
pub mod symbol;

pub use id::Id;
pub use package::{
    CargoVersionError, NameError, NpmVersionError, Package, PackageId, PackageVersion,
    PythonVersionError, RegistryOrigin, VersionError,
};
pub use symbol::{EntryUri, SymbolId};

/// Re-export so `heart::identity::PackageCoordinates` resolves (mirrors the
/// alias used in `registry::identity`).
pub use crate::package::Coordinates as PackageCoordinates;
