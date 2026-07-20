//! The total error taxonomy for the NDPK v1 engine.
//!
//! Every fallible public entry point returns [`PackError`]. No engine path
//! panics on malformed input; adversarial bytes (truncation, bad magic, version
//! skew, TOC hash mismatch, corrupt frames) map to a typed variant.

use heart::object_pack::MemberKey;

/// Every way an ObjectPack operation can fail.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The bytes are too short to contain the structure being read (header,
    /// TOC, a member frame, or a chunk frame). Carries what was expected.
    #[error("truncated pack: needed {needed} bytes at offset {offset}, had {available}")]
    Truncated {
        /// Byte offset where the read began.
        offset: u64,
        /// Bytes required from that offset.
        needed: u64,
        /// Bytes actually available from that offset.
        available: u64,
    },

    /// The leading four bytes were not `NDPK`.
    #[error("bad magic: expected NDPK, found {found:02x?}")]
    BadMagic {
        /// The four bytes that were present.
        found: [u8; 4],
    },

    /// The container version is outside the readable range (`< MIN_READ` or
    /// `> CURRENT`, per INDEX-PLAN ID-22 N/N-1 support).
    #[error("unsupported container version {found} (readable {min_read}..={current})")]
    UnsupportedVersion {
        /// The version found in the header.
        found: u16,
        /// Lowest readable version.
        min_read: u16,
        /// Highest readable (current) version.
        current: u16,
    },

    /// The unknown header flag bits were set — a forward-incompatible pack.
    #[error("unknown header flags set: {bits:#06x}")]
    UnknownFlags {
        /// The raw flags field.
        bits: u16,
    },

    /// A structural offset/length pair in the header or a TOC row points
    /// outside the pack, or a length arithmetic overflowed.
    #[error("structural bounds violated: {detail}")]
    BadStructure {
        /// Human-readable detail of the inconsistency.
        detail: String,
    },

    /// The TOC did not decode as valid postcard.
    #[error("table of contents decode failed: {0}")]
    TocDecode(String),

    /// The recomputed BLAKE3 of the TOC bytes did not match the id / embedded
    /// digest — the pack is corrupt or tampered.
    #[error("table of contents hash mismatch (pack is corrupt or tampered)")]
    TocHashMismatch,

    /// The TOC was not sorted strictly ascending by [`MemberKey`], or contained
    /// a duplicate key. A well-formed pack never violates this.
    #[error("table of contents is not strictly sorted by member key")]
    TocNotSorted,

    /// A member's recomputed content digest did not match its TOC row.
    #[error("member content hash mismatch for key {key:?}")]
    MemberHashMismatch {
        /// The offending member key.
        key: MemberKey,
    },

    /// A zstd frame failed to decompress, or produced the wrong byte count.
    #[error("zstd frame decode failed: {detail}")]
    FrameDecode {
        /// Human-readable detail.
        detail: String,
    },

    /// A zstd frame failed to *compress* while sealing a pack.
    #[error("zstd frame encode failed: {detail}")]
    FrameEncode {
        /// Human-readable detail.
        detail: String,
    },

    /// A lookup was performed for a member not present in the pack.
    #[error("no such member: {key:?}")]
    MemberNotFound {
        /// The key that was requested.
        key: MemberKey,
    },

    /// A requested byte range was invalid: `start > end`, or `end` beyond the
    /// member's uncompressed length.
    #[error(
        "invalid range {start}..{end} for member of {member_length} uncompressed bytes"
    )]
    RangeOutOfBounds {
        /// Requested start (inclusive).
        start: u64,
        /// Requested end (exclusive).
        end: u64,
        /// The member's total uncompressed length.
        member_length: u64,
    },

    // ---- Builder-side path validation (INDEX-PLAN §6.2) ----
    /// A `Source` member path was absolute (started with `/` or a drive/UNC
    /// prefix).
    #[error("absolute path rejected: {path:?}")]
    AbsolutePath {
        /// The offending path.
        path: String,
    },

    /// A path contained a `.` or `..` segment, an empty segment, a backslash,
    /// a NUL, or otherwise failed normalization.
    #[error("unsafe path segment in {path:?}: {reason}")]
    UnsafePathSegment {
        /// The offending path.
        path: String,
        /// Why it was rejected.
        reason: String,
    },

    /// Two members were added with the same [`MemberKey`].
    #[error("duplicate member key: {key:?}")]
    DuplicateMember {
        /// The duplicated key.
        key: MemberKey,
    },

    /// A member exceeded the maximum encodable size (`u64` chunk arithmetic
    /// would overflow), or the pack exceeded addressable size.
    #[error("member or pack too large: {detail}")]
    TooLarge {
        /// Human-readable detail.
        detail: String,
    },

    // ---- Bao verified streaming (INDEX-PLAN §6.2) ----
    /// Generating a verified Bao range encoding failed — the member bytes did
    /// not verify against their outboard (corruption between pack and outboard).
    #[error("bao range encode failed: {detail}")]
    BaoEncode {
        /// Human-readable detail.
        detail: String,
    },

    /// Verifying a received Bao range failed — a leaf or interior hash did not
    /// match the announced root. The transferred bytes are tampered or corrupt.
    #[error("bao range verification failed: {detail}")]
    BaoDecode {
        /// Human-readable detail.
        detail: String,
    },

    /// An outboard was requested for a member that does not carry one (its
    /// uncompressed length is below [`heart::object_pack::BAO_OUTBOARD_THRESHOLD_BYTES`]).
    #[error("no bao outboard for member {key:?} (sub-threshold)")]
    NoOutboard {
        /// The member key.
        key: MemberKey,
    },

    // ---- Transport (INDEX-PLAN §7.2) ----
    /// An iroh transport operation failed (bind, connect, stream, or blob I/O).
    #[error("iroh transport error: {detail}")]
    Transport {
        /// Human-readable detail.
        detail: String,
    },

    /// A provide/fetch was attempted against an endpoint that is not on the
    /// device's trusted-remote list, or lacks the required capability
    /// (INDEX-PLAN ID-18).
    #[error("endpoint not enrolled or lacks capability: {detail}")]
    NotEnrolled {
        /// Human-readable detail (which endpoint / which capability).
        detail: String,
    },

    /// A fetched pack's recomputed [`heart::object_pack::ObjectPackId`] did not
    /// match the id that was requested — a wrong or tampered pack was served.
    #[error("fetched pack id mismatch: requested a different pack than was served")]
    FetchedIdMismatch,

    /// A wire message could not be encoded or decoded.
    #[error("transport codec error: {detail}")]
    Codec {
        /// Human-readable detail.
        detail: String,
    },

    /// An iroh transport method was called before the transport wave landed.
    /// Retained for the trait signature during migration; new code returns a
    /// specific variant above.
    #[error("iroh transport is not wired yet (INDEX-PLAN §7.2, later wave)")]
    TransportNotWired,

    // ---- Host I/O ----
    /// A filesystem operation failed (store / tree walk / mmap open).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience alias for engine results.
pub type PackResult<T> = std::result::Result<T, PackError>;
