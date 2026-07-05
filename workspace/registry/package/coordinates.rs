use heart::{PackageId, RegistryOrigin, ecosystem::Language, identity::{Id, namespace}};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{PackageName, PackageVersion};

/// The address of a package.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Coordinates {
    pub origin: RegistryOrigin,
    pub name: PackageName,
    pub version: PackageVersion,
}

impl Coordinates {
    pub const fn ecosystem(&self) -> Language {
        self.name.ecosystem
    }

    pub fn identity_bytes(&self) -> Vec<u8> {
        todo!("origin.token \\0 name.canonical \\0 version.canonical, length-prefixed")
    }

    pub fn id(&self) -> PackageId {
        Id::from_name(&namespace::PACKAGE, &self.identity_bytes())
    }
}

/// Raised when name and version disagree on ecosystem.
#[derive(Debug, Error)]
#[error("coordinate ecosystem mismatch: name is {name}, version is {version}")]
pub struct CoordinateError {
    pub name: Language,
    pub version: Language,
}
