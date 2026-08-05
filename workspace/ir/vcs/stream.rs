//! Host-side stream driver: wire an `ir-stream` byte transport into a
//! [`RecordingSession`] end-to-end (SMOLVM-PLAN §6.2).
//!
//! # Usage
//!
//! ```no_run
//! use std::num::NonZeroU64;
//! use ir_vcs::stream::{record_stream, StreamPolicy, StreamedRecording};
//!
//! # fn example() -> Result<(), ir_vcs::VcsError> {
//! # use ir::change::{EcosystemId, PackageLineageId, PackageName};
//! # use ir_vcs::repo::IrRepository;
//! # let pkg = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"));
//! # let mut repo = IrRepository::in_memory(pkg, "main").unwrap();
//! # let transport: std::io::Cursor<Vec<u8>> = std::io::Cursor::new(vec![]);
//! let outcome = record_stream(
//!     &mut repo,
//!     transport,
//!     StreamPolicy { checkpoint_every: NonZeroU64::new(1000) },
//! )?;
//! match outcome {
//!     StreamedRecording::Finished { report, .. } => {
//!         println!("recorded {} symbols", report.added + report.updated);
//!     }
//!     StreamedRecording::Aborted { failure, message, .. } => {
//!         eprintln!("producer aborted ({:?}): {}", failure, message);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::io::Read;
use std::num::NonZeroU64;

use crate::protocol::{
    BodyWire, FailureKindWire, PhaseWire, ProducerId, Received, StreamReceiver, WireEntry,
};
use heart::content::{ContentHash, JobKey};
use libpijul::changestore::ChangeStore;

use crate::error::VcsError;
use crate::repo::IrRepository;
use crate::serialize::LinkWire;
use crate::session::{FinishReport, StagedEntry};

// ---------------------------------------------------------------------------
// StreamPolicy
// ---------------------------------------------------------------------------

/// Controls when intermediate checkpoints are recorded during a stream.
///
/// A checkpoint records a partial libpijul change from the symbols staged so
/// far. It does **not** compute deletions (only [`RecordingSession::finish`]
/// does that), so each checkpoint is a durable intermediate point that can be
/// read out of the channel history.
#[derive(Debug, Clone)]
pub struct StreamPolicy {
    /// Record a checkpoint after every `N` staged entries (cumulative across
    /// all `Symbols` batches). `None` means only `finish()` records a change.
    pub checkpoint_every: Option<NonZeroU64>,
}

// ---------------------------------------------------------------------------
// SourceDigestEntry
// ---------------------------------------------------------------------------

/// A source file provenance record emitted by the producer.
#[derive(Debug, Clone)]
pub struct SourceDigestEntry {
    /// Relative path within the job source tree.
    pub path: String,
    /// BLAKE3 content hash of the file.
    pub hash: ContentHash,
    /// Byte length of the file.
    pub size: u64,
}

// ---------------------------------------------------------------------------
// ProgressSnapshot
// ---------------------------------------------------------------------------

/// A snapshot of the last `Progress` frame received before the terminal.
#[derive(Debug, Clone)]
pub struct ProgressSnapshot {
    /// Running emitted count reported by the producer.
    pub emitted: u64,
    /// Current producer phase at the time of the snapshot.
    pub phase: PhaseWire,
}

// ---------------------------------------------------------------------------
// StreamedRecording
// ---------------------------------------------------------------------------

/// The outcome of a [`record_stream`] call.
///
/// An aborted producer is a normal, expected outcome (the guest crashed, hit a
/// resource limit, etc.) — it is **not** returned as `Err`. Only *protocol*
/// errors (framing, count mismatch, truncation, decode) are `Err`, surfaced as
/// [`VcsError::Stream`].
#[derive(Debug)]
pub enum StreamedRecording {
    /// The stream completed normally; `finish()` was called on the session.
    Finished {
        /// The sealed job identity from the `Hello` handshake.
        job: JobKey,
        /// The producer type and version from the `Hello` handshake.
        producer: ProducerId,
        /// The finish report: tip, change hash, added/updated/deleted counts.
        report: FinishReport,
        /// Content hash of the producer's own output (for dedup / provenance).
        producer_digest: ContentHash,
        /// Source file provenance records emitted during the stream.
        sources: Vec<SourceDigestEntry>,
        /// Raw occurrence section bytes (opaque; concatenation of all chunks).
        occurrences: Vec<u8>,
        /// Merged treesitter+oracle body facts emitted during the stream, keyed
        /// by [`ir::change::IntroId`]. These are the implementation-plane
        /// companion of the declaration entries (INDEX-PLAN §5.1); the caller
        /// attaches them to the matching entry's `.nb` companion channel.
        bodies: Vec<BodyWire>,
        /// The last progress update emitted before `Finish`, if any.
        last_progress: Option<ProgressSnapshot>,
    },

    /// The producer aborted the stream before finishing.
    ///
    /// The recording session has been abandoned; the repository tip is
    /// unchanged. Callers can retry by starting a new `record_stream` (or a
    /// `record_generation`) on the same repository.
    Aborted {
        /// The sealed job identity from the `Hello` handshake.
        job: JobKey,
        /// The producer type and version from the `Hello` handshake.
        producer: ProducerId,
        /// Category of the failure reported by the producer.
        failure: FailureKindWire,
        /// Human-readable diagnostic message (for logging; not a security boundary).
        message: String,
    },
}

// ---------------------------------------------------------------------------
// record_stream
// ---------------------------------------------------------------------------

