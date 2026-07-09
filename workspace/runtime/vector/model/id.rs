//! The stable, human-readable model identifier.
//!
//! Unlike the system's UUID [`heart::Id`]s, a `ModelId` is a *validated string* —
//! it is the qdrant collection/migration key and appears in operator-facing
//! config and logs, so readability matters more than uniformity. It simply can
//! never be blank.

use nutype::nutype;

/// A non-empty model identifier (e.g. `openai/text-embedding-3-small`). Trimmed
/// on construction; the empty string is unrepresentable.
#[nutype(
	sanitize(trim),
	validate(not_empty),
	derive(Debug, Clone, PartialEq, Eq, Hash, Display, AsRef, Serialize, Deserialize)
)]
pub struct ModelId(String);
