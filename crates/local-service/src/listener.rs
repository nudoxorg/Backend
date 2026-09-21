//! Unix listener and owner-loop pump for locald.
//!
//! Connections never receive a direct mutable reference to the engine. Each
//! connection has one bounded request/reply slot and forwards frames to the
//! listener thread, which is the sole caller of [`LocaldService::handle_payload`].
//! This keeps client I/O concurrent while workspace selection and publication
//! remain single-owner operations.

use crate::protocol::{FrameLimits, ProtocolError};
use crate::service::{LocaldService, OwnerService};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(any(unix, windows))]
#[path = "listener/transport.rs"]
mod transport;
#[cfg(any(unix, windows))]
use transport::{
    ConnectionContext, configure_stream, connection_worker, prepare_socket_path,
    set_private_socket_permissions,
};

/// Limits and lifecycle policy for a local Unix endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenerConfig {
    /// Endpoint path. It is restricted to the platform Unix socket path
    /// budget before bind.
    pub path: PathBuf,
    /// Frame and replication limits.
    pub limits: FrameLimits,
    /// Read/write timeout for one client operation.
    pub io_timeout: Duration,
    /// Maximum concurrently connected clients.
    pub max_clients: usize,
    /// Poll interval used while waiting for a client or owner request.
    pub poll_interval: Duration,
}

impl ListenerConfig {
    /// Creates a listener configuration with bounded defaults.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            limits: FrameLimits::default(),
            io_timeout: Duration::from_secs(30),
            max_clients: 64,
            poll_interval: Duration::from_millis(5),
        }
    }

    /// Validates path, timeout, count, and frame limits.
    ///
    /// # Errors
    ///
    /// Returns an error when an endpoint or transport limit is invalid.
    pub fn validate(&self) -> Result<(), ListenerError> {
        self.limits.validate().map_err(ListenerError::Protocol)?;
        if backend_engine::UnixEndpointRef::new(&self.path).is_err()
            || self.io_timeout.is_zero()
            || self.max_clients == 0
            || self.poll_interval.is_zero()
        {
            return Err(ListenerError::InvalidConfig);
        }
        Ok(())
    }
}

/// Listener lifecycle report.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunReport {
    /// Number of accepted client connections.
    pub connections: usize,
    /// Number of complete request frames handled.
    pub frames: usize,
    /// Number of protocol or owner failures observed on client threads.
    pub failures: usize,
}

/// Cloneable stop capability for a running embedded listener.
///
/// Holding this value grants only lifecycle control. It cannot access the
/// workspace owner, submit commands, or inspect mutable service state.
#[derive(Clone, Debug)]
pub struct ListenerShutdown {
    stop: Arc<AtomicBool>,
}

impl ListenerShutdown {
    /// Requests clean shutdown of the listener that issued this capability.
    pub fn request(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Reports whether shutdown has been requested.
    #[must_use]
    pub fn is_requested(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// Peer authorization policy for accepted Unix streams.
///
/// The endpoint is created with mode `0600`, and the default policy also
/// obtains the accepted stream's effective UID through the shared engine
/// platform adapter. A stream is admitted only when that UID matches this
/// process. Credential inspection errors fail closed before owner-loop
/// capacity is consumed. Deployments with a stricter identity policy can pass
/// an implementation through [`UnixListenerService::bind_with_peer_policy`].
pub trait PeerPolicy: Send + Sync + 'static {
    /// Authorizes one accepted stream before it consumes owner-loop capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when peer credentials cannot be validated.
    fn authorize(&self, stream: &backend_platform::LocalStream) -> Result<(), PeerPolicyError>;
}

/// Portable peer policy used when the host has no credential adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilesystemPeerPolicy;

impl PeerPolicy for FilesystemPeerPolicy {
    fn authorize(&self, stream: &backend_platform::LocalStream) -> Result<(), PeerPolicyError> {
        let address = stream
            .peer_addr()
            .map_err(|error| PeerPolicyError::Io(error.kind()))?;
        if address.is_unnamed() {
            backend_engine::peer_is_same_effective_uid(stream).map_or_else(
                |error| Err(map_peer_credential_error(error)),
                |same_uid| {
                    if same_uid {
                        Ok(())
                    } else {
                        Err(PeerPolicyError::Rejected)
                    }
                },
            )
        } else {
            Err(PeerPolicyError::Rejected)
        }
    }
}

fn map_peer_credential_error(error: backend_engine::PeerCredentialError) -> PeerPolicyError {
    match error {
        backend_engine::PeerCredentialError::Unsupported => {
            PeerPolicyError::Io(io::ErrorKind::Unsupported)
        }
        backend_engine::PeerCredentialError::Io(kind) => PeerPolicyError::Io(kind),
        backend_engine::PeerCredentialError::InvalidId => {
            PeerPolicyError::Io(io::ErrorKind::InvalidData)
        }
    }
}

/// Failure returned by a Unix peer authorization hook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerPolicyError {
    /// The peer did not meet the host's identity policy.
    Rejected,
    /// Peer metadata could not be inspected.
    Io(io::ErrorKind),
}

impl fmt::Display for PeerPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected => formatter.write_str("locald peer was rejected"),
            Self::Io(kind) => write!(formatter, "locald peer inspection failed: {kind:?}"),
        }
    }
}

