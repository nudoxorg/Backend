//! Bounded remote transport adapters and cancellation propagation.

use backend_execution::{AttemptLease, ScheduleRequest};
use backend_replication::{
    CancelAttempt, CancellationId, ReplicationError, TransportLimits, TransportMessage,
};
use std::collections::BTreeSet;
use std::fmt;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

const MAX_CANCELLED_IDENTITIES: usize = 4096;

fn remember_cancelled(set: &mut BTreeSet<CancellationId>, cancellation: CancellationId) {
    if set.contains(&cancellation) {
        return;
    }
    if set.len() >= MAX_CANCELLED_IDENTITIES
        && let Some(oldest) = set.iter().next().copied()
    {
        set.remove(&oldest);
    }
    set.insert(cancellation);
}

/// A checked remote transport seam. The dispatcher never calls a worker
/// endpoint directly; adapters send a negotiated `TransportMessage` and then
/// pass the returned wire result through `admit_remote`.
pub trait RemoteTransport: Send {
    /// Monotonic connection identity. Implementations increment it after a
    /// reconnect so the daemon renegotiates the new peer session exactly once.
    fn connection_id(&self) -> u64 {
        0
    }
    /// Negotiates capabilities before a request can be sent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn negotiate(
        &mut self,
        local: &backend_replication::CapabilityManifest,
        limits: TransportLimits,
    ) -> Result<backend_replication::NegotiatedCapabilities, ReplicationError>;
    /// Sends one bounded protocol message.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn send(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError>;
    /// Receives the next protocol message.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn recv(&mut self) -> Result<Option<TransportMessage>, ReplicationError>;
    /// Propagates scheduler cancellation over the transport.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn cancel(&mut self, cancellation: CancelAttempt) -> Result<(), ReplicationError>;
}

/// Production stream adapter for the same encoded replication protocol used
/// by loopback tests. It performs capability negotiation before requests and
/// preserves cancellation identities locally so an adapter can fence a late
/// worker result even when the peer transport has no cancellation frame.
///
/// For TCP, the stream type must be
/// [`crate::AuthenticatedTcpStream`]. A successful authority handshake alone
/// does not authenticate later replication bytes; the TCP record wrapper is
/// what supplies transcript-bound integrity and sequence checks.
pub struct StreamTransport<S> {
    stream: backend_replication::FramedStream<S>,
    peer: Option<backend_replication::CapabilityManifest>,
    cancelled: BTreeSet<CancellationId>,
    connection: u64,
}

impl<S> fmt::Debug for StreamTransport<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamTransport")
            .field("negotiated", &self.peer.is_some())
            .field("cancelled", &self.cancelled.len())
            .finish_non_exhaustive()
    }
}

impl<S> StreamTransport<S> {
    /// Wraps a connected byte stream with bounded replication framing.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new(stream: S, limits: TransportLimits) -> Result<Self, ReplicationError> {
        Ok(Self {
            stream: backend_replication::FramedStream::new(stream, limits)?,
            peer: None,
            cancelled: BTreeSet::new(),
            connection: 1,
        })
    }

    /// Returns whether a cancellation was observed for one exact request.
    #[must_use]
    pub fn is_cancelled(&self, cancellation: CancellationId) -> bool {
        self.cancelled.contains(&cancellation)
    }

    /// Replaces the connected stream after an outage while retaining the
    /// negotiated limits and cancellation set.
    pub fn reconnect(&mut self, stream: S) {
        self.stream.reconnect(stream);
        self.peer = None;
        self.connection = self.connection.saturating_add(1);
    }
}

impl<S: Read + Write + Send> RemoteTransport for StreamTransport<S> {
    fn connection_id(&self) -> u64 {
        self.connection
    }

    fn negotiate(
        &mut self,
        local: &backend_replication::CapabilityManifest,
        limits: TransportLimits,
    ) -> Result<backend_replication::NegotiatedCapabilities, ReplicationError> {
        self.stream
            .send_message(&TransportMessage::Capabilities(local.clone()))?;
        let peer = self.stream.recv_message()?;
        let TransportMessage::Capabilities(peer) = peer else {
            return Err(ReplicationError::WrongMessage);
        };
        let negotiated = local.negotiate(&peer, limits)?;
        self.peer = Some(peer);
        Ok(negotiated)
    }

    fn send(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        if message.estimated_size() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.stream.send_message(&message)
    }

