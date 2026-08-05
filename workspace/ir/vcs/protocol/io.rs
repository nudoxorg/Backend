//! Low-level length-prefixed frame I/O for the `ir-stream` protocol.
//!
//! [`FrameWriter`] and [`FrameReader`] are generic over `std::io::Write` /
//! `std::io::Read` so they work over any byte transport — vsock Unix sockets,
//! in-memory `Vec<u8>` pipes, or test `UnixStream` pairs.
//!
//! # Wire format
//!
//! ```text
//! [length: u32 LE][postcard bytes of length `length`]
//! ```
//!
//! Both endpoints refuse frames larger than [`MAX_FRAME_BYTES`]:
//! - The writer returns [`StreamError::FrameTooLarge`] before writing anything.
//! - The reader returns [`StreamError::FrameTooLarge`] after reading the length
//!   header but *before* allocating the body — so an adversarial peer cannot
//!   cause an out-of-memory condition.

use std::io::{Read, Write};

use crate::protocol::error::StreamError;
use crate::protocol::frame::{MAX_FRAME_BYTES, StreamFrame};

// ---------------------------------------------------------------------------
// FrameWriter
// ---------------------------------------------------------------------------

/// Writes length-prefixed postcard frames to an underlying [`Write`] transport.
///
/// The writer is intentionally minimal — it does not buffer or state-track.
/// Higher-level state machines ([`crate::protocol::SymbolSink`]) own the write path.
pub struct FrameWriter<W: Write> {
    inner: W,
}

impl<W: Write> FrameWriter<W> {
    /// Wrap a writer. Does not perform any I/O.
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Encode `frame` with postcard and write the length-prefixed bytes.
    ///
    /// Returns [`StreamError::FrameTooLarge`] if the encoded frame exceeds
    /// [`MAX_FRAME_BYTES`] — the transport is not written to in that case.
    /// Returns [`StreamError::Encode`] if postcard serialization fails.
    pub fn write_frame(&mut self, frame: &StreamFrame) -> Result<(), StreamError> {
        let variant = frame.variant_name();
        let bytes = postcard::to_allocvec(frame).map_err(StreamError::Encode)?;

        let len = bytes.len();
        if len > MAX_FRAME_BYTES {
            return Err(StreamError::FrameTooLarge {
                limit: MAX_FRAME_BYTES,
                actual: len,
                variant: Some(variant),
            });
        }

        // Write the 4-byte LE length prefix.
        let len_bytes = (len as u32).to_le_bytes();
        self.inner.write_all(&len_bytes).map_err(StreamError::Io)?;
        // Write the postcard body.
        self.inner.write_all(&bytes).map_err(StreamError::Io)?;
        Ok(())
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> Result<(), StreamError> {
        self.inner.flush().map_err(StreamError::Io)
    }

    /// Consume the writer, returning the underlying transport.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

// ---------------------------------------------------------------------------
// FrameReader
// ---------------------------------------------------------------------------

/// Reads length-prefixed postcard frames from an underlying [`Read`] transport.
///
/// The reader validates the declared frame length against [`MAX_FRAME_BYTES`]
/// *before* allocating the body buffer, so a malicious peer cannot force an
/// allocation of arbitrary size.
///
/// Truncated streams (EOF mid-frame) surface as [`StreamError::Io`] wrapping
/// [`std::io::ErrorKind::UnexpectedEof`] — never as a panic.
pub struct FrameReader<R: Read> {
    inner: R,
}

impl<R: Read> FrameReader<R> {
    /// Wrap a reader. Does not perform any I/O.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Read the next frame, blocking until it is fully available.
    ///
    /// Returns:
    /// - `Ok(Some(frame))` — a complete frame was read.
    /// - `Ok(None)` — clean EOF before any byte of a frame was read (the peer
    ///   closed the transport after writing all frames normally).
    /// - `Err(StreamError::FrameTooLarge)` — the declared length header exceeded
    ///   [`MAX_FRAME_BYTES`]; no body bytes were read.
    /// - `Err(StreamError::Io(_))` — including `UnexpectedEof` for truncated
    ///   streams.
    /// - `Err(StreamError::Decode(_))` — postcard decoding failed.
    pub fn read_frame(&mut self) -> Result<Option<StreamFrame>, StreamError> {
        // Read the 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        match read_exact_or_eof(&mut self.inner, &mut len_buf)? {
            ReadResult::Eof => return Ok(None),
            ReadResult::Ok => {}
        }
        let len = u32::from_le_bytes(len_buf) as usize;

        // Validate declared length *before* allocating.
        if len > MAX_FRAME_BYTES {
            return Err(StreamError::FrameTooLarge {
                limit: MAX_FRAME_BYTES,
                actual: len,
                variant: None,
            });
        }

        // Allocate and fill the body.
        let mut body = vec![0u8; len];
        self.inner.read_exact(&mut body).map_err(StreamError::Io)?;

        let frame: StreamFrame = postcard::from_bytes(&body).map_err(StreamError::Decode)?;
        Ok(Some(frame))
    }

    /// Consume the reader, returning the underlying transport.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

enum ReadResult {
    Ok,
    Eof,
}

/// Like `read_exact` but returns `Eof` on a clean EOF before any bytes, instead
/// of treating it as `UnexpectedEof`.
fn read_exact_or_eof<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<ReadResult, StreamError> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(ReadResult::Eof);
                }
                // Mid-frame EOF — that is unexpected.
                return Err(StreamError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended mid-frame length header",
                )));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(StreamError::Io(e)),
        }
    }
    Ok(ReadResult::Ok)
}