impl std::error::Error for PeerPolicyError {}

struct Inbound {
    payload: Vec<u8>,
    reply: SyncSender<Result<Vec<u8>, ProtocolError>>,
}

/// A single-owner bounded Unix listener.
#[cfg(any(unix, windows))]
pub struct UnixListenerService<O> {
    listener: backend_platform::LocalListener,
    service: LocaldService<O>,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    config: ListenerConfig,
    workers: Vec<JoinHandle<()>>,
    inbound: Receiver<Inbound>,
    inbound_sender: SyncSender<Inbound>,
    peer_policy: Arc<dyn PeerPolicy>,
    streams: Arc<Mutex<std::collections::BTreeMap<usize, backend_platform::LocalStream>>>,
    next_connection_id: AtomicUsize,
    report: RunReport,
}

#[cfg(any(unix, windows))]
impl<O: OwnerService + 'static> fmt::Debug for UnixListenerService<O>
where
    O: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnixListenerService")
            .field("path", &self.path)
            .field("service", &self.service)
            .field("active", &self.active.load(Ordering::Acquire))
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

#[cfg(any(unix, windows))]
impl<O: OwnerService + 'static> UnixListenerService<O> {
    /// Binds a private Unix endpoint around one owner service.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint cannot be created or authorized.
    pub fn bind(service: LocaldService<O>, config: ListenerConfig) -> Result<Self, ListenerError> {
        Self::bind_with_peer_policy(service, config, Arc::new(FilesystemPeerPolicy))
    }

    /// Binds a private Unix endpoint with an explicit peer credential policy.
    /// The policy runs immediately after `accept`, before the connection is
    /// counted or handed to a worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint or peer policy cannot be established.
    pub fn bind_with_peer_policy(
        service: LocaldService<O>,
        config: ListenerConfig,
        peer_policy: Arc<dyn PeerPolicy>,
    ) -> Result<Self, ListenerError> {
        config.validate()?;
        let path = config.path.clone();
        prepare_socket_path(&path)?;
        let listener = backend_platform::LocalListener::bind(&path)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        set_private_socket_permissions(&path)?;
        listener
            .set_nonblocking(true)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        let (inbound_sender, inbound) = mpsc::sync_channel(config.max_clients);
        Ok(Self {
            listener,
            service,
            path,
            stop: Arc::new(AtomicBool::new(false)),
            active: Arc::new(AtomicUsize::new(0)),
            config,
            workers: Vec::new(),
            inbound,
            inbound_sender,
            peer_policy,
            streams: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            next_connection_id: AtomicUsize::new(1),
            report: RunReport::default(),
        })
    }

    /// Returns the private endpoint path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Requests clean shutdown. Existing clients finish their current bounded
    /// frame and then leave; no new connections are admitted.
    pub fn shutdown(&self) {
        self.shutdown_handle().request();
    }

    /// Returns a lifecycle-only capability suitable for another host thread.
    #[must_use]
    pub fn shutdown_handle(&self) -> ListenerShutdown {
        ListenerShutdown {
            stop: Arc::clone(&self.stop),
        }
    }

    /// Returns whether shutdown has been requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Returns a report of work processed so far.
    #[must_use]
    pub const fn report(&self) -> RunReport {
        self.report
    }

    /// Returns a mutable reference to the owner service for integration hooks.
    #[must_use]
    pub const fn service_mut(&mut self) -> &mut LocaldService<O> {
        &mut self.service
    }

    /// Runs the accept/owner loop until shutdown or all channels close.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting or servicing a connection fails.
    pub fn run(&mut self) -> Result<RunReport, ListenerError> {
        while !self.is_shutdown() {
            self.accept_available()?;
            if !self.drain_owner_once() {
                thread::sleep(self.config.poll_interval);
            }
        }
        self.finish_workers();
        self.service.close();
        Ok(self.report)
    }

    /// Runs one nonblocking listener/owner iteration. This is useful for a
    /// host process that owns its own signal handling or event loop.
    ///
    /// # Errors
    ///
    /// Returns an error when accepting or servicing a connection fails.
    pub fn run_once(&mut self) -> Result<bool, ListenerError> {
        if self.is_shutdown() {
            return Ok(false);
        }
        self.accept_available()?;
        let handled = self.drain_owner_once();
        Ok(handled || self.active.load(Ordering::Acquire) != 0)
    }

    fn accept_available(&mut self) -> Result<(), ListenerError> {
        loop {
            match self.listener.accept() {
                Ok((stream, _address)) => {
                    if self.peer_policy.authorize(&stream).is_err() {
                        self.report.failures = self.report.failures.saturating_add(1);
                        drop(stream);
                        continue;
                    }
                    if self.active.load(Ordering::Acquire) >= self.config.max_clients {
                        drop(stream);
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    }
                    if configure_stream(&stream, self.config.io_timeout).is_err() {
                        // A probe can disconnect between `accept` and socket
                        // configuration. That connection is isolated input;
                        // it must not terminate the process-wide listener.
                        self.report.failures = self.report.failures.saturating_add(1);
                        drop(stream);
                        continue;
                    }
                    let sender = self.inbound_sender.clone();
                    let stop = Arc::clone(&self.stop);
                    let active = Arc::clone(&self.active);
                    let streams = Arc::clone(&self.streams);
                    let limits = self.config.limits;
                    let timeout = self.config.io_timeout;
                    let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
                    if let Ok(mut active_streams) = self.streams.lock()
                        && let Ok(clone) = stream.try_clone()
                    {
                        active_streams.insert(connection_id, clone);
                    }
                    self.active.fetch_add(1, Ordering::AcqRel);
                    self.report.connections = self.report.connections.saturating_add(1);
                    let worker = thread::spawn(move || {
                        connection_worker(
                            stream,
                            ConnectionContext {
                                sender,
                                stop,
                                active,
                                streams,
                                connection_id,
                                limits,
                                timeout,
                            },
                        );
                    });
                    self.workers.push(worker);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(ListenerError::Io(error.kind())),
            }
        }
        Ok(())
    }

    fn drain_owner_once(&mut self) -> bool {
        // Keep owner progress independent of whether clients are currently
        // producing requests.
        let owner_progress = self.service.owner_mut().serve_one();
        match self.inbound.try_recv() {
            Ok(inbound) => {
                let result = self.service.handle_payload(&inbound.payload);
                let failed = result.is_err();
                let _ = inbound.reply.send(result);
                self.report.frames = self.report.frames.saturating_add(1);
                if failed {
                    self.report.failures = self.report.failures.saturating_add(1);
                }
                true
            }
            Err(TryRecvError::Empty) => owner_progress,
            Err(TryRecvError::Disconnected) => {
                self.stop.store(true, Ordering::Release);
                false
            }
        }
    }

    fn finish_workers(&mut self) {
        self.shutdown();
        // Dropping the sender wakes readers that are waiting to submit their
        // final frame. The worker-owned clone is then released on exit.
        while let Some(worker) = self.workers.pop() {
            let _ = worker.join();
        }
    }
}

