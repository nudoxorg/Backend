//! Local queues and framed stream endpoint implementations.

use std::{
    borrow::Borrow,
    collections::VecDeque,
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{Frame, TransportLimits};

use super::error::ReplicationError;
use super::message::TransportMessage;

/// A bounded local transport queue carrying the same messages as a remote
/// transport. The queue is FIFO and can be drained after a reconnect.
type QueuedMessage = (Vec<u8>, TransportLimits);

/// In-memory endpoint used for deterministic local/remote transport parity.
///
/// Messages are queued only after passing the canonical byte codec; reconnect
/// therefore exercises exactly the same bytes and decoder as a stream peer.
#[derive(Clone)]
pub struct LocalTransport {
    queue: Arc<Mutex<VecDeque<QueuedMessage>>>,
    connected: Arc<AtomicBool>,
}
impl Default for LocalTransport {
    fn default() -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::new())),
            connected: Arc::new(AtomicBool::new(true)),
        }
    }
}
impl LocalTransport {
    /// Creates an empty local transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Sends a frame through the local transport convenience entry point.
    ///
    /// # Errors
    ///
    /// Returns a size, backpressure, or disconnected error when the frame is
    /// not accepted by the queue.
    pub fn send(&self, frame: Frame, max_messages: usize) -> Result<(), ReplicationError> {
        self.send_message(
            TransportMessage::Chunk(frame),
            max_messages,
            TransportLimits::default(),
        )
    }
    /// Sends a protocol message after checking queue count and frame budget.
    ///
    /// # Errors
    ///
    /// Returns a validation, backpressure, or disconnected error when the
    /// message cannot be queued.
    pub fn send_message<M: Borrow<TransportMessage>>(
        &self,
        message: M,
        max_messages: usize,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(ReplicationError::Disconnected);
        }
        if max_messages == 0 {
            return Err(ReplicationError::MessageTooLarge);
        }
        let encoded = message.borrow().encode(limits)?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?;
        if queue.len() >= max_messages {
            return Err(ReplicationError::Backpressure);
        }
        queue.push_back((encoded, limits));
        Ok(())
    }
    /// Receives the next FIFO protocol message.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Disconnected`] when the queue lock is
    /// poisoned or this endpoint has been disconnected.
    pub fn recv(&self) -> Result<Option<TransportMessage>, ReplicationError> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(ReplicationError::Disconnected);
        }
        let Some((bytes, limits)) = self
            .queue
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?
            .pop_front()
        else {
            return Ok(None);
        };
        TransportMessage::decode(&bytes, limits).map(Some)
    }
    /// Receives the next frame when using the chunk-only view.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::WrongMessage`] when another protocol
    /// message is at the queue head, or disconnected on lock failure.
    pub fn recv_frame(&self) -> Result<Option<Frame>, ReplicationError> {
        match self.recv()? {
            Some(TransportMessage::Chunk(frame)) => Ok(Some(frame)),
            Some(_) => Err(ReplicationError::WrongMessage),
            None => Ok(None),
        }
    }
    /// Returns the number of queued messages.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Disconnected`] when the queue lock is
    /// poisoned or this endpoint has been disconnected.
    pub fn len(&self) -> Result<usize, ReplicationError> {
        if !self.connected.load(Ordering::Acquire) {
            return Err(ReplicationError::Disconnected);
        }
        Ok(self
            .queue
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?
            .len())
    }
    /// Returns whether no messages are queued.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::Disconnected`] when the queue lock is
    /// poisoned or this endpoint has been disconnected.
    pub fn is_empty(&self) -> Result<bool, ReplicationError> {
        Ok(self.len()? == 0)
    }

    /// Marks this endpoint unavailable while retaining queued messages.
    ///
    /// A caller can invoke [`Self::reconnect`] after restoring its underlying
    /// link; the queue then resumes with the same message framing and bounds.
    pub fn disconnect(&self) {
        self.connected.store(false, Ordering::Release);
    }

    /// Reopens an endpoint after a disconnect without discarding queued data.
    pub fn reconnect(&self) {
        self.connected.store(true, Ordering::Release);
    }
}

