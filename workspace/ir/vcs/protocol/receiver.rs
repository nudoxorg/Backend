//! Host-side protocol state machine.
//!
//! [`StreamReceiver`] enforces the protocol ordering rules and validates the
//! emitted-count invariant. It surfaces all violations as typed
//! [`crate::protocol::StreamError`] variants.
//!
//! # State machine
//!
//! ```text
//! Initial ──Hello──▶ Ready ──data frames──▶ Ready ──Finish/Abort──▶ Done
//!                                                        │
//!                     (any ordering violation)───────────▶ Err(Protocol)
//! ```

use std::io::Read;

use ir::change::StableRef;

use crate::protocol::error::StreamError;
use crate::protocol::frame::{
    BodyWire, FailureKindWire, IR_STREAM_VERSION, PhaseWire, ProducerId, StreamFrame, WireEntry,
    WireLink,
};
use crate::protocol::io::FrameReader;
use heart::content::{ContentHash, JobKey};

// ---------------------------------------------------------------------------
// Receiver state
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum ReceiverState {
    /// Waiting for the first `Hello` frame.
    AwaitingHello,
    /// `Hello` received; expecting data frames or a terminal.
    Ready,
    /// A terminal (`Finish` or `Abort`) was received.
    Done,
}

// ---------------------------------------------------------------------------
// Received
// ---------------------------------------------------------------------------

