pub mod coordinates;
pub mod resolution;
pub mod version;

pub use coordinates::{CoordinateError, Coordinates};
pub use resolution::State;
pub use version::PackageVersion;

use heart::{NameError, RegistryOrigin, ecosystem::Language};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A validated, ecosystem-normalized package name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageName {
    /// The ecosystem this name belongs to (part of its identity).
    pub ecosystem: Language,
    /// Our sanitized name.
    canonical: SmolStr,
    /// Their display name.
    original: SmolStr,
}

impl PackageName {
    pub fn new(ecosystem: Language, raw: impl Into<String>) -> Result<Self, NameError> {
        let _ = (ecosystem, raw);
        todo!("normalize per ecosystem, validate charset + length, retain original")
    }

    pub fn canonical(&self) -> &str { &self.canonical }
    pub fn original(&self) -> &str { &self.original }
    pub const fn ecosystem(&self) -> Language { self.ecosystem }
}
