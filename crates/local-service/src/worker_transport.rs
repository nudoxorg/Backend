//! Daemon-owned worker socket transport.
//!
//! A recipe dispatch must never make the locald owner wait on a worker socket.
//! The transport therefore separates the byte stream from the owner loop:
//! one bounded reader publishes canonical replication messages into an inbox,
//! while a bounded writer consumes outbound messages.  `RemoteTransport::recv`
//! is a poll and never waits for a frame; the owner can continue serving local
//! commands, subscriptions, and fallback work while a remote attempt runs.

use backend_engine::{
    AuthenticatedTcpStream, CapabilityManifest, FramedStream, NegotiatedCapabilities,
    RemoteTransport, ReplicationError, TcpAuthority, TransportLimits, TransportMessage,
};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;
use std::time::Duration;

const COMMAND_CAPACITY: usize = 64;
const INBOX_CAPACITY: usize = 64;
static NEXT_CONNECTION_ID: AtomicU64 = AtomicU64::new(1);

/// Connected stream kinds accepted by the locald worker connector.
#[derive(Debug)]
pub(crate) enum WorkerStream {
    #[cfg(any(unix, windows))]
    Unix(backend_engine::LocalStream),
    AuthenticatedTcp(AuthenticatedTcpStream<std::net::TcpStream>),
}

impl WorkerStream {
    fn set_timeouts(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Unix(stream) => stream
                .set_read_timeout(timeout)
                .and_then(|()| stream.set_write_timeout(timeout)),
            Self::AuthenticatedTcp(stream) => stream
                .inner()
                .set_read_timeout(timeout)
                .and_then(|()| stream.inner().set_write_timeout(timeout)),
        }
    }
}

enum WorkerWake {
    #[cfg(any(unix, windows))]
    Unix(backend_engine::LocalStream),
    Tcp(std::net::TcpStream),
}

impl WorkerWake {
    fn shutdown(self) {
        let result = match self {
            #[cfg(any(unix, windows))]
            Self::Unix(stream) => stream.shutdown(std::net::Shutdown::Both),
            Self::Tcp(stream) => stream.shutdown(std::net::Shutdown::Both),
        };
        let _ = result;
    }
}

impl Read for WorkerStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Unix(stream) => stream.read(bytes),
            Self::AuthenticatedTcp(stream) => stream.read(bytes),
        }
    }
}

impl Write for WorkerStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Unix(stream) => stream.write(bytes),
            Self::AuthenticatedTcp(stream) => stream.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(any(unix, windows))]
            Self::Unix(stream) => stream.flush(),
            Self::AuthenticatedTcp(stream) => stream.flush(),
        }
    }
}

enum Outbound {
    Message(Box<TransportMessage>),
    Shutdown,
}

/// A nonblocking owner-facing transport backed by one authenticated worker
/// stream. The stream itself is never shared with the owner thread.
pub(crate) struct AsyncWorkerTransport {
    commands: SyncSender<Outbound>,
    inbox: Receiver<Result<TransportMessage, ReplicationError>>,
    peer: Option<CapabilityManifest>,
    connection: u64,
    limits: TransportLimits,
    wake: Option<WorkerWake>,
}

impl fmt::Debug for AsyncWorkerTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncWorkerTransport")
            .field("negotiated", &self.peer.is_some())
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl AsyncWorkerTransport {
    /// Splits an already authenticated stream into bounded reader/writer
    /// tasks. Handshake authentication is intentionally performed by the
    /// connector before this method, so no unauthenticated bytes can enter
    /// the canonical replication inbox.
    pub(crate) fn new(
        stream: WorkerStream,
        local: &CapabilityManifest,
        limits: TransportLimits,
        timeout: Duration,
    ) -> Result<Self, ReplicationError> {
        limits.validate()?;
        if timeout.is_zero() {
            return Err(ReplicationError::InvalidLimits);
        }
        // Complete capability negotiation while the connector is still off
        // the owner loop. Once this constructor returns, all owner-facing
        // operations are queue/poll operations and cannot wait on a socket.
        stream
            .set_timeouts(Some(timeout))
            .map_err(|_| ReplicationError::Disconnected)?;
        let mut handshake = FramedStream::new(stream, limits)?;
        handshake.send_message(&TransportMessage::Capabilities(local.clone()))?;
        let peer_message = handshake.recv_message()?;
        let TransportMessage::Capabilities(peer) = peer_message else {
            return Err(ReplicationError::WrongMessage);
        };
        let _negotiated = local.negotiate(&peer, limits)?;
        let stream = handshake.into_inner();
        stream
            .set_timeouts(None)
            .map_err(|_| ReplicationError::Disconnected)?;
        let (commands, command_rx) = mpsc::sync_channel(COMMAND_CAPACITY);
        let (inbox_tx, inbox) = mpsc::sync_channel(INBOX_CAPACITY);
        let wake = match stream {
            #[cfg(any(unix, windows))]
            WorkerStream::Unix(stream) => {
                let wake = stream
                    .try_clone()
                    .map(WorkerWake::Unix)
                    .map_err(|_| ReplicationError::Disconnected)?;
                let reader_stream = stream
                    .try_clone()
                    .map_err(|_| ReplicationError::Disconnected)?;
                spawn_reader(reader_stream, limits, inbox_tx.clone());
                spawn_writer(stream, limits, command_rx, inbox_tx.clone());
                wake
            }
            WorkerStream::AuthenticatedTcp(stream) => {
                let wake = stream
                    .inner()
                    .try_clone()
                    .map(WorkerWake::Tcp)
                    .map_err(|_| ReplicationError::Disconnected)?;
                let (reader_stream, writer_stream) = stream
                    .try_split_with(|inner| Ok((inner.try_clone()?, inner.try_clone()?)))
                    .map_err(|_| ReplicationError::Disconnected)?;
                spawn_reader(reader_stream, limits, inbox_tx.clone());
                spawn_writer(writer_stream, limits, command_rx, inbox_tx.clone());
                wake
            }
        };
        let connection = NEXT_CONNECTION_ID.fetch_add(1, Ordering::Relaxed).max(1);
        Ok(Self {
            commands,
            inbox,
            peer: Some(peer),
            connection,
            limits,
            wake: Some(wake),
        })
    }

    fn enqueue(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        message.validate(limits)?;
        match self.commands.try_send(Outbound::Message(Box::new(message))) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(ReplicationError::Backpressure),
            Err(TrySendError::Disconnected(_)) => return Err(ReplicationError::Disconnected),
        }
        Ok(())
    }
}

