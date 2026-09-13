//! Canonical worker stream framing.
//!
//! Worker payloads are always `backend-replication` messages. This module adds
//! only the bounded outer stream frame needed for a byte stream; it never
//! serializes an execution request a second time or accepts ad-hoc JSON.

use backend_engine::{
    FramedStream, FramedStreamError, ReplicationError, TransportLimits, TransportMessage,
};
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

impl From<backend_engine::WorkerError> for WorkerProtocolError {
    fn from(error: backend_engine::WorkerError) -> Self {
        Self::Rejected(error.to_string())
    }
}

/// Bounded worker stream policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerLimits {
    /// Canonical replication limits.
    pub transport: TransportLimits,
    /// Maximum complete request frames on one connection.
    pub max_frames_per_connection: usize,
    /// I/O timeout used by Unix and stdio integrations.
    pub io_timeout: Duration,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            transport: TransportLimits::default(),
            max_frames_per_connection: 256,
            io_timeout: Duration::from_secs(30),
        }
    }
}

impl WorkerLimits {
    /// Checks all negotiated and process limits before a stream is opened.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn validate(self) -> Result<(), WorkerProtocolError> {
        self.transport
            .validate()
            .map_err(WorkerProtocolError::Replication)?;
        if self.max_frames_per_connection == 0 || self.io_timeout.is_zero() {
            return Err(WorkerProtocolError::InvalidLimits);
        }
        Ok(())
    }
}

/// Worker stream failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerProtocolError {
    /// Local process limits are inconsistent.
    InvalidLimits,
    /// The stream ended before a complete outer frame arrived.
    Truncated,
    /// The outer frame exceeds the negotiated maximum.
    FrameTooLarge,
    /// The canonical replication codec rejected a message.
    Replication(ReplicationError),
    /// Stream I/O failed.
    Io(io::ErrorKind),
    /// The peer exceeded its time budget while sending or receiving.
    Timeout,
    /// The endpoint was closed.
    Closed,
    /// A worker request was rejected because no registered recipe/admission
    /// capability could accept it.
    Rejected(String),
    /// The bounded worker is already handling its maximum in-flight work.
    Backpressure,
}

impl fmt::Display for WorkerProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str("invalid worker limits"),
            Self::Truncated => formatter.write_str("truncated worker frame"),
            Self::FrameTooLarge => formatter.write_str("worker frame exceeds its bound"),
            Self::Replication(error) => error.fmt(formatter),
            Self::Io(kind) => write!(formatter, "worker stream I/O failed: {kind:?}"),
            Self::Timeout => formatter.write_str("worker stream timed out"),
            Self::Closed => formatter.write_str("worker endpoint is closed"),
            Self::Rejected(message) => write!(formatter, "worker request rejected: {message}"),
            Self::Backpressure => formatter.write_str("worker backpressure"),
        }
    }
}

impl std::error::Error for WorkerProtocolError {}

/// Stateful worker framing capability.
///
/// One instance owns the incremental receive cursor for a connection. A
/// timeout therefore suspends the current frame instead of turning the next
/// payload byte into a new header, and its receive allocation is reused for
/// subsequent messages.
pub struct WorkerFramed<S> {
    stream: FramedStream<S>,
}

impl<S> WorkerFramed<S> {
    /// Wraps one worker stream under the canonical replication bounds.
    /// # Errors
    /// Returns an error for invalid worker or transport limits.
    pub fn new(stream: S, limits: WorkerLimits) -> Result<Self, WorkerProtocolError> {
        limits.validate()?;
        Ok(Self {
            stream: FramedStream::new(stream, limits.transport).map_err(map_replication_error)?,
        })
    }

    /// Borrows the underlying stream for deadline configuration.
    #[must_use]
    pub const fn inner(&self) -> &S {
        self.stream.inner()
    }
}

impl<S: Read> WorkerFramed<S> {
    /// Reads the next message while retaining partial-frame state on timeout.
    /// # Errors
    /// Returns a mapped I/O, bounds, or canonical replication error.
    pub fn read_message(&mut self) -> Result<TransportMessage, WorkerProtocolError> {
        self.stream
            .recv_message_or_eof_with_io()
            .map_err(map_framed_error)?
            .ok_or(WorkerProtocolError::Closed)
    }
}

impl<S: Write> WorkerFramed<S> {
    /// Writes one canonical bounded replication message.
    /// # Errors
    /// Returns a mapped I/O, bounds, or canonical replication error.
    pub fn write_message(&mut self, message: &TransportMessage) -> Result<(), WorkerProtocolError> {
        self.stream
            .send_message_with_io(message)
            .map_err(map_framed_error)
    }
}

/// Reads one complete outer frame and decodes it through the canonical
/// replication codec.
/// # Errors
/// Returns an error when the input is invalid or durable state cannot be accessed.
pub fn read_message(
    reader: &mut impl Read,
    limits: WorkerLimits,
) -> Result<TransportMessage, WorkerProtocolError> {
    WorkerFramed::new(reader, limits)?.read_message()
}