    fn recv(&mut self) -> Result<Option<TransportMessage>, ReplicationError> {
        match self.stream.recv_message_or_eof_with_io() {
            Ok(Some(message)) => Ok(Some(message)),
            Ok(None) => Err(ReplicationError::Disconnected),
            Err(backend_replication::FramedStreamError::Io(
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut,
            )) => Ok(None),
            Err(backend_replication::FramedStreamError::Io(std::io::ErrorKind::UnexpectedEof)) => {
                Err(ReplicationError::TruncatedFrame)
            }
            Err(backend_replication::FramedStreamError::Io(_)) => {
                Err(ReplicationError::Disconnected)
            }
            Err(backend_replication::FramedStreamError::Replication(error)) => Err(error),
        }
    }

    fn cancel(&mut self, cancellation: CancelAttempt) -> Result<(), ReplicationError> {
        remember_cancelled(&mut self.cancelled, cancellation.cancellation);
        self.stream
            .send_message(&TransportMessage::CancelAttempt(cancellation))?;
        Ok(())
    }
}

/// In-process loopback transport with the same bounded replication message
/// path and outage semantics as a stream adapter.
pub struct LoopbackTransport {
    outbound: backend_replication::LocalTransport,
    inbound: backend_replication::LocalTransport,
    peer_capabilities: Option<backend_replication::CapabilityManifest>,
    max_messages: usize,
    cancelled: Arc<Mutex<BTreeSet<CancellationId>>>,
    connection: u64,
}

impl fmt::Debug for LoopbackTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoopbackTransport")
            .field("max_messages", &self.max_messages)
            .finish_non_exhaustive()
    }
}

impl LoopbackTransport {
    /// Creates two connected endpoints backed by bounded replication queues.
    #[must_use]
    pub fn pair(max_messages: usize) -> (Self, Self) {
        let left_to_right = backend_replication::LocalTransport::new();
        let right_to_left = backend_replication::LocalTransport::new();
        let cancelled = Arc::new(Mutex::new(BTreeSet::new()));
        (
            Self {
                outbound: left_to_right.clone(),
                inbound: right_to_left.clone(),
                peer_capabilities: None,
                max_messages,
                cancelled: Arc::clone(&cancelled),
                connection: 1,
            },
            Self {
                outbound: right_to_left,
                inbound: left_to_right,
                peer_capabilities: None,
                max_messages,
                cancelled,
                connection: 1,
            },
        )
    }

    /// Installs the peer capability advertisement used by negotiate.
    pub fn set_peer_capabilities(&mut self, capabilities: backend_replication::CapabilityManifest) {
        self.peer_capabilities = Some(capabilities);
    }

    /// Simulates an outage while preserving queued frames for reconnect.
    pub fn disconnect(&self) {
        self.outbound.disconnect();
    }

    /// Restores a disconnected endpoint.
    pub fn reconnect(&mut self) {
        self.outbound.reconnect();
        self.connection = self.connection.saturating_add(1);
    }

    /// Returns whether the connected peer cancelled this exact request.
    #[must_use]
    pub fn is_cancelled(&self, cancellation: CancellationId) -> bool {
        self.cancelled
            .lock()
            .map_or(true, |values| values.contains(&cancellation))
    }
}

impl RemoteTransport for LoopbackTransport {
    fn connection_id(&self) -> u64 {
        self.connection
    }
    fn negotiate(
        &mut self,
        local: &backend_replication::CapabilityManifest,
        limits: TransportLimits,
    ) -> Result<backend_replication::NegotiatedCapabilities, ReplicationError> {
        local.negotiate(
            self.peer_capabilities
                .as_ref()
                .ok_or(ReplicationError::Disconnected)?,
            limits,
        )
    }

    fn send(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        self.outbound
            .send_message(message, self.max_messages, limits)
    }

    fn recv(&mut self) -> Result<Option<TransportMessage>, ReplicationError> {
        self.inbound.recv()
    }

    fn cancel(&mut self, cancellation: CancelAttempt) -> Result<(), ReplicationError> {
        let mut cancelled = self
            .cancelled
            .lock()
            .map_err(|_| ReplicationError::Disconnected)?;
        remember_cancelled(&mut cancelled, cancellation.cancellation);
        self.outbound.send_message(
            TransportMessage::CancelAttempt(cancellation),
            self.max_messages,
            TransportLimits::default(),
        )
    }
}

/// Convenience alias retaining the lower scheduler's request type.
pub type EngineScheduleRequest<R> = ScheduleRequest<R>;

/// Exposes the lower lease type without introducing a second lease identity.
pub type EngineAttemptLease = AttemptLease;
