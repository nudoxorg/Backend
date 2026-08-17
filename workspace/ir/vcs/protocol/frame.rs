//! Frame types for the `ir-stream` protocol (SMOLVM-PLAN §6.1).
//!
//! # K27 discipline
//!
//! All frame types carry only:
//! - [`ir::change::StableRef`] — wire-stable cross-package symbol references.
//! - [`crate::wire::OwnedEntryPayload`] — the sealed payload wire twin.
//! - [`heart::content::ContentHash`] / [`heart::content::JobKey`] — content digests.
//! - Primitive scalars (counts, paths, version numbers).
//!
//! Arena indices (`IntroId` assigned by the host) and interned string ids are
//! **never** present in any frame. The type definitions make this impossible to
//! violate accidentally.

use crate::wire::OwnedEntryPayload;
use heart::content::{ContentHash, JobKey};
use ir::change::{IntroId, StableRef};
use ir::kind::KindDiscriminant;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// BodyWire
// ---------------------------------------------------------------------------

/// The merged treesitter+oracle body facts for one entry, keyed by the entry's
/// content-derived [`IntroId`].
///
/// # K27 discipline
///
/// `intro` is a *content-derived* identity (BLAKE3 of the introduction
/// preimage), never a transient arena index. Producers compute it with
/// `ir::intro::bootstrap_intro_id` and transmit it verbatim so the host
/// can attach the body facts to the correct entry without depending on stream
/// order. This preserves the K27 invariant: no arena indices appear in any
/// frame.
///
/// # Body plane separation
///
/// A `BodyWire` travels in a [`StreamFrame::Bodies`] frame, which is kept
/// separate from [`StreamFrame::Symbols`] so the host can stage declaration
/// and implementation facts independently. Both frames share the same
/// `IntroId` as the join key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct BodyWire {
    /// Content-derived identity of the owning entry (never an arena index).
    pub intro: IntroId,
    /// The merged body facts for this entry.
    pub body: ir::body::BodyEmbed,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Protocol version announced in the `Hello` frame.
///
/// The host rejects connections with a different version via
/// [`crate::protocol::Error::VersionMismatch`].
pub const IR_STREAM_VERSION: u32 = 1;

/// Maximum allowed size for a single postcard-encoded frame (4 MiB).
///
/// This is well under smolvm's 32 MiB vsock cap (§6.1). Both [`crate::protocol::FrameWriter`]
/// and [`crate::protocol::FrameReader`] enforce this limit. The writer rejects encoding;
/// the reader rejects the *declared length* without allocating the bytes.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Number of entries buffered in [`crate::protocol::SymbolSink`] before an auto-flush.
///
/// The actual flush threshold is the smaller of this count and the
/// [`MAX_FRAME_BYTES`] limit computed during encoding.
pub(crate) const SINK_BATCH_ENTRIES: usize = 256;

/// Dedicated vsock port for the guest→host IR stream (SMOLVM-PLAN §6.1).
///
/// The guest producer connects to this port; the host binds a Unix socket per
/// job and registers it with smolvm's `VsockPort` mechanism so the VM routes
/// connections here. Port 4100 is in the user-defined range (> 1024, < 5000)
/// and chosen to avoid collision with smolvm's own JSON control channel.
pub const IR_STREAM_VSOCK_PORT: u32 = 4100;

// ---------------------------------------------------------------------------
// ProducerId
// ---------------------------------------------------------------------------

/// Strongly-typed identifier for the producing agent.
///
/// A producer id is a short, human-readable string (e.g. `"rust-1.79"`,
/// `"typescript-oxc"`, `"python-pyrefly"`) that names the producer type and
/// optionally its version. The host uses it for logging and metrics; it is
/// not a security boundary.
///
/// # Invariants
///
/// - Must be non-empty.
/// - Must be valid UTF-8 (guaranteed by `String`).
/// - Maximum 128 bytes (enforced by [`ProducerId::new`]).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ProducerId(String);

impl ProducerId {
    /// Maximum byte length of a producer id string.
    pub const MAX_LEN: usize = 128;

    /// Construct a `ProducerId` from a string, returning `None` if it is empty
    /// or exceeds [`ProducerId::MAX_LEN`] bytes.
    pub fn new(s: impl Into<String>) -> Option<Self> {
        let s = s.into();
        if s.is_empty() || s.len() > Self::MAX_LEN {
            return None;
        }
        Some(Self(s))
    }

    /// Infallible constructor for compile-time-known literals.
    ///
    /// # Panics
    ///
    /// Panics if `s` is empty or longer than [`ProducerId::MAX_LEN`] bytes.
    /// Only call this with string literals or other statically-known values.
    pub fn from_static(s: &'static str) -> Self {
        Self::new(s).expect("ProducerId::from_static: invalid string")
    }