/// Encodes and writes one canonical replication message with a bounded outer
/// frame.
/// # Errors
/// Returns an error when the input is invalid or durable state cannot be accessed.
pub fn write_message(
    writer: &mut impl Write,
    message: &TransportMessage,
    limits: WorkerLimits,
) -> Result<(), WorkerProtocolError> {
    WorkerFramed::new(writer, limits)?.write_message(message)
}

fn map_framed_error(error: FramedStreamError) -> WorkerProtocolError {
    match error {
        FramedStreamError::Replication(error) => map_replication_error(error),
        FramedStreamError::Io(kind) => map_io(kind),
    }
}

fn map_io(kind: io::ErrorKind) -> WorkerProtocolError {
    match kind {
        io::ErrorKind::UnexpectedEof => WorkerProtocolError::Truncated,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => WorkerProtocolError::Timeout,
        io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::NotConnected => WorkerProtocolError::Closed,
        other => WorkerProtocolError::Io(other),
    }
}

/// Encodes a complete canonical frame in memory.
/// # Errors
/// Returns an error when the input is invalid or durable state cannot be accessed.
pub fn frame(
    message: &TransportMessage,
    limits: WorkerLimits,
) -> Result<Vec<u8>, WorkerProtocolError> {
    limits.validate()?;
    backend_engine::encode_stream_frame(message, limits.transport).map_err(map_replication_error)
}

fn map_replication_error(error: ReplicationError) -> WorkerProtocolError {
    match error {
        ReplicationError::TruncatedFrame => WorkerProtocolError::Truncated,
        ReplicationError::MessageTooLarge
        | ReplicationError::ChunkTooLarge
        | ReplicationError::ObjectTooLarge
        | ReplicationError::Overflow => WorkerProtocolError::FrameTooLarge,
        ReplicationError::Disconnected => WorkerProtocolError::Closed,
        other => WorkerProtocolError::Replication(other),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_engine::{SchemaDescriptor, VersionRange};
    use std::io::Cursor;

    struct InterruptedFrame {
        bytes: Cursor<Vec<u8>>,
        reads: usize,
    }

    impl Read for InterruptedFrame {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let call = self.reads;
            self.reads = self.reads.saturating_add(1);
            if matches!(call, 1 | 3) {
                return Err(io::Error::from(io::ErrorKind::TimedOut));
            }
            let bounded = output.len().min(2);
            self.bytes.read(&mut output[..bounded])
        }
    }

    impl Write for InterruptedFrame {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn limits() -> WorkerLimits {
        WorkerLimits {
            transport: TransportLimits {
                max_frame: 4096,
                max_chunk: 4096,
                ..TransportLimits::default()
            },
            max_frames_per_connection: 4,
            io_timeout: Duration::from_millis(10),
        }
    }

    #[test]
    fn canonical_capability_message_round_trips_through_outer_frame() {
        let limits = limits();
        let message = TransportMessage::Capabilities(backend_engine::CapabilityManifest {
            protocol: VersionRange { min: 1, max: 1 },
            schemas: vec![SchemaDescriptor {
                domain: 1,
                type_id: 1,
                versions: VersionRange { min: 1, max: 1 },
            }],
            recipes: Vec::new(),
            max_object: 1024,
            max_chunk: 512,
            max_frame: 4096,
            max_ranges: 4,
            max_resources: backend_engine::ResourceEnvelope::UNBOUNDED,
        });
        let bytes = frame(&message, limits).expect("frame");
        let mut stream = Cursor::new(bytes);
        let decoded = read_message(&mut stream, limits).expect("decode");
        assert_eq!(decoded, message);
    }

    #[test]
    fn partial_and_oversized_frames_fail_before_large_allocation() {
        let limits = limits();
        let mut partial = Cursor::new([0_u8, 0, 0, 10, b'R']);
        assert_eq!(
            read_message(&mut partial, limits),
            Err(WorkerProtocolError::Truncated)
        );
        let mut closed = Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_message(&mut closed, limits),
            Err(WorkerProtocolError::Closed)
        );
        let mut oversized = Cursor::new([0xff_u8; 4]);
        assert_eq!(
            read_message(&mut oversized, limits),
            Err(WorkerProtocolError::FrameTooLarge)
        );
    }

    #[test]
    fn polling_worker_stream_resumes_one_frame_after_header_and_payload_timeouts() {
        let limits = limits();
        let message = TransportMessage::Capabilities(backend_engine::CapabilityManifest {
            protocol: VersionRange { min: 1, max: 1 },
            schemas: Vec::new(),
            recipes: Vec::new(),
            max_object: 1024,
            max_chunk: 512,
            max_frame: 4096,
            max_ranges: 4,
            max_resources: backend_engine::ResourceEnvelope::UNBOUNDED,
        });
        let stream = InterruptedFrame {
            bytes: Cursor::new(frame(&message, limits).expect("frame")),
            reads: 0,
        };
        let mut stream = WorkerFramed::new(stream, limits).expect("worker stream");
        assert_eq!(stream.read_message(), Err(WorkerProtocolError::Timeout));
        assert_eq!(stream.read_message(), Err(WorkerProtocolError::Timeout));
        assert_eq!(stream.read_message(), Ok(message));
    }
}
