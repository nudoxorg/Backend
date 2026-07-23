use crate::{PackageId, RegistryOrigin, ecosystem::Language, identity::derive};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::PackageName;
use crate::PackageVersion;

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

    /// The frozen-framing seed bytes for this coordinate's `(origin, name,
    /// version)` triple. Framing law lives in [`derive::frame_identity_parts`].
    pub fn identity_bytes(&self) -> Vec<u8> {
        let origin = self.origin.token();
        let version = self.version.canonical();
        derive::frame_identity_parts([
            origin.as_bytes(),
            self.name.canonical().as_bytes(),
            version.as_bytes(),
        ])
    }

    pub fn id(&self) -> PackageId {
        derive::package_id_from_parts([
            self.origin.token().as_bytes(),
            self.name.canonical().as_bytes(),
            self.version.canonical().as_bytes(),
        ])
    }
}

/// Raised when name and version disagree on ecosystem.
#[derive(Debug, Error)]
#[error("coordinate ecosystem mismatch: name is {name}, version is {version}")]
pub struct CoordinateError {
    pub name: Language,
    pub version: Language,
}
