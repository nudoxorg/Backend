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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RepoId(String);

impl RepoId {
	/// Return the inner string as a `&str`.
	pub fn as_str(&self) -> &str { &self.0 }

	/// Consume `self` and return the inner `String`.
	pub fn into_string(self) -> String { self.0 }
}

impl From<String> for RepoId {
	fn from(s: String) -> Self { Self(s) }
}

impl From<&str> for RepoId {
	fn from(s: &str) -> Self { Self(s.to_owned()) }
}

/// Reference to a blob in blob storage. Opaque to consumers.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BlobRef(String);

impl BlobRef {
	/// Return the inner string as a `&str`.
	pub fn as_str(&self) -> &str { &self.0 }

	/// Consume `self` and return the inner `String`.
	pub fn into_string(self) -> String { self.0 }
}

impl From<String> for BlobRef {
	fn from(s: String) -> Self { Self(s) }
}

impl From<&str> for BlobRef {
	fn from(s: &str) -> Self { Self(s.to_owned()) }
}

impl fmt::Display for GlobalSymbolId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

impl fmt::Display for OccurrenceId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) }
}

impl fmt::Display for RepoId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl fmt::Display for BlobRef {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
