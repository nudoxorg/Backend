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
    root_ref: Option<ContentHash>,
}

/// Hand-written: `ContentHasher` has no `Debug`, and dumping pending bytes
/// would flood output; identity + counts are what error contexts need.
impl std::fmt::Debug for BlobBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobBuilder")
            .field("package", &self.package)
            .field("files", &self.files.len())
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
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
            root_ref: None,
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
    pub fn push_file(
        &mut self,
        path: SmolStr,
        bytes: bytes::Bytes,
    ) -> Result<&mut Self, BlobError> {
        if self.files.iter().any(|entry| entry.path == path) {
            return Err(BlobError::DuplicateFilePathInBuilder);
        }
        let size = bytes.len() as u64;
        self.digest
            .update(&(path.len() as u64).to_le_bytes())
            .update(path.as_bytes());
        let hash = self.address(bytes);
        self.files.push(FileEntry { path, hash, size });
        Ok(self)
    }

    /// Attach the serialized IR (`ir::entry::Index`) section. `// runs on
    /// spawn_blocking` (serialization + hashing).
    pub fn set_ir(&mut self, ir_bytes: bytes::Bytes) -> Result<&mut Self, BlobError> {
        if self.ir_ref.is_some() {
            return Err(BlobError::IrSectionAttachedTwice);
        }
        self.ir_ref = Some(self.address(ir_bytes));
        Ok(self)
    }

    /// Attach the CST-free extracted-reference section. `// runs on
    /// spawn_blocking`.
    pub fn set_references(&mut self, refs: &ReferenceSet) -> Result<&mut Self, BlobError> {
        if self.references_ref.is_some() {
            return Err(BlobError::ReferencesSectionAttachedTwice);
        }
        let encoded = refs.encode()?;
        self.references_ref = Some(self.address(bytes::Bytes::from(encoded)));
        Ok(self)
    }

    /// Attach a [`ir::generation::GenerationRoot`] plus the entry payloads it
    /// names, **alongside** (not instead of) the legacy `ir_ref` section — P3
    /// of `docs/IR-STORAGE-PLAN.md` (dual-write). `// runs on spawn_blocking`
    /// (encoding the root + hashing every payload).
    ///
    /// The root itself is content-addressed the same way [`Self::set_ir`] and
    /// [`Self::set_references`] address their own sections: `self.address`
    /// hashes `root.encode()` and queues the `cas/` write, and that hash
    /// becomes `root_ref`.
    ///
    /// Each entry payload is **keyed by the caller-supplied** `ContentBlake3`
    /// — the same `entry_storage_hash` the root's `RootEntry::content` names —
    /// rather than by a freshly computed hash of its bytes, so the root's
    /// addressing and the object's storage key are the same value (the whole
    /// point of the root being a fault-in manifest, plan §2.3). Every such
    /// section still lands in the **same, single, verified `cas/{hash}`
    /// namespace** as every other section: `entry_storage_hash` is
    /// domain-separated over a narrower, position-free preimage than the
    /// stored bytes (it excludes `sym.source`/`sym.span` — see
    /// `ir::generation`'s module doc, "Moved is not changed"), so
    /// `crate::cas::Store` cannot verify these sections by recomputing
    /// `ContentHash::of_bytes` the way it does for `Content`-addressed ones —
    /// it instead decodes the payload back into an `Entry` and re-derives
    /// `entry_storage_hash` from that, the same way the addressing itself
    /// works. See `Store::verify_section_integrity` (`cas.rs`). This is why
    /// `payloads` is contractually **JSON-encoded semantic `Entry` values**
    /// (plan §5's "semantic, not wire" payload), not an arbitrary byte string:
    /// the verifier has to be able to decode it back.
    ///
    /// `ContentBlake3` (the `ir` crate's payload-identity hash) and
    /// `ContentHash` (this crate's `cas/` addressing hash) are distinct
    /// newtypes over the same 32-byte BLAKE3 digest space — never implicitly
    /// interchangeable — so each supplied `ContentBlake3` is converted
    /// explicitly via its raw bytes.
    pub fn set_generation_root(
        &mut self,
        root: &ir::generation::GenerationRoot,
        payloads: impl IntoIterator<Item = (ir::change::ContentBlake3, bytes::Bytes)>,
    ) -> Result<&mut Self, BlobError> {
        if self.root_ref.is_some() {
            return Err(BlobError::GenerationRootAttachedTwice);
        }
        self.root_ref = Some(self.address(bytes::Bytes::from(root.encode())));
        for (content, bytes) in payloads {
            // Explicit conversion: `ContentBlake3` -> `ContentHash`, same 32
            // raw bytes, distinct types (see doc comment above).
            let hash = ContentHash::from_bytes(*content.as_bytes());
            self.digest.update(hash.as_bytes());
            self.pending.push(PendingSection { hash, bytes });
        }
        Ok(self)
    }

    /// Finalize into a validated manifest plus the outstanding `cas/` writes.
    ///
    /// The [`Generation`] is the finalized canonical digest; the manifest's
    /// `files` are sorted for reproducibility. Fails if no files, IR, or
    /// references were supplied.
    pub fn finalize(self) -> Result<(BlobManifest, Vec<PendingSection>), BlobError> {
        let ir_ref = self.ir_ref.ok_or(BlobError::MissingIrSection)?;
        let references_ref = self
            .references_ref
            .ok_or(BlobError::MissingReferencesSection)?;

        let mut files = self.files;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let files = nonempty::NonEmpty::from_vec(files).ok_or(BlobError::ManifestHasNoFiles)?;

        let manifest = BlobManifest {
            package: self.package,
            files,
            ir_ref,
            references_ref,
            root_ref: self.root_ref,
            toolchain: self.toolchain,
        };
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
    pub fn provisional_generation(&self) -> ContentHash {
        self.digest.finalize()
    }

    /// Iterate the sanitized source files already staged in this builder as
    /// `(package-relative path, bytes)` pairs. Used by the compile phase to
    /// materialize a temporary tree for the language producers without a
    /// second archive pass.
    pub fn source_files(&self) -> impl Iterator<Item = (&SmolStr, &bytes::Bytes)> + '_ {
        self.files.iter().filter_map(|entry| {
            let section = self.pending.iter().find(|s| s.hash == entry.hash)?;
            Some((&entry.path, &section.bytes))
        })
    }

    /// The toolchain this builder was created with.
    pub fn toolchain(&self) -> &Toolchain {
        &self.toolchain
    }
}
