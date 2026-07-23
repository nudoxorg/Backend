//! Error type for the `ir-stream` protocol.

use thiserror::Error;

/// The unified error type for all `ir-stream` operations.
///
/// Each variant is precise enough for the caller to act on without inspecting
/// a nested error string.
#[derive(Debug, Error)]
pub enum StreamError {
    /// An underlying I/O error on the transport.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Postcard encoding failed (produced a frame that could not be serialized).
    #[error("frame encode error: {0}")]
    Encode(postcard::Error),

    /// Postcard decoding failed (received a malformed frame).
    #[error("frame decode error: {0}")]
    Decode(postcard::Error),

    /// A frame exceeded [`crate::protocol::MAX_FRAME_BYTES`].
    ///
    /// On the write path, `variant` names the [`crate::protocol::StreamFrame`] variant
    /// that overflowed. On the read path, `variant` is `None` because the
    /// length header is read before the frame is decoded.
    #[error("frame too large: {actual} bytes (limit {limit}); variant: {variant:?}")]
    FrameTooLarge {
        /// The configured maximum frame size.
        limit: usize,
        /// The actual (or declared) size that exceeded the limit.
        actual: usize,
        /// Which `StreamFrame` variant caused the overflow, if known.
        variant: Option<&'static str>,
    },

    /// The remote peer announced a protocol version we do not support.
    #[error("protocol version mismatch: ours={ours}, theirs={theirs}")]
    VersionMismatch {
        /// The version this endpoint supports.
        ours: u32,
        /// The version announced by the remote.
        theirs: u32,
    },

    /// A frame arrived out of the expected protocol sequence.
    ///
    /// Examples: two `Hello` frames, a frame after `Finish`/`Abort`, or a
    /// non-`Hello` frame before `Hello` has been received.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// The `Finish` frame declared a different emitted count than the sum of
    /// `Symbols` batch lengths observed by the receiver.
    #[error("emitted count mismatch: declared={declared}, observed={observed}")]
    EmittedCountMismatch {
        /// The count declared in the `Finish` frame.
        declared: u64,
        /// The count actually observed by the receiver.
        observed: u64,
    },
}
