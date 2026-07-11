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

use heart::{content::ContentHash, PackageId, Toolchain};
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

// ─────────────────────────────────────────────────────────────────────────────
// TWO DELIBERATELY DISTINCT HASHES — do not unify them.
//
// A `BlobManifest` is the centre of two separate hash computations, each with
// a different input and a different semantic role:
//
// ① GENERATION-STAMP hash  →  `BlobManifest::identity_bytes` (below)
//
//    INPUT:  a hand-rolled, length-prefixed, path-sorted encoding of the
//            manifest's *logical content*: the set of (path, file-hash) pairs,
//            ir_ref bytes, references_ref bytes, and postcard-encoded toolchain.
//    ROLE:   "Is this the same snapshot as before?"  Callers derive a stable
//            fingerprint from this buffer (e.g. `ContentHash::of_bytes(&ib)`)
//            to decide whether a package's content has changed.  The encoding
//            is deliberately bespoke so it can survive a serde-schema bump:
//            postcard-encoding the *whole* struct would silently change all
//            past stamps whenever a field is added, breaking freshness checks.
//
// ② SERIALIZED-BLOB CAS key  →  `BlobManifest::manifest_cas_key` (below)
//
//    INPUT:  `postcard::to_allocvec(manifest)` — the full postcard wire form.
//    ROLE:   "Where in the object store is this manifest stored?"  The CAS key
//            is the address under which the serialized manifest lives in
//            `cas/{hash}` and the value written to `ptr/{package-uuid}`.  It
//            must cover every field so the stored bytes round-trip exactly.
//
// WHY they must stay separate:
// • Unifying them (using the postcard hash as the generation stamp) would mean
//   adding an unrelated field to `BlobManifest` silently invalidates all
//   previously-fresh cache entries — a cache-busting event with no content
//   change.
// • Unifying them the other way (using the bespoke encoding as the CAS key)
//   would mean the CAS address no longer corresponds to the stored bytes,
//   breaking integrity checks on every `get_manifest` call.
//
// The pinning tests in `tests/blob_hash_pins.rs` assert specific golden values
// for both hashes over a fixed fixture.  If either encoding ever changes, those
// tests fail loudly before the change can ship.
// ─────────────────────────────────────────────────────────────────────────────

impl BlobManifest {
    /// The canonical byte encoding fed to the generation hasher — the single
    /// definition of "the same snapshot". Length-prefixed and order-stable.
    ///
    /// # Hash identity (generation stamp) — Hash ①
    ///
    /// This encoding is the input to a generation-stamp hash.  It is *not* the
    /// same as [`BlobManifest::manifest_cas_key`] (Hash ②).  See the module-level
    /// comment above for why they must remain distinct.
    pub fn identity_bytes(&self) -> Vec<u8> {
        let push = |bytes: &mut Vec<u8>, part: &[u8]| {
            bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
            bytes.extend_from_slice(part);
        };

        // Sort defensively so the fingerprint is order-stable even over a
        // manifest that has not passed [`BlobManifest::validate`] yet.
        let mut sorted: Vec<&FileEntry> = self.files.iter().collect();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));

        let mut bytes = Vec::new();
        for entry in sorted {
            push(&mut bytes, entry.path.as_bytes());
            bytes.extend_from_slice(entry.hash.as_bytes());
        }
        bytes.extend_from_slice(self.ir_ref.as_bytes());
        bytes.extend_from_slice(self.references_ref.as_bytes());
        let toolchain = postcard::to_allocvec(&self.toolchain)
            .expect("Toolchain is plain owned data and serializes infallibly with alloc");
        push(&mut bytes, &toolchain);
        bytes
    }

    /// The CAS key (BLAKE3 of the postcard-serialized manifest) under which this
    /// manifest is stored in `cas/` and to which `ptr/{package-id}` points.
    ///
    /// # Serialized-blob CAS key (blob address) — Hash ②
    ///
    /// This is **not** the same as hashing [`BlobManifest::identity_bytes`]
    /// (Hash ①).  See the module-level comment above for why they must remain
    /// distinct.
    ///
    /// The CAS key covers the full postcard wire encoding of the manifest.  Any
    /// change to the encoding (new field, removed field, changed type) produces a
    /// different CAS key, which is correct: the new bytes need a new address.
    ///
    /// This function is the single definition of "the manifest's blob address" so
    /// that [`crate::store::Store::put_manifest`] and any future reader share
    /// exactly one encoding path.  If you are tempted to inline
    /// `postcard::to_allocvec` + `ContentHash::of_bytes` at a call site, call
    /// this function instead.
    pub fn manifest_cas_key(&self) -> Result<ContentHash, BlobError> {
        let bytes = postcard::to_allocvec(self).map_err(BlobError::Codec)?;
        Ok(ContentHash::of_bytes(&bytes))
    }

    /// Verify the manifest is structurally well-formed (non-empty, sorted,
    /// no duplicate paths) before it is trusted.
    pub fn validate(&self) -> Result<(), BlobError> {
        // `NonEmpty` already carries the non-emptiness invariant in the type;
        // what remains is strict path ordering, which implies uniqueness.
        for pair in self.files.iter().collect::<Vec<_>>().windows(2) {
            match pair[0].path.cmp(&pair[1].path) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(BlobError::DuplicateFilePathInManifest);
                }
                std::cmp::Ordering::Greater => {
                    return Err(BlobError::ManifestFilesNotSorted);
                }
            }
        }
        Ok(())
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
        let wire = self
            .by_file
            .iter()
            .map(|file| {
                let references = file
                    .references
                    .iter()
                    .map(WireReference::from_reference)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(WireFile {
                    path: file.path.to_string(),
                    references,
                })
            })
            .collect::<Result<Vec<_>, BlobError>>()?;
        postcard::to_allocvec(&wire).map_err(BlobError::Codec)
    }

    /// Decode a section back into references. `// runs on spawn_blocking`.
    pub fn decode(bytes: &[u8]) -> Result<Self, BlobError> {
        let wire: Vec<WireFile> = postcard::from_bytes(bytes).map_err(BlobError::Codec)?;
        let by_file = wire
            .into_iter()
            .map(|file| {
                let references = file
                    .references
                    .into_iter()
                    .map(WireReference::into_reference)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FileReferences {
                    path: file.path.into(),
                    references,
                })
            })
            .collect::<Result<Vec<_>, BlobError>>()?;
        Ok(Self { by_file })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// The bespoke wire mirror of `ir::syntax::ResolvedReference` (which carries no