#[cfg(any(unix, windows))]
impl<O> Drop for UnixListenerService<O> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(streams) = self.streams.lock() {
            for stream in streams.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        while let Some(worker) = self.workers.pop() {
            let _ = worker.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Listener failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListenerError {
    /// Configuration was empty, oversized, or inconsistent.
    InvalidConfig,
    /// A non-socket file already occupies the endpoint path.
    EndpointOccupied,
    /// Another locald process currently owns the endpoint.
    AlreadyRunning,
    /// Filesystem or stream I/O failure.
    Io(io::ErrorKind),
    /// Protocol limits or framing failed validation.
    Protocol(ProtocolError),
    /// The owner service stopped before the listener could continue.
    ServiceStopped,
}

impl fmt::Display for ListenerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => formatter.write_str("invalid locald listener configuration"),
            Self::EndpointOccupied => {
                formatter.write_str("locald endpoint is occupied by a non-socket")
            }
            Self::AlreadyRunning => formatter.write_str("locald endpoint is already in use"),
            Self::Io(kind) => write!(formatter, "locald listener I/O failed: {kind:?}"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::ServiceStopped => formatter.write_str("locald owner service stopped"),
        }
    }
}

impl std::error::Error for ListenerError {}

#[cfg(not(any(unix, windows)))]
/// Local endpoints are unavailable on this target.
pub struct UnixListenerService<O>(std::marker::PhantomData<O>);

