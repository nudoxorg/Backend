//! The hash of a package's code / treesitter representation.
//!
//! Postgres records this next to a package's canonical GUID
//! (see [`guid`](super::guid)) for **one purpose**: detecting re-parses. When a
//! freshly-computed hash differs from the recorded one, the library is pulled
//! down again and re-indexed.

/// A content hash of a package's parsed representation, used only for
/// freshness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
	/// Wrap a freshly-computed (or postgres-recorded) representation hash.
	pub fn new(hash: [u8; 32]) -> Self { Self(hash) }
}

/// Whether a package's stored representation still matches its source.
pub enum Freshness {
	/// The recorded hash matches a freshly-computed one.
	Fresh,

	/// The representation changed.
	Stale,
}

/// Decide whether a package needs re-parsing by comparing its recorded hash to
/// a freshly-computed one.
pub fn freshness(recorded: ContentHash, recomputed: ContentHash) -> Freshness {
	if recorded == recomputed { Freshness::Fresh } else { Freshness::Stale }
}
