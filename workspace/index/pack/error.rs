//! The total error taxonomy for the NDPK v1 engine.
//!
//! Every fallible public entry point returns [`PackError`]. No engine path
//! panics on malformed input; adversarial bytes (truncation, bad magic, version
//! skew, TOC hash mismatch, corrupt frames) map to a typed variant.

use std::io;

use heart::object_pack::MemberKey;

/// Every way an ObjectPack operation can fail.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The bytes are too short to contain the structure being read (header,
    /// TOC, a member frame, or a chunk frame). Carries what was expected.
    #[error("truncated pack")]
    Truncated {
        /// Byte offset where the read began.
        offset: u64,
        /// Bytes required from that offset.
        needed: u64,
        /// Bytes actually available from that offset.
        available: u64,
    },

    /// The leading four bytes were not `NDPK`.
    #[error("bad magic")]
    BadMagic {
        /// The four bytes that were present.
        found: [u8; 4],
    },

    /// The container version is outside the readable range (`< MIN_READ` or
    /// `> CURRENT`, per INDEX-PLAN ID-22 N/N-1 support).
    #[error("unsupported container version")]
    UnsupportedVersion {
        /// The version found in the header.
        found: u16,
        /// Lowest readable version.
        min_read: u16,
        /// Highest readable (current) version.
        current: u16,
    },

    /// The unknown header flag bits were set — a forward-incompatible pack.
    #[error("unknown header flags set")]
    UnknownFlags {
        /// The raw flags field.
        bits: u16,
    },

    /// A structural offset/length pair in the header or a TOC row points
    /// outside the pack, or a length arithmetic overflowed.
    #[error("structural bounds violated")]
    BadStructure {
        /// Human-readable detail of the inconsistency (domain data).
        detail: String,
    },

    /// The TOC did not decode as valid postcard — untrusted bytes are corrupt
    /// or were produced by an incompatible writer.
    #[error("table of contents decode failed")]
    TocDecode(#[source] postcard::Error),

    /// The TOC could not be *encoded* to postcard while sealing a pack or
    /// re-deriving canonical bytes for verification.
    ///
    /// Deliberately distinct from [`PackError::TocDecode`]: encoding runs over a
    /// `TableOfContents` we already hold in memory, so a failure here is a local
    /// logic or allocation fault and can never be caused by untrusted input. A
    /// caller that treats it as pack corruption — quarantining the pack, marking
    /// the peer bad — would be acting on the wrong diagnosis. The two directions
    /// shared one variant, which is what let both call sites stringify their
    /// cause into a `String` the variant could not hold.
    #[error("table of contents encode failed")]
    TocEncode(#[source] postcard::Error),

    /// The recomputed BLAKE3 of the TOC bytes did not match the id / embedded
    /// digest — the pack is corrupt or tampered.
    #[error("table of contents hash mismatch")]
    TocHashMismatch,

    /// The TOC was not sorted strictly ascending by [`MemberKey`], or contained
    /// a duplicate key. A well-formed pack never violates this.
    #[error("table of contents is not strictly sorted by member key")]
    TocNotSorted,

    /// A member's recomputed content digest did not match its TOC row.
    #[error("member content hash mismatch")]
    MemberHashMismatch {
        /// The offending member key.
        key: MemberKey,
    },

    /// A zstd frame failed to decompress. Carries the frame's byte offset and
    /// the underlying zstd failure as a real `#[source]`, so the whole chain is
    /// walkable (§8: reading only the terse top-level `Display` is how a
    /// five-second diagnosis becomes an hour).
    ///
    /// The offset is a field rather than text spliced into the message because
    /// that is the datum a caller acts on — it identifies which frame to re-fetch.
    #[error("zstd frame decode failed at offset {offset}")]
    FrameDecode {
        /// Byte offset of the frame that failed to decompress.
        offset: u64,
        /// The underlying zstd/io failure, preserved rather than stringified.
        source: io::Error,
    },

    /// A zstd frame decompressed cleanly but yielded a different byte count than
    /// its TOC row declares.
    ///
    /// This is a container-integrity violation, not an I/O failure, and it used
    /// to be forced through [`PackError::FrameDecode`] by manufacturing a
    /// synthetic `io::Error` whose only payload was a formatted message. That is
    /// what made `format_args!` reachable here at all: the variant could carry an
    /// `io::Error` but not the two numbers that *are* the evidence. They are
    /// fields now, so the condition is inspectable without parsing a string.
    #[error(
        "frame length mismatch at offset {offset}: TOC declares {expected} bytes, frame decoded to {actual}"
    )]
    FrameLengthMismatch {
        /// Byte offset of the offending frame.
        offset: u64,
        /// Uncompressed length the TOC row declares.
        expected: u64,
        /// Uncompressed length the frame actually decoded to.
        actual: u64,
    },

    /// A zstd frame failed to *compress* while sealing a pack.
    #[error("zstd frame encode failed")]
    FrameEncode(#[source] io::Error),

    /// A lookup was performed for a member not present in the pack.
    #[error("no such member")]
    MemberNotFound {
        /// The key that was requested.
        key: MemberKey,
    },

    /// A requested byte range was invalid: `start > end`, or `end` beyond the
    /// member's uncompressed length.
    #[error("range out of bounds")]
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
    #[error("absolute path rejected")]
    AbsolutePath {
        /// The offending path.
        path: String,
    },

    /// A path contained a `.` or `..` segment, an empty segment, a backslash,
    /// a NUL, or otherwise failed normalization.
    #[error("unsafe path segment")]
    UnsafePathSegment {
        /// The offending path.
        path: String,
        /// Why it was rejected (domain data).
        reason: String,
    },

    /// Two members were added with the same [`MemberKey`].
    #[error("duplicate member key")]
    DuplicateMember {
        /// The duplicated key.
        key: MemberKey,
    },

    /// A member exceeded the maximum encodable size (`u64` chunk arithmetic
    /// would overflow), or the pack exceeded addressable size.
    #[error("member or pack too large")]
    TooLarge {
        /// Human-readable detail (domain data).
        detail: String,
    },

    // ---- Bao verified streaming (INDEX-PLAN §6.2) ----
    /// Generating a verified Bao range encoding failed — the member bytes did
    /// not verify against their outboard (corruption between pack and outboard).
    #[error("bao range encode failed")]
    BaoEncode(#[source] io::Error),

    /// Verifying a received Bao range failed — a leaf or interior hash did not
    /// match the announced root. The transferred bytes are tampered or corrupt.
    #[error("bao range verification failed")]
    BaoDecode(#[source] io::Error),

    /// An outboard was requested for a member that does not carry one (its
    /// uncompressed length is below [`heart::object_pack::BAO_OUTBOARD_THRESHOLD_BYTES`]).
    #[error("no bao outboard (sub-threshold)")]
    NoOutboard {
        /// The member key.
        key: MemberKey,
    },

    // ---- Transport (INDEX-PLAN §7.2) ----
    /// An iroh transport operation failed (bind, connect, stream, or blob I/O).
    #[error("iroh transport error")]
    Transport(#[source] io::Error),

    /// The provider answered the protocol correctly and *declined* the request,
    /// carrying its stated reason.
    ///
    /// Deliberately not a [`PackError::Transport`]: the link is healthy and the
    /// exchange completed, so this is the one negative outcome that retrying
    /// against the same peer cannot fix. Folding it into `Transport` — which is
    /// what a hand-built `io::ErrorKind::Other` did — erased exactly the
    /// distinction a caller needs to choose between "retry" and "ask elsewhere",
    /// and left the peer's reason recoverable only by parsing a message.
    #[error("provider refused the request: {reason}")]
    ProviderRefused {
        /// The reason the provider gave for declining (domain data).
        reason: String,
    },

    /// A provide/fetch was attempted against an endpoint that is not on the
    /// device's trusted-remote list, or lacks the required capability
    /// (INDEX-PLAN ID-18).
    #[error("endpoint not enrolled or lacks capability")]
    NotEnrolled {
        /// Which endpoint / which capability was missing (domain data).
        detail: String,
    },

    /// A fetched pack's recomputed [`heart::object_pack::ObjectPackId`] did not
    /// match the id that was requested — a wrong or tampered pack was served.
    #[error("fetched pack id mismatch")]
    FetchedIdMismatch,

    /// A wire message could not be encoded or decoded.
    #[error("transport codec error")]
    Codec(#[source] postcard::Error),

    /// An iroh transport method was called before the transport wave landed.
    /// Retained for the trait signature during migration; new code returns a
    /// specific variant above.
    #[error("iroh transport is not wired yet")]
    TransportNotWired,

    // ---- Host I/O ----
    /// A filesystem operation failed (store / tree walk / mmap open).
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Map a shared [`heart::sync::SyncError`] (produced by the `transport` crate's
/// framing / endpoint helpers) onto the pack-format [`PackError`] so the pack
/// transport surface is unchanged. Codec failures stay codec failures; transport
/// failures fold into PackError::Transport; I/O errors pass through directly.
impl From<heart::sync::SyncError> for PackError {
    fn from(error: heart::sync::SyncError) -> Self {
        match error {
            heart::sync::SyncError::Codec(source) => PackError::Codec(source),
            heart::sync::SyncError::Transport(source) => PackError::Transport(source),
            heart::sync::SyncError::Io(source) => PackError::Io(source),
            heart::sync::SyncError::VerificationFailed(_) => {
                PackError::Transport(io::Error::other(error))
            }
            other => PackError::Transport(io::Error::other(other)),
        }
    }
}

/// Convenience alias for engine results.
pub type PackResult<T> = std::result::Result<T, PackError>;
