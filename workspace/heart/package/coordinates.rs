use crate::{PackageId, RegistryOrigin, ecosystem::Language, identity::{Id, namespace}};
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

    pub fn identity_bytes(&self) -> Vec<u8> {
        // Length-prefixing every part makes the encoding injective — no
        // crafted origin/name pair can collide with a different tuple's byte
        // stream; the trailing NUL is a readability seam, not the mechanism.
        let origin = self.origin.token();
        let version = self.version.canonical();
        [origin.as_bytes(), self.name.canonical().as_bytes(), version.as_bytes()]
            .into_iter()
            .fold(Vec::new(), |mut bytes, part| {
                bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
                bytes.extend_from_slice(part);
                bytes.push(0);
                bytes
            })
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