// serde impls of its own). Total in both directions: every field is either
// re-validated (span order, kind range, UTF-8 paths) or rejected.
// ─────────────────────────────────────────────────────────────────────────────

/// One file's worth of reference spans, on the wire.
#[derive(Serialize, Deserialize)]
struct WireFile {
    path: String,
    references: Vec<WireReference>,
}

/// One reference span, on the wire.
#[derive(Serialize, Deserialize)]
struct WireReference {
    target: WireTarget,
    span_start: u64,
    span_end: u64,
    kind: u8,
}

/// The wire form of `ir::entry::NudoxPath`.
#[derive(Serialize, Deserialize)]
enum WireTarget {
    External { path: String, dependency: String },
    Local(String),
}

impl WireReference {
    fn from_reference(reference: &ResolvedReference) -> Result<Self, BlobError> {
        Ok(Self {
            target: WireTarget::from_target(&reference.target)?,
            span_start: reference.span.start as u64,
            span_end: reference.span.end as u64,
            kind: kind_to_wire(&reference.kind),
        })
    }

    fn into_reference(self) -> Result<ResolvedReference, BlobError> {
        if self.span_start > self.span_end {
            return Err(BlobError::InvertedReferenceSpan);
        }
        Ok(ResolvedReference {
            target: self.target.into_target(),
            span: (self.span_start as usize)..(self.span_end as usize),
            kind: kind_from_wire(self.kind)?,
        })
    }
}

impl WireTarget {
    fn from_target(target: &ir::entry::NudoxPath) -> Result<Self, BlobError> {
        let utf8 = |path: &std::path::Path| {
            path.to_str().map(str::to_owned).ok_or(BlobError::NonUtf8ReferencePath)
        };
        Ok(match target {
            ir::entry::NudoxPath::External { path, dependency } => Self::External {
                path: utf8(path)?,
                dependency: dependency.clone(),
            },
            ir::entry::NudoxPath::Local(path) => Self::Local(utf8(path)?),
        })
    }

    fn into_target(self) -> ir::entry::NudoxPath {
        match self {
            Self::External { path, dependency } => ir::entry::NudoxPath::External {
                path: path.into(),
                dependency,
            },
            Self::Local(path) => ir::entry::NudoxPath::Local(path.into()),
        }
    }
}

/// The stable `ReferenceKind` wire discriminants. Kept explicit (not `as u8`
/// on an untagged enum) so reordering the ir enum can never silently reshuffle
/// stored data.
fn kind_to_wire(kind: &ir::syntax::ReferenceKind) -> u8 {
    use ir::syntax::ReferenceKind::*;
    match kind {
        FunctionCall => 0,
        MethodCall => 1,
        TypeReference => 2,
        VariableUse => 3,
        MacroInvocation => 4,
        FieldAccess => 5,
        Import => 6,
    }
}

/// Inverse of [`kind_to_wire`]; rejects out-of-range discriminants.
fn kind_from_wire(wire: u8) -> Result<ir::syntax::ReferenceKind, BlobError> {
    use ir::syntax::ReferenceKind::*;
    Ok(match wire {
        0 => FunctionCall,
        1 => MethodCall,
        2 => TypeReference,
        3 => VariableUse,
        4 => MacroInvocation,
        5 => FieldAccess,
        6 => Import,
        _ => return Err(BlobError::UnknownReferenceKindDiscriminant { wire }),
    })
}

/// The extracted references for a single file.
#[derive(Debug, Clone)]
pub struct FileReferences {
    /// The in-package path these references were extracted from.
    pub path: SmolStr,

    /// The resolved reference spans, as plain data (no tree).
    pub references: Vec<ResolvedReference>,
}
