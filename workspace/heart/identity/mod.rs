pub mod derive;
pub mod id;
pub mod namespace;
pub mod package;
pub mod symbol;

pub use derive::{frame_identity_parts, package_id_from_parts};
pub use id::Id;
pub use package::{
    CargoVersionError, NameError, NpmVersionError, Package, PackageId, PackageVersion,
    PythonVersionError, RegistryOrigin, VersionError,
};
pub use symbol::{EntryUri, SymbolId};

/// Re-export so `heart::identity::PackageCoordinates` resolves (mirrors the
/// alias used in `registry::identity`).
pub use crate::package::Coordinates as PackageCoordinates;
