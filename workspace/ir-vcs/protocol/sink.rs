//! Producer-side protocol state machine.
//!
//! [`SymbolSink`] is the single handle a producer threads through its lowering
//! pass. It hides all framing details:
//!
//! - Entries are buffered and auto-flushed when the batch count reaches
//!   [`SINK_BATCH_ENTRIES`] or when the encoded batch would approach
//!   [`MAX_FRAME_BYTES`].
//! - [`SymbolSink::emit`] never produces a frame that exceeds the cap: if a
//!   single entry + batch overhead would overflow, the current batch is flushed
//!   first, then the entry is added to a fresh batch.
//! - The sink is consumed (moved) by [`SymbolSink::finish`] or
//!   [`SymbolSink::abort`], making invalid sequences unrepresentable at the
//!   type level after termination.

use std::io::Write;

use heart::content::{ContentHash, JobKey};

use crate::protocol::error::StreamError;
use ir::change::StableRef;

use ir::change::IntroId;

use crate::protocol::frame::{
    BodyWire, FailureKindWire, PhaseWire, ProducerId, StreamFrame, WireEntry, WireLink,
    SINK_BATCH_ENTRIES, MAX_FRAME_BYTES,
};
use crate::protocol::io::FrameWriter;

// ---------------------------------------------------------------------------
// SymbolSink
// ---------------------------------------------------------------------------

/// Producer-side streaming handle for a single compile job.
///
/// # Lifecycle
///
/// 1. [`SymbolSink::hello`] — opens the stream and sends the `Hello` frame.
/// 2. Zero or more calls to [`SymbolSink::emit`], [`SymbolSink::emit_link`],
///    [`SymbolSink::source_digest`], and [`SymbolSink::progress`].
/// 3. Exactly one call to [`SymbolSink::finish`] or [`SymbolSink::abort`].
///
/// After step 3 the `SymbolSink` is consumed (moved); no further calls are
/// possible.
pub struct SymbolSink<W: Write> {
    writer: FrameWriter<W>,
    /// Pending symbol entries not yet flushed.
    pending: Vec<WireEntry>,
    /// Exact postcard-encoded size of the entries in `pending`.
    ///
    /// Postcard encodes `Vec<T>` as a length varint followed by the
    /// concatenation of each element's standalone encoding, so the frame size
    /// is `FRAME_OVERHEAD + pending_bytes` — each entry is encoded once, at
    /// [`SymbolSink::emit`] time, never re-encoded for size probing.
    pending_bytes: usize,
    /// Running count of all entries emitted (across all flushed batches).
    emitted: u64,
}

/// Upper bound on the non-entry bytes of a `Symbols` frame: the enum variant
/// tag (1 varint byte) plus the batch-length varint (≤ 5 bytes for `u32`
/// counts), rounded up for safety margin.
const FRAME_OVERHEAD: usize = 8;

impl<W: Write> SymbolSink<W> {
    /// Open a new stream: wrap `writer`, send the `Hello` frame, and return
    /// the sink ready for use.
    pub fn hello(writer: W, job: JobKey, producer: ProducerId) -> Result<Self, StreamError> {
        let mut fw = FrameWriter::new(writer);
        fw.write_frame(&StreamFrame::Hello {
            version: crate::protocol::IR_STREAM_VERSION,
            job,
            producer,
        })?;
        fw.flush()?;
        Ok(Self { writer: fw, pending: Vec::new(), pending_bytes: 0, emitted: 0 })
    }

    /// Buffer a single [`WireEntry`] for emission.
    ///
    /// If adding this entry to the current batch would cause the encoded frame
    /// to approach [`MAX_FRAME_BYTES`], the current batch is flushed first.
    /// If the batch count reaches [`SINK_BATCH_ENTRIES`], the batch is also
    /// flushed.
    pub fn emit(&mut self, entry: WireEntry) -> Result<(), StreamError> {
        let entry_bytes = postcard::to_allocvec(&entry)
            .map_err(StreamError::Encode)?
            .len();

        // A single entry that cannot fit in any frame is the producer's bug —
        // reject it eagerly instead of at flush time.
        if FRAME_OVERHEAD + entry_bytes > MAX_FRAME_BYTES {
            return Err(StreamError::FrameTooLarge {
                limit: MAX_FRAME_BYTES,
                actual: FRAME_OVERHEAD + entry_bytes,
                variant: Some("Symbols"),
            });
        }

        // Flush first if the count limit is reached or this entry would push
        // the encoded frame over the cap.
        if !self.pending.is_empty()
            && (self.pending.len() >= SINK_BATCH_ENTRIES
                || FRAME_OVERHEAD + self.pending_bytes + entry_bytes > MAX_FRAME_BYTES)
        {
            self.flush_symbols()?;
        }

        self.pending_bytes += entry_bytes;
        self.pending.push(entry);
        Ok(())
    }

