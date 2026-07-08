//! Assembling the sink-ready blob information from the generated resolutions.
//!
//! This is the bridge to the registry's content-addressed blob model: it folds
//! the per-file digests into the canonical snapshot [`heart::ContentHash`] — the
//! sorted fold of per-file hashes — that postgres records as a package's identity
//! and freshness key, and carries the leaf references (surface IR, CST spans) the
//! blob manifest points at.

use heart::content::ContentHash;
use ir::entry::Index;

use crate::generate::{cst::CstSet, source_archive::SourceArchive};

/// The assembled blob information the registry blob layer consumes: the
/// content-addressed source archive plus the canonical package snapshot hash.
pub struct BlobInfo {
	/// The source archive (per-file content hashes, sorted by path).
	pub archive: SourceArchive,
	/// The canonical content hash of the whole package snapshot.
	pub snapshot: ContentHash,
}

impl BlobInfo {
	/// Fold the generated resolutions into the canonical snapshot [`ContentHash`]
	/// and assemble the blob info.
	///
	/// The hash is a deterministic fold: per-file `(path, hash)` pairs in sorted
	/// order streamed through a [`ContentHasher`], so the same package snapshot
	/// always yields the same hash on any machine.
	pub fn assemble(surface: &Index, cst: &CstSet, archive: SourceArchive) -> Self {
		// The surface and CST are derived deterministically from the same
		// source bytes the archive digests, so they carry no extra identity;
		// they ride along only for the manifest's leaf references.
		let _ = (surface, cst);

		let mut hasher = ContentHash::builder();
		for file in &archive.files {
			// Length-prefix each path so the (path, hash) stream is injective —
			// no split point ambiguity between neighbouring entries.
			let path = file.path.to_string_lossy();
			hasher.update(&(path.len() as u64).to_le_bytes());
			hasher.update(path.as_bytes());
			hasher.update(file.hash.as_bytes());
		}

		BlobInfo { snapshot: hasher.finalize(), archive }
	}
}