impl RemoteTransport for AsyncWorkerTransport {
    fn connection_id(&self) -> u64 {
        self.connection
    }

    fn negotiate(
        &mut self,
        local: &CapabilityManifest,
        limits: TransportLimits,
    ) -> Result<NegotiatedCapabilities, ReplicationError> {
        self.peer
            .as_ref()
            .ok_or(ReplicationError::Disconnected)
            .and_then(|peer| local.negotiate(peer, limits))
    }

    fn send(
        &mut self,
        message: TransportMessage,
        limits: TransportLimits,
    ) -> Result<(), ReplicationError> {
        if message.estimated_size() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        self.enqueue(message, limits)
    }

    fn recv(&mut self) -> Result<Option<TransportMessage>, ReplicationError> {
        match self.inbox.try_recv() {
            Ok(Ok(message)) => Ok(Some(message)),
            Ok(Err(error)) => Err(error),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ReplicationError::Disconnected),
        }
    }

    fn cancel(
        &mut self,
        cancellation: backend_engine::CancelAttempt,
    ) -> Result<(), ReplicationError> {
        self.enqueue(TransportMessage::CancelAttempt(cancellation), self.limits)
    }
}

impl Drop for AsyncWorkerTransport {
    fn drop(&mut self) {
        let _ = self.commands.try_send(Outbound::Shutdown);
        if let Some(stream) = self.wake.take() {
            stream.shutdown();
        }
    }
}

fn spawn_reader<S>(
    stream: S,
    limits: TransportLimits,
    inbox: SyncSender<Result<TransportMessage, ReplicationError>>,
) where
    S: Read + Send + 'static,
{
    let _ = thread::Builder::new()
        .name("backend-locald-worker-reader".to_owned())
        .spawn(move || {
            let mut stream = match FramedStream::new(stream, limits) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = inbox.send(Err(error));
                    return;
                }
            };
            loop {
                match stream.recv_message() {
                    Ok(message) => {
                        if inbox.send(Ok(message)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = inbox.send(Err(error));
                        break;
                    }
                }
            }
        });
}

fn spawn_writer<S>(
    stream: S,
    limits: TransportLimits,
    commands: Receiver<Outbound>,
    inbox: SyncSender<Result<TransportMessage, ReplicationError>>,
) where
    S: Write + Send + 'static,
{
    let _ = thread::Builder::new()
        .name("backend-locald-worker-writer".to_owned())
        .spawn(move || {
            let mut stream = match FramedStream::new(stream, limits) {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = inbox.send(Err(error));
                    return;
                }
            };
            while let Ok(command) = commands.recv() {
                match command {
                    Outbound::Message(message) => {
                        let result = stream.send_message(&message);
                        if let Err(error) = result {
                            let _ = inbox.send(Err(error));
                            break;
                        }
                    }
                    Outbound::Shutdown => {
                        break;
                    }
                }
            }
        });
}

/// Authenticates a TCP stream and returns the stream kind used by the
/// asynchronous transport. Kept here so every cross-host connector applies
/// the same authority and deadline policy.
pub(crate) fn authenticate_tcp(
    stream: std::net::TcpStream,
    authority: &TcpAuthority,
    timeout: Duration,
    max_record: usize,
) -> Result<WorkerStream, io::Error> {
    let stream = stream;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let stream = authority
        .client_handshake_authenticated(stream, max_record)
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
    Ok(WorkerStream::AuthenticatedTcp(stream))
}

#[cfg(any(unix, windows))]
pub(crate) fn authenticate_unix(
    stream: backend_engine::LocalStream,
    timeout: Duration,
) -> Result<WorkerStream, io::Error> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    if !backend_engine::peer_is_same_effective_uid(&stream)
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker peer credentials do not match locald",
        ));
    }
    Ok(WorkerStream::Unix(stream))
}
