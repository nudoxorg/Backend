//! Opaque, keyset-based pagination cursors.
//!
//! Offset pagination over an eventually-consistent index duplicates and skips
//! results as the index shifts between pages, and deep offsets are pathological
//! in both tantivy and qdrant. A [`Cursor`] instead encodes the *keyset*
//! position plus the [`Generation`] the page was served from, so the next page
//! resumes from a stable point and can detect when it is reading across a
//! regenerated index.
//!
//! The wire form is an opaque base64url token; callers never construct or
//! inspect its internals.

use serde::{Deserialize, Serialize};

use crate::content::Generation;

/// A pagination position over a stream of `K`-keyed results.
///
/// `K` is the keyset key (e.g. `(Score, GlobalSymbolId)`), which must round-trip
/// through `postcard` for the opaque token encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor<K> {
	/// The last key returned; the next page resumes strictly after it.
	pub after: K,
	/// The index generation this cursor is anchored to. A page served against a
	/// newer generation is flagged so callers can choose to restart cleanly.
	pub generation: Generation,
}

impl<K> Cursor<K>
where
	K: Serialize + for<'de> Deserialize<'de>,
{
	/// Create a cursor anchored at `after` within `generation`.
	pub fn new(after: K, generation: Generation) -> Self { Self { after, generation } }

	/// Encode to an opaque base64url token for the wire.
	pub fn encode(&self) -> String { todo!("postcard-serialize then base64url (no padding)") }

	/// Decode an opaque token back into a cursor.
	pub fn decode(token: &str) -> Result<Self, CursorError> {
		let _ = token;
		todo!("base64url-decode then postcard-deserialize")
	}
}

/// Why an opaque cursor token could not be decoded.
#[derive(Debug, thiserror::Error)]
pub enum CursorError {
	/// The token was not valid base64url.
	#[error("malformed cursor encoding")]
	Encoding,
	/// The decoded bytes did not match the expected keyset shape.
	#[error("malformed cursor payload")]
	Payload,
}
