use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Version-agnostic identifier for a symbol across all parsed libraries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlobalSymbolId(pub Uuid);

/// Uniquely identifies one occurrence of a symbol in one location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OccurrenceId(pub Uuid);

/// Identifies a repository we have indexed or are indexing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RepoId(pub String);

/// Reference to a blob in blob storage. Opaque to consumers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobRef(pub String);

impl fmt::Display for GlobalSymbolId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

impl fmt::Display for OccurrenceId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

impl fmt::Display for RepoId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

impl fmt::Display for BlobRef {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}