    /// The underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProducerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// PhaseWire
// ---------------------------------------------------------------------------

/// The current phase of a producer, reported in [`StreamFrame::Progress`] frames.
///
/// Phases are reported in order, though a producer may skip phases that are not
/// applicable to it (e.g. a static parser skips `Acquire`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum PhaseWire {
    /// Fetching source code or dependencies (network I/O, registry pulls).
    Acquire,
    /// Parsing source text into a CST or AST.
    Parse,
    /// Lowering the AST into IR entries (type resolution, kind assignment).
    Lower,
    /// Emitting [`WireEntry`] / [`WireLink`] frames over the stream.
    Emit,
    /// Sealing the produce (computing the producer digest, finalizing).
    Seal,
}

// ---------------------------------------------------------------------------
// FailureKindWire
// ---------------------------------------------------------------------------

/// The category of failure reported in an [`StreamFrame::Abort`] frame.
///
/// The host uses this to classify the failure in metrics and to decide whether
/// a retry makes sense.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum FailureKindWire {
    /// The producer process panicked or was killed by a signal.
    ProducerPanic,
    /// A required language toolchain (compiler, runtime) was not found or had
    /// an incompatible version.
    ToolchainMissing,
    /// The producer exceeded a resource limit (CPU time, memory, output size).
    LimitExceeded,
    /// An internal error in the producer that does not fit another category.
    Internal,
}

// ---------------------------------------------------------------------------
// WireLink
// ---------------------------------------------------------------------------

/// A directional semantic link between two symbols.
///
/// Links carry only stable identities (K27): the `other` endpoint is a
/// [`StableRef`] (ecosystem + package + `IntroId`), and the kind discriminants
/// are frozen wire values.
///
/// The canonical owner (as defined in `nudox-ir-vcs/serialize.rs`) is the
/// symbol with the smaller `IntroId` byte representation. Cross-package links:
/// the local endpoint always owns the entry on the host side.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WireLink {
    /// The other endpoint of the link.
    pub other: StableRef,
    /// The kind discriminant of the *self* (emitting) endpoint.
    pub kind_self: KindDiscriminant,
    /// The kind discriminant of the `other` endpoint.
    pub kind_other: KindDiscriminant,
}

// ---------------------------------------------------------------------------
// WireEntry
// ---------------------------------------------------------------------------

/// A single IR symbol entry ready to be staged into the host recording session.
///
/// `WireEntry` is the dictated shared contract (see task spec): field names and
/// types must match exactly so the host crate can mirror this definition without
/// re-deriving.
///
/// # K27 discipline
///
/// `stable` is a [`StableRef`] (wire-stable, cross-package). `payload` is the
/// sealed wire twin. `links` are [`WireLink`]s. No arena indices appear here.
///
/// `parent` carries the nesting edge if this symbol was introduced as a child of
/// another symbol. It is an [`IntroId`] — a *content-derived identity* (BLAKE3
/// of the introduction preimage), not a transient arena index. The host assigns
/// `IntroId`s deterministically via `intro::determin`; the guest transmits the
/// same value it computed locally so the host can validate consistency rather than
/// having to derive the nesting topology from the stream order.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct WireEntry {
    /// Wire-stable identity of this symbol (ecosystem + package + intro hash).
    pub stable: StableRef,
    /// The sealed symbol payload (symbol metadata + kind body + content hash).
    pub payload: OwnedEntryPayload,
    /// Parent intro for nesting, if any.
    ///
    /// This is a *content-derived* [`IntroId`] — never an arena-local index.
    /// Producers set this when the symbol was introduced as a child of another
    /// symbol. The host uses it verbatim when staging via
    /// [`nudox_ir_vcs::session::StagedEntry::parent`].
    pub parent: Option<IntroId>,
    /// Semantic links originating from this symbol.
    pub links: Vec<WireLink>,
}

// ---------------------------------------------------------------------------
// StreamFrame
// ---------------------------------------------------------------------------