#[cfg(not(any(unix, windows)))]
impl<O> UnixListenerService<O> {
    /// Returns a platform error instead of silently selecting an alternate
    /// transport.
    pub fn bind(
        _service: LocaldService<O>,
        _config: ListenerConfig,
    ) -> Result<Self, ListenerError> {
        Err(ListenerError::InvalidConfig)
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::protocol::{EngineRequest, EngineStatus, FrameLimits, read_frame};
    use crate::service::OwnerService;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    #[derive(Debug, Default)]
    struct FakeOwner;

    impl OwnerService for FakeOwner {
        fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            Ok(body.to_vec())
        }

        fn engine(
            &mut self,
            _request_id: u64,
            _request: EngineRequest,
        ) -> Result<EngineStatus, ProtocolError> {
            Ok(EngineStatus::Accepted)
        }

        fn serve_one(&mut self) -> bool {
            false
        }

        fn close(&mut self) {}
    }

    #[derive(Debug)]
    struct RejectPeers;

    impl PeerPolicy for RejectPeers {
        fn authorize(
            &self,
            _stream: &std::os::unix::net::UnixStream,
        ) -> Result<(), PeerPolicyError> {
            Err(PeerPolicyError::Rejected)
        }
    }

    fn socket_path(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("backend-locald-{label}-{nonce}.sock"))
    }

    fn limits() -> FrameLimits {
        FrameLimits {
            max_frame: 4096,
            max_cursor: 128,
            max_frames_per_connection: 2,
            transport: backend_engine::TransportLimits {
                max_frame: 4096,
                max_chunk: 4096,
                ..backend_engine::TransportLimits::default()
            },
        }
    }

    #[test]
    fn unix_listener_round_trips_one_command_and_sets_private_mode() {
        let path = socket_path("roundtrip");
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout: Duration::from_millis(250),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(client_path)
                .unwrap_or_else(|error| panic!("connect: {error}"));
            let request = crate::protocol::frame(b"ping", limits())
                .unwrap_or_else(|error| panic!("frame: {error}"));
            stream
                .write_all(&request)
                .unwrap_or_else(|error| panic!("write: {error}"));
            read_frame(&mut stream, limits())
                .unwrap_or_else(|error| panic!("read response: {error}"))
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && listener.report().frames == 0 {
            let _ = listener
                .run_once()
                .unwrap_or_else(|error| panic!("run once: {error}"));
            thread::sleep(Duration::from_millis(1));
        }
        listener.shutdown();
        let response = client
            .join()
            .unwrap_or_else(|_| panic!("client thread panicked"));
        assert_eq!(response, b"ping");
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("socket metadata: {error}"))
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
        drop(listener);
        assert!(!path.exists());
    }

    #[test]
    fn injected_peer_policy_rejects_before_owner_admission() {
        let path = socket_path("peer-reject");
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout: Duration::from_millis(100),
            max_clients: 1,
            poll_interval: Duration::from_millis(1),
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener =
            UnixListenerService::bind_with_peer_policy(service, config, Arc::new(RejectPeers))
                .unwrap_or_else(|error| panic!("bind: {error}"));
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let _ = std::os::unix::net::UnixStream::connect(client_path);
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && listener.report().failures == 0 {
            let _ = listener
                .run_once()
                .unwrap_or_else(|error| panic!("run once: {error}"));
            thread::sleep(Duration::from_millis(1));
        }
        client
            .join()
            .unwrap_or_else(|_| panic!("client thread panicked"));
        assert_eq!(listener.report().connections, 0);
        assert_eq!(listener.report().failures, 1);
        drop(listener);
    }
}
