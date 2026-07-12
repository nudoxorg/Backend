//! Content addressing and freshness.

use serde::{Deserialize, Serialize};

/// The content hash which serves three roles:
/// 1. Ensuring that package freshness hasn't changed.
/// 2. Dedupe on the content
/// 3. type marking anything that depends on it
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
	/// Wrap a raw 32-byte digest (e.g. read back from postgres).
	pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }

	/// The raw digest bytes.
	pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }

	/// Hash a contiguous byte buffer.
	pub fn of_bytes(bytes: &[u8]) -> Self { Self(*blake3::hash(bytes).as_bytes()) }

	pub fn builder() -> ContentHasher { ContentHasher(blake3::Hasher::new()) }
}

/// An incremental hasher for building a [`ContentHash`] from a stream of parts
/// without holding the whole package in memory.
pub struct ContentHasher(blake3::Hasher);

impl ContentHasher {
	/// Fold another chunk into the digest.
	pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
		self.0.update(bytes);
		self
	}

	/// Finalize into a [`ContentHash`].
	pub fn finalize(&self) -> ContentHash { ContentHash(*self.0.finalize().as_bytes()) }
}

/// Whether a package's stored representation still matches its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
	/// The recorded hash matches a freshly-computed one; no work needed.
	Fresh,

	/// The representation changed; the package must be re-acquired and
	/// re-indexed.
	Stale,
}

impl Freshness {
	/// Compare a recorded hash against a freshly-computed one.
	pub fn compare(recorded: ContentHash, recomputed: ContentHash) -> Self {
		if recorded == recomputed { Freshness::Fresh } else { Freshness::Stale }
	}
}
