//! Content-addressed package blobs.
//!
//! ## Why the old shape was unimplementable
//! The previous `Blob { concrete_syntax_tree: Tree, api_surface: Index,
//! source_text: tar::Archive<Cursor<Vec<u8>>> }` could not be built:
//! - a tree-sitter [`Tree`](arborium_tree_sitter::Tree) is non-`Send`,
//!   non-serializable C memory — it cannot cross a wire or land in a blob;
//! - one-tree-per-package is the wrong granularity (a package is many files);
//! - a `tar::Archive<_>` is a *reader*, not owned data — you cannot store it;
//! - holding the whole archive in a `Vec<u8>` is unbounded memory over
//!   untrusted input.
//!
//! ## The replacement: a manifest + content-addressed sections
//! A package snapshot is a [`BlobManifest`] — a small, serializable record —
//! plus a set of **individual content-addressed files**, each stored under its
//! own BLAKE3 digest in the object store's `cas/` space. This buys:
//! - **cross-version dedupe**: identical files across versions share one object;
//! - **ranged reads**: a caller can fetch one file without the whole package;
//! - **bounded memory**: files stream in and out; nothing is fully materialized;
//! - **integrity**: every read is verified against the hash it was keyed under.
//!
//! The CST is **not** stored — it is re-parsed on demand (tree-sitter is fast
//! and its output is non-portable). What we *do* persist from parsing is the set
//! of extracted [`ir::syntax::ResolvedReference`] spans, as plain serializable
//! data, so cross-references survive without the tree.

use heart::{
	PackageId, Toolchain,
	content::ContentHash,
};
use ir::syntax::ResolvedReference;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::error::BlobError;

pub mod creation;
pub mod emit;

pub use creation::BlobBuilder;

/// The serializable, content-addressed description of one package snapshot.
///
/// Small enough to store whole (in postgres and/or as a `cas/` object); the
/// bulk (source files, extracted references, IR) lives as separate objects this
/// manifest points at by hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[must_use = "a built manifest must be emitted (recorded + stored) or explicitly discarded"]
pub struct BlobManifest {
	/// The package this snapshot describes.
	pub package: PackageId,

	/// The source files, each addressed by its own content hash. Sorted by path
	/// for a canonical, reproducible manifest fingerprint. `NonEmpty` because a
	/// manifest with zero files is a meaningless blob — the invariant the builder
	/// enforces at `finalize` is now carried in the type.
	pub files: nonempty::NonEmpty<FileEntry>,

	/// The content hash of the serialized IR (`ir::entry::Index`) object this
	/// snapshot produced, stored separately in `cas/`.
	pub ir_ref: ContentHash,

	/// The content hash of the serialized extracted [`ResolvedReference`] spans
	/// (the CST-free cross-reference data), stored separately in `cas/`.
	pub references_ref: ContentHash,

	/// The toolchain this snapshot was produced against (provenance).
	pub toolchain: Toolchain,
}

impl BlobManifest {
	/// The canonical byte encoding fed to the generation hasher — the single
	/// definition of "the same snapshot". Length-prefixed and order-stable.
	pub fn identity_bytes(&self) -> Vec<u8> {
		todo!("fold sorted file (path, hash), ir_ref, references_ref, toolchain deterministically")
	}

	/// Verify the manifest is structurally well-formed (non-empty, sorted,
	/// no duplicate paths) before it is trusted.
	pub fn validate(&self) -> Result<(), BlobError> {
		todo!("assert files sorted+unique by path, generation matches identity_bytes")
	}
}

/// One source file within a package snapshot: its in-package path, its
/// content-addressed digest, and its size.
///
/// The bytes are **not** inline — they live at `cas/{hash}`. This is what makes
/// the manifest small and dedupe free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
	/// The file's path relative to the package root (already sanitized by
	/// [`crate::ingest`]).
	pub path: SmolStr,

	/// The BLAKE3 digest the file's bytes are stored under.
	pub hash: ContentHash,

	/// The file's uncompressed size in bytes.
	pub size: u64,
}

/// The CST-free cross-reference payload persisted alongside a snapshot.
///
/// We drop the tree-sitter tree but keep every [`ResolvedReference`] span it
/// yielded, grouped per file, so callers can resolve cross-references and
/// re-highlight without re-owning non-portable C memory.
///
/// JUDGMENT CALL: `ir::syntax::ResolvedReference` is *not* `serde`-tagged (and
/// this crate may not edit `ir`), so this payload is (de)serialized through a
/// bespoke codec at the storage boundary rather than a derived `Serialize` —
/// see [`ReferenceSet::encode`] / [`ReferenceSet::decode`]. `// runs on
/// spawn_blocking`.
#[derive(Debug, Clone)]
pub struct ReferenceSet {
	/// Per-file reference spans, keyed by the same in-package path as
	/// [`FileEntry::path`].
	pub by_file: Vec<FileReferences>,
}

impl ReferenceSet {
	/// Encode to the object-store section bytes. `// runs on spawn_blocking`.
	pub fn encode(&self) -> Result<Vec<u8>, BlobError> {
		todo!("bespoke length-prefixed encoding of (path, span, kind) triples")
	}

	/// Decode a section back into references. `// runs on spawn_blocking`.
	pub fn decode(_bytes: &[u8]) -> Result<Self, BlobError> {
		todo!("inverse of encode; validate spans + kinds")
	}
}

/// The extracted references for a single file.
#[derive(Debug, Clone)]
pub struct FileReferences {
	/// The in-package path these references were extracted from.
	pub path: SmolStr,

	/// The resolved reference spans, as plain data (no tree).
	pub references: Vec<ResolvedReference>,
}
