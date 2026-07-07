use serde::{Deserialize, Serialize};

use crate::content::ContentHash;

/// A pagination position over a stream of `K`-keyed results.
///
/// `K` is the keyset key (e.g. `(Score, SymbolId)`), which must
/// round-trip through `postcard` for the opaque token encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor<K> {
	/// The last key returned; the next page resumes strictly after it.
	pub after: K,

	/// The index snapshot (content hash) this cursor is anchored to. A page served
	/// against a newer snapshot is flagged so callers can choose to restart cleanly.
	pub snapshot: ContentHash,
}

impl<K> Cursor<K>
where
	K: Serialize + for<'de> Deserialize<'de>,
{
	/// Create a cursor anchored at `after` within `snapshot`.
	pub fn new(after: K, snapshot: ContentHash) -> Self { Self { after, snapshot } }

	/// Encode to an opaque base64url token for the wire.
	pub fn encode(&self) -> String {
		let bytes = postcard::to_allocvec(self)
			.expect("cursor keys are plain data and serialize infallibly");
		data_encoding::BASE64URL_NOPAD.encode(&bytes)
	}

	/// Decode an opaque token back into a cursor.
	pub fn decode(token: &str) -> Result<Self, CursorError> {
		let bytes = data_encoding::BASE64URL_NOPAD
			.decode(token.as_bytes())
			.map_err(|_| CursorError::Encoding)?;
		postcard::from_bytes(&bytes).map_err(|_| CursorError::Payload)
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
