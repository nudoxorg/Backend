//! Content addressing and freshness.
//!
//! A [`ContentHash`] is a BLAKE3 digest of a package's canonical parsed
//! representation. It serves three roles the rest of the system leans on:
//! - **freshness**: recorded-vs-recomputed tells us when to re-parse;
//! - **content addressing**: blob storage keys on the hash, giving idempotent
//!   puts, cross-version dedupe, and integrity verification for free;
//! - **generation stamping**: every derived record (vector, graph doc, text
//!   doc) is tagged with the [`Generation`] it was produced from, so a query
//!   can detect — never silently mix — cross-store version skew.

use serde::{Deserialize, Serialize};

/// A 256-bit BLAKE3 content digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
	/// Wrap a raw 32-byte digest (e.g. read back from postgres).
	pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }

	/// The raw digest bytes.
	pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }

	/// Hash a contiguous byte buffer.
	pub fn of_bytes(bytes: &[u8]) -> Self { Self(*blake3::hash(bytes).as_bytes()) }

	/// Lowercase hex, for keys and display.
	pub fn to_hex(&self) -> String { todo!("blake3 hex encoding") }

	/// Parse from lowercase hex.
	pub fn from_hex(hex: &str) -> Option<Self> {
		let _ = hex;
		todo!("decode 64 hex chars into [u8; 32]")
	}

	/// Begin an incremental, streaming hash — the canonical way to fingerprint a
	/// package without materializing it, feeding per-file digests in a stable
	/// (sorted) order.
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
	/// The representation changed; the package must be re-acquired and re-indexed.
	Stale,
}

impl Freshness {
	/// Compare a recorded hash against a freshly-computed one.
	pub fn compare(recorded: ContentHash, recomputed: ContentHash) -> Self {
		if recorded == recomputed { Freshness::Fresh } else { Freshness::Stale }
	}
}

/// The generation a derived record was produced from — the content hash of the
/// package snapshot it reflects. Stamped onto every vector/graph/text record so
/// cross-store skew is observable, never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Generation(pub ContentHash);