/// A stream endpoint that applies the same bounded codec as [`LocalTransport`].
///
/// Each message is carried as a four-byte big-endian length followed by one
/// complete versioned replication envelope.  The length is checked before any
/// payload allocation, and a reconnect can replace the underlying stream
/// while the caller retains its durable transfer checkpoint.
pub struct FramedStream<S> {
    stream: S,
    limits: TransportLimits,
    receive: FrameReceive,
}

/// Incremental receive cursor owned by one stream generation.
///
/// A socket timeout is an idle observation, not a frame boundary. Retaining
/// this cursor prevents a timeout between any two bytes from making the next
/// poll interpret a payload byte as a new length prefix. The payload vector
/// is cleared after decode while keeping its allocation for the next frame.
#[derive(Debug, Default)]
struct FrameReceive {
    length: [u8; 4],
    length_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
}

impl FrameReceive {
    fn reset(&mut self) {
        self.length = [0; 4];
        self.length_read = 0;
        self.payload.clear();
        self.payload_read = 0;
    }

    const fn is_empty(&self) -> bool {
        self.length_read == 0 && self.payload_read == 0
    }
}

/// I/O-aware error returned by the shared byte-stream framing kernel.
///
/// The ordinary [`FramedStream::send_message`] and
/// [`FramedStream::recv_message`] methods collapse socket errors to the
/// transport taxonomy. Process listeners can use the I/O-aware methods when
/// they need to distinguish a bounded timeout from a disconnect while still
/// sharing the exact envelope codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FramedStreamError {
    /// Canonical replication validation or decoding failed.
    Replication(ReplicationError),
    /// The underlying byte stream returned this error kind.
    Io(io::ErrorKind),
}

impl<S> FramedStream<S> {
    /// Wraps a byte stream with a negotiated allocation budget.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidLimits`] for an unusable budget.
    pub fn new(stream: S, limits: TransportLimits) -> Result<Self, ReplicationError> {
        limits.validate()?;
        Ok(Self {
            stream,
            limits,
            receive: FrameReceive::default(),
        })
    }

    /// Replaces a disconnected stream without changing the negotiated budget.
    pub fn reconnect(&mut self, stream: S) {
        self.stream = stream;
        self.receive.reset();
    }

    /// Returns the wrapped stream after the endpoint is shut down.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }

    /// Borrows the wrapped stream for transport-specific configuration.
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.stream
    }

    /// Mutably borrows the wrapped stream without exposing framing state.
    #[must_use]
    pub const fn inner_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Returns the negotiated endpoint budget.
    #[must_use]
    pub const fn limits(&self) -> TransportLimits {
        self.limits
    }
}

impl<S: Write> FramedStream<S> {
    /// Sends one canonical encoded message with a bounded outer frame.
    ///
    /// # Errors
    ///
    /// Returns a backpressure, size, or disconnect error when the message
    /// cannot be written.
    pub fn send_message(&mut self, message: &TransportMessage) -> Result<(), ReplicationError> {
        self.send_message_with_io(message).map_err(map_framed_error)
    }

    /// Sends one frame while preserving the underlying I/O error kind.
    ///
    /// # Errors
    ///
    /// Returns a framing or underlying I/O error.
    pub fn send_message_with_io(
        &mut self,
        message: &TransportMessage,
    ) -> Result<(), FramedStreamError> {
        let encoded =
            encode_stream_frame(message, self.limits).map_err(FramedStreamError::Replication)?;
        self.stream
            .write_all(&encoded)
            .and_then(|()| self.stream.flush())
            .map_err(|error| FramedStreamError::Io(error.kind()))
    }
}

impl<S: Read> FramedStream<S> {
    /// Receives one complete outer frame and decodes it through the canonical
    /// message codec.
    ///
    /// # Errors
    ///
    /// Returns a truncation, size, codec, or disconnect error. A malicious
    /// length is rejected before allocating a receive buffer.
    pub fn recv_message(&mut self) -> Result<TransportMessage, ReplicationError> {
        self.recv_message_or_eof()?
            .ok_or(ReplicationError::TruncatedFrame)
    }

    /// Receives one complete outer frame, distinguishing a clean stream EOF
    /// from a truncated frame. This is the shared primitive for process
    /// listeners that need to stop normally when a client disconnects.
    ///
    /// # Errors
    ///
    /// Returns a framing, bounds, or transport error.
    pub fn recv_message_or_eof(&mut self) -> Result<Option<TransportMessage>, ReplicationError> {
        self.recv_message_or_eof_with_io().map_err(map_framed_error)
    }

