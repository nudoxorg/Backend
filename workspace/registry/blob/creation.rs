//! Streaming assembly of a [`BlobManifest`](super::BlobManifest).
//!
//! A [`BlobBuilder`] accepts sanitized `(path, bytes)` pairs one at a time
//! (streamed from [`crate::ingest`]), content-addresses each file with BLAKE3,
//! and folds a canonical package-level [`Generation`] hash as it goes. It never
//! holds more than one file in memory at once, so it is safe against
//! decompression bombs the extractor has already bounded.
//!
//! Finalizing yields the manifest and the object-store writes still to perform;
//! the builder does no I/O itself — the caller decides transaction ordering.

use heart::{
	Toolchain,
	content::{ContentHash, ContentHasher, Generation},
	package::PackageId,
};
use smol_str::SmolStr;

use super::{BlobManifest, FileEntry, ReferenceSet};
use crate::error::BlobError;

/// A pending object-store write the builder emitted: the section key (a BLAKE3
/// hex) and the bytes to put there. The caller performs the actual idempotent
/// put against [`crate::Store`].
#[derive(Debug, Clone)]
pub struct PendingSection {
	/// The content hash the bytes will be stored under (`cas/{hash}`).
	pub hash: ContentHash,

	/// The bytes to persist. Owned so the write can outlive the builder.
	pub bytes: bytes::Bytes,
}

/// Streaming builder for a package snapshot.
///
/// Files are added in arrival order; the finalizer sorts them into the
/// canonical (path-ordered) manifest and computes the deterministic
/// [`Generation`]. The `ir` and `references` sections are attached before
/// finalizing.
#[must_use = "a builder that is never finalized produces no blob"]
pub struct BlobBuilder {
	package: PackageId,
	toolchain: Toolchain,
	files: Vec<FileEntry>,
	pending: Vec<PendingSection>,
	digest: ContentHasher,
	ir_ref: Option<ContentHash>,
	references_ref: Option<ContentHash>,
}

impl BlobBuilder {
	/// Begin assembling a snapshot for `package` produced under `toolchain`.
	pub fn new(package: PackageId, toolchain: Toolchain) -> Self {
		let _ = (package, toolchain);
		todo!("init with an empty ContentHasher + empty file/pending vectors")
	}

	/// Add one sanitized source file. Hashes the bytes (BLAKE3), records a
	/// [`FileEntry`], queues the `cas/` write, and folds the file into the
	/// running package digest. `// runs on spawn_blocking` (blake3 on large
	/// files).
	pub fn push_file(&mut self, path: SmolStr, bytes: bytes::Bytes) -> Result<&mut Self, BlobError> {
		let _ = (path, bytes, &mut self.files, &mut self.pending, &mut self.digest);
		todo!("blake3 the bytes, push FileEntry + PendingSection, update the canonical digest")
	}

	/// Attach the serialized IR (`ir::entry::Index`) section. `// runs on
	/// spawn_blocking` (serialization + hashing).
	pub fn set_ir(&mut self, ir_bytes: bytes::Bytes) -> Result<&mut Self, BlobError> {
		let _ = (ir_bytes, &mut self.ir_ref);
		todo!("hash the ir bytes, set ir_ref, queue the cas/ write, fold into digest")
	}

	/// Attach the CST-free extracted-reference section. `// runs on
	/// spawn_blocking`.
	pub fn set_references(&mut self, refs: &ReferenceSet) -> Result<&mut Self, BlobError> {
		let _ = (refs, &mut self.references_ref);
		todo!("encode refs, hash, set references_ref, queue the cas/ write, fold into digest")
	}

	/// Finalize into a validated manifest plus the outstanding `cas/` writes.
	///
	/// The [`Generation`] is the finalized canonical digest; the manifest's
	/// `files` are sorted for reproducibility. Fails if no files, IR, or
	/// references were supplied.
	pub fn finalize(self) -> Result<(BlobManifest, Vec<PendingSection>), BlobError> {
		let _ = (self.package, self.toolchain, self.files, self.pending, self.digest);
		todo!("sort files, finalize digest into Generation, require ir_ref+references_ref, validate")
	}

	/// The generation computed *so far* (for progress/debug); not the final
	/// committed value until [`finalize`](BlobBuilder::finalize).
	pub fn provisional_generation(&self) -> Generation { Generation(self.digest.finalize()) }
}