/// A typed, decoded protocol event returned by [`StreamReceiver::next`].
///
/// The receiver maps raw [`StreamFrame`] variants to this enum, injecting
/// the validated metadata (job, producer) from `Hello` into the type.
#[derive(Debug)]
pub enum Received {
    /// A batch of symbol entries staged for recording.
    Symbols(Vec<WireEntry>),
    /// A batch of semantic links originating from `from`.
    Links {
        /// The emitting symbol that owns these links.
        from: StableRef,
        /// The links in this batch.
        batch: Vec<WireLink>,
    },
    /// Source file provenance.
    SourceDigest {
        /// Relative path of the source file.
        path: String,
        /// Content hash.
        hash: ContentHash,
        /// Byte length.
        size: u64,
    },
    /// A chunked occurrence section.
    Occurrences(Vec<u8>),
    /// A batch of merged body facts received from the producer.
    ///
    /// Each [`BodyWire`] in the batch is keyed by a content-derived
    /// [`ir::change::IntroId`] (never an arena index) and carries the
    /// merged treesitter+oracle [`ir::body::BodyEmbed`] for that entry.
    /// The host associates these with the corresponding entry staged from a
    /// previous [`Received::Symbols`] batch.
    ///
    /// Body facts do not affect [`StreamReceiver::observed_count`]; they are
    /// an extension slot on the entry, not a new symbol entry.
    Bodies(Vec<BodyWire>),
    /// A progress update.
    Progress {
        /// Running emitted count reported by the producer.
        emitted: u64,
        /// Current producer phase.
        phase: PhaseWire,
    },
    /// The stream completed normally.
    Finish {
        /// Total entries declared by the producer.
        emitted: u64,
        /// Producer output digest.
        producer_digest: ContentHash,
    },
    /// The stream was aborted by the producer.
    Abort {
        /// Failure category.
        failure: FailureKindWire,
        /// Diagnostic message.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// StreamReceiver
// ---------------------------------------------------------------------------

/// Host-side receiver for a single producer stream.
///
/// # Usage
///
/// 1. Call [`StreamReceiver::accept`] to validate the `Hello` frame and
///    retrieve the [`JobKey`] and [`ProducerId`].
/// 2. Call [`StreamReceiver::next`] in a loop until it returns `Ok(None)` or a
///    terminal [`Received::Finish`] / [`Received::Abort`].
///
/// All protocol violations return a typed [`StreamError`]; the receiver does
/// not panic.
pub struct StreamReceiver<R: Read> {
    reader: FrameReader<R>,
    state: ReceiverState,
    /// Number of symbol entries received across all `Symbols` batches.
    observed: u64,
    /// Job key from the `Hello` frame.
    job: Option<JobKey>,
    /// Producer id from the `Hello` frame.
    producer: Option<ProducerId>,
}

impl<R: Read> StreamReceiver<R> {
    /// Wrap a reader. Does not perform any I/O; call [`StreamReceiver::accept`]
    /// next.
    pub fn new(reader: R) -> Self {
        Self {
            reader: FrameReader::new(reader),
            state: ReceiverState::AwaitingHello,
            observed: 0,
            job: None,
            producer: None,
        }
    }

    /// Read and validate the `Hello` frame.
    ///
    /// Returns the [`JobKey`] and [`ProducerId`] from the handshake.
    ///
    /// # Errors
    ///
    /// - [`StreamError::VersionMismatch`] — the remote announced a different version.
    /// - [`StreamError::Protocol`] — the first frame was not `Hello`, or the
    ///   stream ended before `Hello` arrived.
    /// - [`StreamError::Io`] / [`StreamError::Decode`] — transport/encoding errors.
    pub fn accept(&mut self) -> Result<(JobKey, ProducerId), StreamError> {
        if self.state != ReceiverState::AwaitingHello {
            return Err(StreamError::Protocol(
                "accept() called after Hello already received".into(),
            ));
        }

        let frame = self
            .reader
            .read_frame()?
            .ok_or_else(|| StreamError::Protocol("stream ended before Hello".into()))?;

        match frame {
            StreamFrame::Hello {
                version,
                job,
                producer,
            } => {
                if version != IR_STREAM_VERSION {
                    return Err(StreamError::VersionMismatch {
                        ours: IR_STREAM_VERSION,
                        theirs: version,
                    });
                }
                self.state = ReceiverState::Ready;
                self.job = Some(job);
                self.producer = Some(producer.clone());
                Ok((job, producer))
            }
            other => Err(StreamError::Protocol(format!(
                "expected Hello as first frame, got {}",
                other.variant_name()
            ))),
        }
    }

    /// Read the next frame and return a typed [`Received`] event.
    ///
    /// Returns `Ok(None)` on a clean EOF (transport closed after `Finish`/`Abort`).
    ///
    /// # Ordering enforcement
    ///
    /// - [`StreamError::Protocol`] if called before [`StreamReceiver::accept`].
    /// - [`StreamError::Protocol`] if a frame arrives after a terminal.
    /// - [`StreamError::Protocol`] if a `Hello` frame arrives mid-stream.
    /// - [`StreamError::EmittedCountMismatch`] on `Finish` if the declared count
    ///   disagrees with the observed count.
    pub fn recv(&mut self) -> Result<Option<Received>, StreamError> {
        match self.state {
            ReceiverState::AwaitingHello => {
                return Err(StreamError::Protocol(
                    "next() called before accept()".into(),
                ));
            }
            ReceiverState::Done => {
                // The peer must close after a terminal frame. Draining here is
                // what *detects* a violating peer: a clean EOF is the normal
                // end-of-stream, any further frame is a protocol error.
                return match self.reader.read_frame()? {
                    None => Ok(None),
                    Some(frame) => Err(StreamError::Protocol(format!(
                        "{} frame received after terminal frame",
                        frame.variant_name()
                    ))),
                };
            }
            ReceiverState::Ready => {}
        }

        let frame = match self.reader.read_frame()? {
            Some(f) => f,
            None => return Ok(None),
        };

        match frame {
            StreamFrame::Hello { .. } => Err(StreamError::Protocol(
                "unexpected second Hello frame".into(),
            )),

            StreamFrame::Symbols { batch } => {
                self.observed += batch.len() as u64;
                Ok(Some(Received::Symbols(batch)))
            }

            StreamFrame::Links { from, batch } => Ok(Some(Received::Links { from, batch })),

            StreamFrame::SourceDigest { path, hash, size } => {
                Ok(Some(Received::SourceDigest { path, hash, size }))
            }

            StreamFrame::Occurrences { section } => Ok(Some(Received::Occurrences(section))),

            StreamFrame::Bodies { batch } => Ok(Some(Received::Bodies(batch))),

            StreamFrame::Progress { emitted, phase } => {
                Ok(Some(Received::Progress { emitted, phase }))
            }

            StreamFrame::Finish {
                emitted,
                producer_digest,
            } => {
                if emitted != self.observed {
                    return Err(StreamError::EmittedCountMismatch {
                        declared: emitted,
                        observed: self.observed,
                    });
                }
                self.state = ReceiverState::Done;
                Ok(Some(Received::Finish {
                    emitted,
                    producer_digest,
                }))
            }

            StreamFrame::Abort { failure, message } => {
                self.state = ReceiverState::Done;
                Ok(Some(Received::Abort { failure, message }))
            }
        }
    }

    /// The [`JobKey`] from the `Hello` frame, or `None` if [`accept`] has not
    /// been called yet.
    ///
    /// [`accept`]: StreamReceiver::accept
    pub fn job(&self) -> Option<JobKey> {
        self.job
    }

    /// The [`ProducerId`] from the `Hello` frame, or `None` if [`accept`] has
    /// not been called yet.
    ///
    /// [`accept`]: StreamReceiver::accept
    pub fn producer(&self) -> Option<&ProducerId> {
        self.producer.as_ref()
    }

    /// The number of symbol entries observed so far across all `Symbols` batches.
    pub fn observed_count(&self) -> u64 {
        self.observed
    }
}
