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
	PackageId, Toolchain,
	content::{ContentHash, ContentHasher},
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
		Self {
			package,
			toolchain,
			files: Vec::new(),
			pending: Vec::new(),
			digest: ContentHash::builder(),
			ir_ref: None,
			references_ref: None,
		}
	}

	/// Content-address one section: hash the bytes, queue the `cas/` write, and
	/// fold the section into the running (arrival-order, provisional) digest.
	fn address(&mut self, bytes: bytes::Bytes) -> ContentHash {
		let hash = ContentHash::of_bytes(&bytes);
		self.digest.update(hash.as_bytes());
		self.pending.push(PendingSection { hash, bytes });
		hash
	}

	/// Add one sanitized source file. Hashes the bytes (BLAKE3), records a
	/// [`FileEntry`], queues the `cas/` write, and folds the file into the
	/// running package digest. `// runs on spawn_blocking` (blake3 on large
	/// files).
	pub fn push_file(&mut self, path: SmolStr, bytes: bytes::Bytes) -> Result<&mut Self, BlobError> {
		if self.files.iter().any(|entry| entry.path == path) {
			return Err(BlobError::Malformed("duplicate file path pushed into builder"));
		}
		let size = bytes.len() as u64;
		self.digest.update(&(path.len() as u64).to_le_bytes()).update(path.as_bytes());
		let hash = self.address(bytes);
		self.files.push(FileEntry { path, hash, size });
		Ok(self)
	}

	/// Attach the serialized IR (`ir::entry::Index`) section. `// runs on
	/// spawn_blocking` (serialization + hashing).
	pub fn set_ir(&mut self, ir_bytes: bytes::Bytes) -> Result<&mut Self, BlobError> {
		if self.ir_ref.is_some() {
			return Err(BlobError::Malformed("ir section attached twice"));
		}
		self.ir_ref = Some(self.address(ir_bytes));
		Ok(self)
	}

	/// Attach the CST-free extracted-reference section. `// runs on
	/// spawn_blocking`.
	pub fn set_references(&mut self, refs: &ReferenceSet) -> Result<&mut Self, BlobError> {
		if self.references_ref.is_some() {
			return Err(BlobError::Malformed("references section attached twice"));
		}
		let encoded = refs.encode()?;
		self.references_ref = Some(self.address(bytes::Bytes::from(encoded)));
		Ok(self)
	}

	/// Finalize into a validated manifest plus the outstanding `cas/` writes.
	///
	/// The [`Generation`] is the finalized canonical digest; the manifest's
	/// `files` are sorted for reproducibility. Fails if no files, IR, or
	/// references were supplied.
	pub fn finalize(self) -> Result<(BlobManifest, Vec<PendingSection>), BlobError> {
		let ir_ref = self.ir_ref.ok_or(BlobError::Malformed("manifest is missing its ir section"))?;
		let references_ref = self
			.references_ref
			.ok_or(BlobError::Malformed("manifest is missing its references section"))?;

		let mut files = self.files;
		files.sort_by(|a, b| a.path.cmp(&b.path));
		let files = nonempty::NonEmpty::from_vec(files)
			.ok_or(BlobError::Malformed("manifest has no files"))?;

		let manifest =
			BlobManifest { package: self.package, files, ir_ref, references_ref, toolchain: self.toolchain };
		manifest.validate()?;
		tracing::debug!(
			package = %manifest.package,
			files = manifest.files.len(),
			sections = self.pending.len(),
			"blob manifest finalized"
		);
		Ok((manifest, self.pending))
	}

	/// The snapshot hash computed *so far* (for progress/debug); not the final
	/// committed value until [`finalize`](BlobBuilder::finalize).
	pub fn provisional_generation(&self) -> ContentHash { self.digest.finalize() }
}