    /// Flush a single link as a [`StreamFrame::Links`] frame.
    ///
    /// `from` is the emitting (self) endpoint; `link` describes the other end.
    /// The caller is responsible for splitting large link batches across calls.
    pub fn emit_link(&mut self, from: StableRef, link: WireLink) -> Result<(), StreamError> {
        self.emit_links(from, std::iter::once(link))
    }

    /// Flush a batch of links originating from `from`.
    ///
    /// All links in `batch` must originate from the same symbol (`from`). If
    /// links from multiple symbols need to be emitted, call `emit_links` once
    /// per originating symbol. The encoded frame must not exceed
    /// [`MAX_FRAME_BYTES`]; callers are responsible for splitting large batches.
    pub fn emit_links(
        &mut self,
        from: StableRef,
        links: impl IntoIterator<Item = WireLink>,
    ) -> Result<(), StreamError> {
        let batch: Vec<WireLink> = links.into_iter().collect();
        if batch.is_empty() {
            return Ok(());
        }
        self.writer
            .write_frame(&StreamFrame::Links { from, batch })?;
        Ok(())
    }

    /// Flush a single [`ir::body::BodyEmbed`] as a [`StreamFrame::Bodies`] frame.
    ///
    /// `intro` is the content-derived identity of the owning entry (never an
    /// arena index). This is a convenience shim over [`SymbolSink::emit_bodies`].
    ///
    /// Body facts do not count towards the emitted-symbol total: bodies are an
    /// extension slot on the entry, not a new entry. `self.emitted` is not
    /// incremented.
    pub fn emit_body(
        &mut self,
        intro: IntroId,
        body: ir::body::BodyEmbed,
    ) -> Result<(), StreamError> {
        self.emit_bodies(std::iter::once(BodyWire { intro, body }))
    }

    /// Flush a batch of [`BodyWire`] records as a single [`StreamFrame::Bodies`] frame.
    ///
    /// All records in `bodies` are collected into one frame. The caller is
    /// responsible for keeping the encoded frame under [`MAX_FRAME_BYTES`]; for
    /// very large batches call `emit_bodies` once per chunk. Returns `Ok(())`
    /// immediately if the iterator is empty.
    ///
    /// Body facts do not count towards the emitted-symbol total: bodies are an
    /// extension slot on the entry, not a new entry. `self.emitted` is not
    /// incremented.
    pub fn emit_bodies(
        &mut self,
        bodies: impl IntoIterator<Item = BodyWire>,
    ) -> Result<(), StreamError> {
        let batch: Vec<BodyWire> = bodies.into_iter().collect();
        if batch.is_empty() {
            return Ok(());
        }
        self.writer.write_frame(&StreamFrame::Bodies { batch })?;
        Ok(())
    }

    /// Emit a source file digest.
    pub fn source_digest(
        &mut self,
        path: impl Into<String>,
        hash: ContentHash,
        size: u64,
    ) -> Result<(), StreamError> {
        self.writer.write_frame(&StreamFrame::SourceDigest {
            path: path.into(),
            hash,
            size,
        })
    }

    /// Emit a progress update.
    pub fn progress(&mut self, phase: PhaseWire) -> Result<(), StreamError> {
        let emitted = self.emitted + self.pending.len() as u64;
        self.writer
            .write_frame(&StreamFrame::Progress { emitted, phase })
    }

    /// Convenience shim for unported batch producers.
    ///
    /// Iterates `entries` and calls [`SymbolSink::emit`] for each. This lets
    /// producers that still build an in-memory collection stream it in one call
    /// with zero behavior change; auto-batching still applies.
    pub fn emit_all(
        &mut self,
        entries: impl IntoIterator<Item = WireEntry>,
    ) -> Result<(), StreamError> {
        for entry in entries {
            self.emit(entry)?;
        }
        Ok(())
    }

    /// Flush pending entries, send `Finish`, and consume the sink.
    ///
    /// Returns the total number of entries emitted across all batches.
    pub fn finish(mut self, producer_digest: ContentHash) -> Result<u64, StreamError> {
        // Flush any remaining buffered entries.
        self.flush_symbols()?;
        let emitted = self.emitted;
        self.writer.write_frame(&StreamFrame::Finish {
            emitted,
            producer_digest,
        })?;
        self.writer.flush()?;
        Ok(emitted)
    }

    /// Discard pending entries, send `Abort`, and consume the sink.
    pub fn abort(mut self, failure: FailureKindWire, message: impl Into<String>) -> Result<(), StreamError> {
        // Discard pending entries — do not flush them.
        self.pending.clear();
        self.pending_bytes = 0;
        self.writer.write_frame(&StreamFrame::Abort {
            failure,
            message: message.into(),
        })?;
        self.writer.flush()?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Flush the pending symbol batch as a single [`StreamFrame::Symbols`] frame.
    fn flush_symbols(&mut self) -> Result<(), StreamError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
        let count = batch.len() as u64;
        self.writer
            .write_frame(&StreamFrame::Symbols { batch })?;
        self.emitted += count;
        Ok(())
    }
}
