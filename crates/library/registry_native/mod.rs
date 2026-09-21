//! Bounded, versioned native registry metadata shared by every product surface.
//!
//! The engine owns acquisition and journal admission, while this value owns
//! the vocabulary exposed to local-service, JSON, MCP, CLI, and desktop. A
//! native feed may omit a fact; omission is represented by a typed observation
//! instead of an empty string or a fabricated value.

mod admission;
mod codec;
mod model;

#[cfg(test)]
mod tests;

pub use codec::RegistryNativeMetadataCodecError;
pub use model::*;

/// Current native metadata DTO schema.
pub const REGISTRY_NATIVE_METADATA_VERSION: u16 = 1;
/// Maximum number of rows retained in one native metadata collection.
pub const MAX_REGISTRY_NATIVE_ROWS: usize = 4_096;
/// Maximum text extent retained in one metadata field.
pub const MAX_REGISTRY_NATIVE_TEXT_BYTES: usize = 4_096;
/// Maximum serialized metadata extent admitted on a product surface.
pub const MAX_REGISTRY_NATIVE_METADATA_BYTES: usize = 2 * 1024 * 1024;