/// A single protocol frame exchanged between guest producer and host session.
///
/// Frames are postcard-encoded and length-prefixed (4-byte little-endian).
/// Only one variant is legal as the first frame ([`StreamFrame::Hello`]); only
/// [`StreamFrame::Finish`] and [`StreamFrame::Abort`] are terminal.
///
/// # Ordering rules (enforced by [`crate::protocol::StreamReceiver`])
///
/// 1. First frame **must** be `Hello`.
/// 2. `Hello` must appear exactly once.
/// 3. After `Finish` or `Abort`, no further frames may be sent.
/// 4. `Finish.emitted` must equal the sum of all `Symbols` batch lengths.
#[derive(Clone, PartialEq, Serialize, Deserialize, Debug)]
pub enum StreamFrame {
    /// Handshake: identifies the stream version, the job being compiled, and
    /// the producer generating the IR.
    Hello {
        /// Protocol version — must equal [`IR_STREAM_VERSION`].
        version: u32,
        /// The sealed job identity (BLAKE3 of producer + toolchain + source + deps).
        job: JobKey,
        /// The producer type and version string.
        producer: ProducerId,
    },

    /// A batch of IR symbol entries (K27: wire twins, no arena indices).
    Symbols {
        /// The symbol entries in this batch. May be empty only if the host must
        /// update its count; producers should avoid empty batches.
        batch: Vec<WireEntry>,
    },

    /// A batch of semantic links originating from a specific symbol.
    ///
    /// `from` identifies the emitting (self) endpoint; the host uses it to
    /// route the links to the correct symbol file under the canonical-owner
    /// rule. Without `from` the host cannot determine which symbol owns these
    /// links — that was the original design flaw this field corrects.
    Links {
        /// The emitting symbol whose links this batch describes.
        from: StableRef,
        /// The links in this batch.
        batch: Vec<WireLink>,
    },

    /// Content digest and size of a source file used during compilation.
    ///
    /// The host uses this to populate occurrence index provenance and to
    /// validate source freshness on next compile.
    SourceDigest {
        /// Relative path of the source file within the job source tree.
        path: String,
        /// BLAKE3 content hash of the file.
        hash: ContentHash,
        /// Byte length of the file.
        size: u64,
    },

    /// A chunked section of serialized occurrence data.
    ///
    /// Occurrence data is optional and producer-specific. The host records it
    /// opaquely and associates it with the current job. Senders must chunk
    /// occurrence data to stay under [`MAX_FRAME_BYTES`].
    Occurrences {
        /// A raw serialized occurrence section chunk (≤ [`MAX_FRAME_BYTES`] bytes).
        section: Vec<u8>,
    },

    /// A batch of merged body facts, one [`BodyWire`] per entry.
    ///
    /// Body facts travel in their own frame type (separate from
    /// [`StreamFrame::Symbols`]) so the host can stage declaration and
    /// implementation facts independently. The join key is
    /// [`BodyWire::intro`], which matches the [`WireEntry::parent`] /
    /// [`ir::change::IntroId`] used in the declaration plane.
    ///
    /// Producers call [`crate::protocol::SymbolSink::emit_body`] or
    /// [`crate::protocol::SymbolSink::emit_bodies`] to produce these frames.
    /// The receiver surfaces them as [`crate::protocol::Received::Bodies`].
    Bodies {
        /// The body-fact records in this batch.
        batch: Vec<BodyWire>,
    },

    /// A progress update from the producer.
    Progress {
        /// Number of IR entries emitted so far (running total).
        emitted: u64,
        /// Current producer phase.
        phase: PhaseWire,
    },

    /// Successful termination of the stream.
    ///
    /// The host validates that `emitted` equals the sum of all `Symbols`
    /// batch lengths.
    Finish {
        /// Total number of IR entries emitted across all `Symbols` batches.
        emitted: u64,
        /// Content hash of the producer's own output (for dedup / provenance).
        producer_digest: ContentHash,
    },

    /// Abnormal termination of the stream.
    ///
    /// The host records the failure and abandons the recording session.
    Abort {
        /// Category of the failure.
        failure: FailureKindWire,
        /// Human-readable description (for logging; not a security boundary).
        message: String,
    },
}

impl StreamFrame {
    /// Returns the name of this variant (for error messages).
    pub fn variant_name(&self) -> &'static str {
        match self {
            StreamFrame::Hello { .. } => "Hello",
            StreamFrame::Symbols { .. } => "Symbols",
            StreamFrame::Links { .. } => "Links",
            StreamFrame::SourceDigest { .. } => "SourceDigest",
            StreamFrame::Occurrences { .. } => "Occurrences",
            StreamFrame::Bodies { .. } => "Bodies",
            StreamFrame::Progress { .. } => "Progress",
            StreamFrame::Finish { .. } => "Finish",
            StreamFrame::Abort { .. } => "Abort",
        }
    }

    /// Returns `true` if this frame terminates the protocol.
    pub fn is_terminal(&self) -> bool {
        matches!(self, StreamFrame::Finish { .. } | StreamFrame::Abort { .. })
    }
}
