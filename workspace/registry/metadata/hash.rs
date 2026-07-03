//! Package-level content hashing.
//!
//! The content-addressing primitives ([`ContentHash`], [`ContentHasher`],
//! [`Freshness`]) now live in [`heart::content`]; this module re-exports them so
//! registry callers have one import path, and adds the *package-level* canonical
//! hashing this crate is responsible for: folding a package's per-file digests,
//! in a stable sorted order, into a single [`ContentHash`].

pub use heart::content::{ContentHash, ContentHasher, Freshness};

use crate::blob::FileEntry;

/// Fold a package's sorted per-file digests into its canonical [`ContentHash`].
///
/// This is *the* definition of a package's content identity: sort the
/// [`FileEntry`]s by path (so file order in the archive is irrelevant),
/// length-prefix and feed each `(path, hash)` into a [`ContentHasher`], and
/// finalize. Deterministic and offline-recomputable. `// runs on spawn_blocking
/// for large file sets`.
pub fn package_generation(files: &[FileEntry]) -> ContentHash {
	let _ = files;
	todo!("sort by path, fold length-prefixed (path, file-hash) pairs, finalize into ContentHash")
}

/// Compare a package's recorded snapshot hash against a freshly-computed one to
/// decide whether it must be re-acquired and re-indexed. Thin wrapper over
/// [`Freshness::compare`] at the [`ContentHash`] granularity.
pub fn freshness(recorded: ContentHash, recomputed: ContentHash) -> Freshness {
	Freshness::compare(recorded, recomputed)
}
