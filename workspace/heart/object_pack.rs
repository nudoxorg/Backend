//! ObjectPack — the one container family for source trees, goldens, and
//! compile stages (INDEX-PLAN ID-16, §6).
//!
//! Layout (`NDPK` v1): header, zstd-framed members (optionally Bao-capable),
//! then a TOC mapping sorted member keys to byte ranges + BLAKE3 digests, so
//! a single snippet is one range get — never a whole-tree load.
//!
//! This module holds the *vocabulary*; the builder/reader/store engines live
//! in the `index` crate (local store) and `nudox-sync` (iroh transfer).

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::content::ContentHash;

/// Identity of a sealed pack: the BLAKE3 root over TOC + policy bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectPackId(pub ContentHash);

/// A `/`-separated path relative to the pack's tree root; never absolute,
/// never containing `.` or `..` segments.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RelativePath(pub SmolStr);

/// OCI image digest (`sha256:<hex>`) identifying a toolchain image.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ImageDigest(pub SmolStr);

/// What a member of a pack *is*. The TOC is sorted by this key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemberKey {
    /// One file of a source tree.
    Source { path: RelativePath },
    /// A warm-boot VM checkpoint, valid only for its exact image + producer.
    Golden { image_digest: ImageDigest, producer_version: u32 },
    /// An intermediate compile stage, keyed by job.
    Stage { job_key_hex: SmolStr, stage: SmolStr },
    /// Small named metadata (provenance notes, blob-hash sidecars, …).
    Meta { name: SmolStr },
}

/// Byte offset of a member's chunk region from the start of the pack file.
///
/// A newtype (never a bare `u64` in the public API, per the workspace style
/// law) so an offset can never be confused with a length or a byte-range bound.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct PackOffset(pub u64);

impl PackOffset {
    /// The raw offset as a `u64` for seeking / slicing.
    pub const fn get(self) -> u64 { self.0 }
}

/// A count of bytes (compressed or uncompressed) — distinct from an offset.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct ByteLength(pub u64);

impl ByteLength {
    /// The raw length as a `u64`.
    pub const fn get(self) -> u64 { self.0 }
}

/// The uncompressed chunk size at which large [`MemberKey::Source`] members are
/// split into independently-decompressable zstd frames (INDEX-PLAN §6.2).
///
/// Fixed at 128 KiB. Larger chunks compress marginally better but force a
/// range get to decompress more slack; smaller chunks bloat the chunk table and
/// waste zstd frame overhead. 128 KiB balances snippet-window locality (a
/// single function body almost always lives in one chunk) against overhead.
/// It is a **format constant**: changing it changes every pack's bytes and id.
pub const SOURCE_CHUNK_SIZE_BYTES: u64 = 128 * 1024;

/// The uncompressed-size threshold at or above which a member should carry a
/// Bao outboard for verified range streaming over iroh (INDEX-PLAN §6.2).
pub const BAO_OUTBOARD_THRESHOLD_BYTES: u64 = 1024 * 1024;

/// One independently-decompressable zstd frame within a member.
///
/// Each chunk covers exactly [`SOURCE_CHUNK_SIZE_BYTES`] uncompressed bytes,
/// except the final chunk of a member which covers the remainder. A range get
/// decompresses only the chunks that cover the requested byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntry {
    /// Byte offset of this chunk's zstd frame from the start of the pack file.
    pub frame_offset: PackOffset,
    /// Length in bytes of this chunk's compressed zstd frame.
    pub compressed_length: ByteLength,
    /// Length in bytes of this chunk once decompressed (≤ chunk size).
    pub uncompressed_length: ByteLength,
}

/// One TOC row: where a member lives and what it hashes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRecord {
    pub key: MemberKey,
    /// Byte offset of the member's first zstd frame from pack start.
    ///
    /// Redundant with `chunks[0].frame_offset` but retained for cheap locate
    /// and for TOC readers that ignore chunking (N-1 compatibility).
    pub offset: PackOffset,
    pub compressed_length: ByteLength,
    pub uncompressed_length: ByteLength,
    /// BLAKE3 of the uncompressed member bytes (whole member, all chunks
    /// concatenated in order).
    pub content: ContentHash,
    /// The member's chunk table, in ascending uncompressed-offset order. A
    /// small member is a single chunk; a large member (of any kind) is split
    /// at [`SOURCE_CHUNK_SIZE_BYTES`] boundaries.
    pub chunks: Vec<ChunkEntry>,
    /// Whether a Bao outboard exists for verified range streaming (members
    /// at or above [`BAO_OUTBOARD_THRESHOLD_BYTES`], INDEX-PLAN §6.2).
    pub bao_outboard: bool,
}

// ---------------------------------------------------------------------------
// Source acquisition policy (INDEX-PLAN §6.3). These types describe *where a
// materialised source tree came from*; they are stored by the catalog and
// referenced by an [`ObjectPackId`], not embedded in the pack container.
// ---------------------------------------------------------------------------

/// Stable identifier of a package registry (e.g. `crates.io`, `npm`, `nuget`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RegistryId(pub SmolStr);

/// A canonical, comparison-ready version string (e.g. `0.7.9`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VersionCanonical(pub SmolStr);

/// An upstream source-repository URL.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RepoUrl(pub SmolStr);

/// A VCS revision (tag or commit) used to materialise a source tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GitRev(pub SmolStr);

/// How the source tree behind a published version was acquired (§6.3).
///
/// Both variants yield the **same information quality** as the registry
/// artifact (checksum + file set); neither retains the vendor container format
/// as the working representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceAcquisition {
    /// Preferred: clone/fetch the upstream repository at a specific revision.
    Git {
        /// Upstream repository URL.
        url: RepoUrl,
        /// The tag or commit that backs this published version.
        rev: GitRev,
        /// Registry checksum of the published package, kept for cross-check
        /// even when source comes from git.
        registry_checksum: Option<ContentHash>,
    },
    /// Reconstruct: unpack the registry artifact (`.crate` / `.nupkg` / npm
    /// tarball / maven sources) once, then ObjectPack it. Quality equals the
    /// registry artifact; format equals our rangeable pack.
    ReconstructedRegistryPackage {
        /// Which registry the artifact came from.
        registry: RegistryId,
        /// Checksum of the published artifact (e.g. crates.io sha256).
        checksum: ContentHash,
        /// Ephemeral fetch locator; unused at query time once the pack exists.
        package_uri: SmolStr,
    },
}

/// Published-version provenance stored per index row (§6.3).
///
/// Ties a published coordinate to the exact VCS ref (or reconstructed pack)
/// that backs it — not "whatever is on main today".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionProvenance {
    /// Published registry coordinate (e.g. `crates.io`).
    pub registry_id: RegistryId,
    /// Canonical published version (e.g. `0.7.9`).
    pub version: VersionCanonical,
    /// Registry checksum of the published artifact, when known.
    pub registry_checksum: Option<ContentHash>,
    /// How the source ObjectPack was acquired.
    pub acquisition: SourceAcquisition,
    /// The materialised source tree's pack identity.
    pub source_pack: ObjectPackId,
}