    /// Receives one frame while preserving the underlying I/O error kind.
    ///
    /// # Errors
    ///
    /// Returns a framing or underlying I/O error.
    pub fn recv_message_or_eof_with_io(
        &mut self,
    ) -> Result<Option<TransportMessage>, FramedStreamError> {
        while self.receive.length_read < self.receive.length.len() {
            match self
                .stream
                .read(&mut self.receive.length[self.receive.length_read..])
            {
                Ok(0) if self.receive.is_empty() => return Ok(None),
                Ok(0) => {
                    self.receive.reset();
                    return Err(FramedStreamError::Replication(
                        ReplicationError::TruncatedFrame,
                    ));
                }
                Ok(read) => {
                    self.receive.length_read = self.receive.length_read.saturating_add(read);
                }
                Err(error) => return Err(FramedStreamError::Io(error.kind())),
            }
        }

        let length = usize::try_from(u32::from_be_bytes(self.receive.length))
            .map_err(|_| FramedStreamError::Replication(ReplicationError::Overflow))?;
        if length < 10 || length > self.limits.max_frame {
            self.receive.reset();
            return Err(FramedStreamError::Replication(
                ReplicationError::MessageTooLarge,
            ));
        }
        if self.receive.payload.len() != length {
            self.receive.payload.resize(length, 0);
        }
        while self.receive.payload_read < length {
            match self
                .stream
                .read(&mut self.receive.payload[self.receive.payload_read..])
            {
                Ok(0) => {
                    self.receive.reset();
                    return Err(FramedStreamError::Replication(
                        ReplicationError::TruncatedFrame,
                    ));
                }
                Ok(read) => {
                    self.receive.payload_read = self.receive.payload_read.saturating_add(read);
                }
                Err(error) => return Err(FramedStreamError::Io(error.kind())),
            }
        }
        let decoded = TransportMessage::decode(&self.receive.payload, self.limits)
            .map(Some)
            .map_err(FramedStreamError::Replication);
        self.receive.reset();
        decoded
    }
}

fn map_framed_error(error: FramedStreamError) -> ReplicationError {
    match error {
        FramedStreamError::Replication(error) => error,
        FramedStreamError::Io(io::ErrorKind::UnexpectedEof) => ReplicationError::TruncatedFrame,
        FramedStreamError::Io(_) => ReplicationError::Disconnected,
    }
}

/// Encodes one bounded replication message with the canonical four-byte outer
/// stream length prefix. Process adapters use this helper when they need an
/// in-memory frame for a bounded handoff or test socket.
///
/// # Errors
///
/// Returns an encoding, size, or limit error.
pub fn encode_stream_frame(
    message: &TransportMessage,
    limits: TransportLimits,
) -> Result<Vec<u8>, ReplicationError> {
    let encoded = message.encode(limits)?;
    let length = u32::try_from(encoded.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
    let capacity = 4usize
        .checked_add(encoded.len())
        .ok_or(ReplicationError::Overflow)?;
    let mut frame = Vec::with_capacity(capacity);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&encoded);
    Ok(frame)
}

/// A transport abstraction shared by local and stream-backed endpoints.
pub trait TransportEndpoint {
    /// Sends one message under the endpoint's negotiated limits.
    ///
    /// # Errors
    ///
    /// Returns a size, validation, or disconnect error when the message cannot
    /// be sent.
    fn send_message(&mut self, message: &TransportMessage) -> Result<(), ReplicationError>;
    /// Receives one message, preserving the wire-claim boundary.
    ///
    /// # Errors
    ///
    /// Returns a truncation, codec, size, or disconnect error when a complete
    /// message cannot be read.
    fn recv_message(&mut self) -> Result<TransportMessage, ReplicationError>;
}

impl<S: Read + Write> TransportEndpoint for FramedStream<S> {
    fn send_message(&mut self, message: &TransportMessage) -> Result<(), ReplicationError> {
        Self::send_message(self, message)
    }

    fn recv_message(&mut self) -> Result<TransportMessage, ReplicationError> {
        Self::recv_message(self)
    }
}
