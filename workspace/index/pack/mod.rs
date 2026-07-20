//! object-pack — the NDPK v1 container engine (INDEX-PLAN §6, ID-13/ID-16).
//!
//! One format for source trees, VM goldens, and compile stages:
//! range-addressable, highly compressed, content-addressed. A single snippet is
//! one *range get* — never a whole-tree load.
//!
//! # Layout
//!
//! ```text
//! ┌────────────────────────── NDPK v1 pack file ──────────────────────────┐
//! │ Header (fixed 24 bytes, little-endian)                                 │
//! │   magic b"NDPK" | version u16=1 | flags u16 | toc_offset u64 | toc_len │
//! │ Members — zstd frames. A large Source member is split into             │
//! │   independently-decompressable 128 KiB chunks (one frame each).        │
//! │ TOC — sorted by MemberKey, postcard-encoded, blake3-checked.           │
//! └────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! The container vocabulary ([`heart::object_pack::MemberKey`],
//! [`heart::object_pack::MemberRecord`], [`heart::object_pack::ObjectPackId`],
//! …) is frozen in `heart`; this crate owns the *engines*.
//!
//! # Determinism (hard requirement)
//!
//! The same input tree produces byte-identical pack bytes and thus the same
//! [`ObjectPackId`]. Enforced by: a fixed zstd level, no timestamps in any
//! byte, TOC iteration sorted by [`heart::object_pack::MemberKey`], insertion
//! order irrelevant, and a stable binary codec (postcard). See `FORMAT.md`.
//!
//! [`ObjectPackId`]: heart::object_pack::ObjectPackId

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod builder;
pub mod error;
pub mod format;
pub mod outboard;
pub mod reader;
pub mod store;
pub mod sync;
pub mod transport;
pub mod tree;

pub use builder::ObjectPackBuilder;
pub use error::PackError;
pub use outboard::{MemberOutboard, OutboardSidecar, verify_bao_range};
pub use reader::ObjectPackReader;
pub use store::{FilesystemObjectPackStore, ObjectPackStore};
pub use sync::{ObjectPackApplyHook, ObjectPackContentIo};
pub use transport::{
    ObjectPackFetcher, ObjectPackProvider, ProvideTarget, provide_to_trusted,
};
pub use tree::TreeIngest;

// Re-export the frozen vocabulary for ergonomic downstream use.
pub use heart::object_pack::{
    BAO_OUTBOARD_THRESHOLD_BYTES, ByteLength, ChunkEntry, ImageDigest, MemberKey, MemberRecord,
    ObjectPackId, PackOffset, RelativePath, SOURCE_CHUNK_SIZE_BYTES, SourceAcquisition,
    VersionProvenance,
};

/// The zstd compression level used for every frame in every pack.
///
/// A **format constant**: it is part of what makes packs deterministic and is
/// mixed into the [`ObjectPackId`] policy bytes. Level 19 is a strong,
/// still-fast-to-decompress preset; changing it changes every pack's bytes.
///
/// [`ObjectPackId`]: heart::object_pack::ObjectPackId
pub const ZSTD_COMPRESSION_LEVEL: i32 = 19;

/// The four-byte container magic.
pub const NDPK_MAGIC: [u8; 4] = *b"NDPK";

/// The current container format version.
pub const NDPK_VERSION_CURRENT: u16 = 1;

/// The lowest container format version this build can *read* (N-1 support per
/// INDEX-PLAN ID-22). Equal to [`NDPK_VERSION_CURRENT`] until a v2 exists.
pub const NDPK_VERSION_MIN_READ: u16 = 1;