/// Drive a byte transport through a recording session end-to-end.
///
/// # Protocol
///
/// 1. Calls [`StreamReceiver::accept`] to read the `Hello` frame.
/// 2. Opens a [`RecordingSession`] via [`IrRepository::begin_recording`].
/// 3. Loops [`StreamReceiver::recv`]:
///    - `Symbols` → maps each [`WireEntry`] to a [`StagedEntry`] (verbatim
///      field transfer, including `parent`) and calls `session.stage(batch)`.
///      Applies the checkpoint policy on the cumulative staged count.
///    - `Links { from, batch }` → calls `session.stage_links(from, links)` to
///      add links to an already-staged symbol without re-encoding the payload.
///    - `SourceDigest` / `Occurrences` / `Progress` → accumulated into the
///      outcome for the caller's provenance recording.
///    - `Finish` → calls `session.finish()` and returns
///      [`StreamedRecording::Finished`].
///    - `Abort` → calls `session.abandon()` and returns
///      [`StreamedRecording::Aborted`] (not `Err`).
///
/// # Errors
///
/// Any [`crate::protocol::StreamError`] (framing violation, count mismatch,
/// truncation, decode failure, version mismatch) causes `session.abandon()` to
/// be called best-effort before returning [`VcsError::Stream`].
///
/// A [`VcsError::ForeignPackage`] from `session.stage()` propagates as-is
/// after abandoning the session.
pub fn record_stream<R, C>(
    repo: &mut IrRepository<C>,
    transport: R,
    policy: StreamPolicy,
) -> Result<StreamedRecording, VcsError>
where
    R: Read,
    C: ChangeStore + Clone + Send + 'static,
    C::Error: std::fmt::Display + Send + Sync + 'static,
{
    let mut rx = StreamReceiver::new(transport);

    // 1. Handshake — protocol errors before the session opens propagate directly.
    let (job, producer) = rx.accept()?;

    // 2. Open a recording session.
    let mut session = repo.begin_recording()?;

    // Provenance accumulators.
    let mut sources: Vec<SourceDigestEntry> = Vec::new();
    let mut occurrences: Vec<u8> = Vec::new();
    let mut bodies: Vec<BodyWire> = Vec::new();
    let mut last_progress: Option<ProgressSnapshot> = None;

    // Checkpoint tracking.
    let mut total_staged: u64 = 0;
    let mut last_checkpoint_at: u64 = 0;

    // 3. Main loop.
    loop {
        let event = match rx.recv() {
            Ok(Some(e)) => e,
            Ok(None) => {
                // Clean EOF without Finish/Abort — treated as truncation.
                let _ = session.abandon();
                return Err(VcsError::Stream(crate::protocol::StreamError::Protocol(
                    "stream ended without Finish or Abort".into(),
                )));
            }
            Err(stream_err) => {
                let _ = session.abandon();
                return Err(VcsError::Stream(stream_err));
            }
        };

        match event {
            Received::Symbols(batch) => {
                let count = batch.len() as u64;
                let staged: Vec<StagedEntry> = batch
                    .into_iter()
                    .map(|e: WireEntry| StagedEntry {
                        stable: e.stable,
                        payload: e.payload,
                        parent: e.parent,
                        links: convert_links(e.links),
                    })
                    .collect();

                if let Err(e) = session.stage(staged) {
                    let _ = session.abandon();
                    return Err(e);
                }

                total_staged += count;

                // Apply checkpoint policy.
                if let Some(n) = policy.checkpoint_every {
                    let n_val = n.get();
                    let current_multiple = (total_staged / n_val) * n_val;
                    if current_multiple > last_checkpoint_at && current_multiple > 0 {
                        if let Err(e) = session.checkpoint("checkpoint") {
                            let _ = session.abandon();
                            return Err(e);
                        }
                        last_checkpoint_at = current_multiple;
                    }
                }
            }

            Received::Links { from, batch } => {
                let links: Vec<LinkWire> = batch
                    .into_iter()
                    .map(|wl| LinkWire {
                        other: wl.other,
                        kind_self: wl.kind_self,
                        kind_other: wl.kind_other,
                    })
                    .collect();

                if let Err(e) = session.stage_links(from, links) {
                    let _ = session.abandon();
                    return Err(e);
                }
            }

            Received::SourceDigest { path, hash, size } => {
                sources.push(SourceDigestEntry { path, hash, size });
            }

            Received::Occurrences(chunk) => {
                occurrences.extend_from_slice(&chunk);
            }

            Received::Bodies(batch) => {
                // Body facts are an additive extension slot on the entry
                // (INDEX-PLAN §5.1). They are (a) staged into the session so
                // Phase B can feed the body axis into the continuity matcher and
                // persist the `.nb` companion at the durable id, and (b) still
                // handed to the caller for any out-of-band attachment. They do
                // not stage a declaration entry and do not move declaration bytes.
                session.stage_bodies(&batch);
                bodies.extend(batch);
            }

            Received::Progress { emitted, phase } => {
                last_progress = Some(ProgressSnapshot { emitted, phase });
            }

            Received::Finish {
                emitted: _,
                producer_digest,
            } => {
                let report = session.finish()?;
                return Ok(StreamedRecording::Finished {
                    job,
                    producer,
                    report,
                    producer_digest,
                    sources,
                    occurrences,
                    bodies,
                    last_progress,
                });
            }

            Received::Abort { failure, message } => {
                let _ = session.abandon();
                return Ok(StreamedRecording::Aborted {
                    job,
                    producer,
                    failure,
                    message,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert `crate::protocol::WireLink` vec to `crate::serialize::LinkWire` vec.
///
/// The two types are structurally identical; this is a crate-boundary rename
/// with no data transformation.
fn convert_links(src: Vec<crate::protocol::WireLink>) -> Vec<LinkWire> {
    src.into_iter()
        .map(|wl| LinkWire {
            other: wl.other,
            kind_self: wl.kind_self,
            kind_other: wl.kind_other,
        })
        .collect()
}
