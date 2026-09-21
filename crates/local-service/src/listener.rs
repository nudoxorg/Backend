//! Unix listener and owner-loop pump for locald.
//!
//! Connections never receive a direct mutable reference to the engine. Each
//! connection has one bounded request/reply slot and forwards frames to the
//! listener thread, which is the sole caller of [`LocaldService::handle_payload`].
//! This keeps client I/O concurrent while workspace selection and publication
//! remain single-owner operations.
//!
//! # Who ends a daemon
//!
//! A locald spawned by a surface is detached: the spawning process is gone
//! long before the daemon is, and nothing waits on it. The daemon therefore
//! retires itself. [`ListenerConfig::idle_timeout`] is the only mechanism that
//! does so without a client: once the connected-client count reaches zero and
//! stays there for the configured window, the run loop sets its own stop flag,
//! drains its workers, closes the owner, and unlinks the socket on drop
//! (see `Drop for UnixListenerService`).
//!
//! The default window is ten minutes. A surface keeps its daemon alive simply
//! by staying connected — the window only advances while nothing is connected
//! and no owner work is in flight — and a host that wants a daemon to outlive
//! every client passes `--idle-timeout-ms 0`.
//!
//! A surface can also end a daemon explicitly. [`ListenerShutdown`] is handed
//! to the service at bind time, so a `backend_locald::EngineRequest::Shutdown`
//! frame arriving on the wire is answered by this listener's own stop flag
//! rather than by the workspace owner.

use crate::protocol::{FrameLimits, ProtocolError};
use crate::service::{LocaldService, OwnerService};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Default window a listener may spend with no connected client before it
/// retires itself. Long enough that a surface which closes one session and
/// opens another keeps its warm owner; short enough that an abandoned daemon
/// is gone before the user notices it.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(10);

/// Default window the single owner loop may take to answer one request.
///
/// Indexing a project runs on the owner thread and is bounded by the engine's
/// ten-minute per-package compile deadline, so a client that gives up sooner
/// always gives up on work that is still succeeding. This is deliberately
/// longer than that deadline rather than equal to it.
pub const DEFAULT_OWNER_REPLY_TIMEOUT: Duration = Duration::from_mins(15);

#[cfg(unix)]
#[path = "listener/transport.rs"]
mod transport;
#[cfg(unix)]
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
    ///
    /// This is charged only once a frame has started arriving. A connection
    /// sitting between requests is idle, not slow, and is bounded by
    /// [`Self::request_idle_timeout`] instead.
    pub io_timeout: Duration,
    /// How long a connected client may sit between requests before its
    /// connection is closed.
    ///
    /// Charging [`Self::io_timeout`] for this wait closed every connection
    /// that went 30 seconds without a request, which is what an agent does
    /// while it thinks — so a long-lived client's next call always failed
    /// with a reset. The wait is still bounded, by the same window the
    /// daemon uses to retire itself, so an abandoned connection never pins a
    /// client slot forever.
    pub request_idle_timeout: Duration,
    /// How long the single owner loop may take to answer one admitted
    /// request before the connection gives up on it.
    ///
    /// This is not an I/O deadline: the request has already been handed to
    /// the owner, which indexes a project on the calling thread. Charging
    /// [`Self::io_timeout`] for that wait made every index or removal of a
    /// real project report `Timeout` and close the connection after 30
    /// seconds — while the owner went on to run the intent anyway, so the
    /// surface reported a failure for work that then happened. The window
    /// must outlast the engine's own per-package compile deadline, which is
    /// ten minutes (`crates/engine/src/application/host.rs`,
    /// `COMPILER_TIMEOUT`), or the client always gives up first.
    pub owner_reply_timeout: Duration,
    /// Maximum concurrently connected clients.
    pub max_clients: usize,
    /// Poll interval used while waiting for a client or owner request.
    pub poll_interval: Duration,
    /// How long the listener may run with no connected client and no owner
    /// progress before it stops itself. `None` keeps the daemon resident for
    /// the life of the process.
    pub idle_timeout: Option<Duration>,
}

impl ListenerConfig {
    /// Creates a listener configuration with bounded defaults.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            limits: FrameLimits::default(),
            io_timeout: Duration::from_secs(30),
            request_idle_timeout: DEFAULT_IDLE_TIMEOUT,
            owner_reply_timeout: DEFAULT_OWNER_REPLY_TIMEOUT,
            max_clients: 64,
            poll_interval: Duration::from_millis(5),
            idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
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
            // A zero window would close a connection before it could send
            // its first request.
            || self.request_idle_timeout.is_zero()
            || self.owner_reply_timeout.is_zero()
            || self.max_clients == 0
            || self.poll_interval.is_zero()
            // A zero idle window would retire the daemon before its first
            // client could connect. "Never time out" is spelled `None`.
            || self.idle_timeout.is_some_and(|idle| idle.is_zero())
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
    fn authorize(&self, stream: &std::os::unix::net::UnixStream) -> Result<(), PeerPolicyError>;
}

/// Portable peer policy used when the host has no credential adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilesystemPeerPolicy;

impl PeerPolicy for FilesystemPeerPolicy {
    fn authorize(&self, stream: &std::os::unix::net::UnixStream) -> Result<(), PeerPolicyError> {
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
#[cfg(unix)]
pub struct UnixListenerService<O> {
    listener: std::os::unix::net::UnixListener,
    service: LocaldService<O>,
    path: PathBuf,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
    /// Number of workers that currently own an admitted request and still
    /// owe its response to the client.  Shutdown must let these responses
    /// cross the socket before closing listener-owned stream clones.
    inflight: Arc<AtomicUsize>,
    config: ListenerConfig,
    workers: Vec<JoinHandle<()>>,
    inbound: Receiver<Inbound>,
    inbound_sender: SyncSender<Inbound>,
    peer_policy: Arc<dyn PeerPolicy>,
    streams: Arc<Mutex<std::collections::BTreeMap<usize, std::os::unix::net::UnixStream>>>,
    next_connection_id: AtomicUsize,
    report: RunReport,
    telemetry: backend_engine::Telemetry,
}

#[cfg(unix)]
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

#[cfg(unix)]
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
        // Sweeping runs on every start, before bind: a killed owner always
        // leaves its socket behind, and a live one must be reported as
        // `AlreadyRunning` rather than have its endpoint stolen.
        prepare_socket_path(&path)?;
        let listener = std::os::unix::net::UnixListener::bind(&path)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        set_private_socket_permissions(&path)?;
        listener
            .set_nonblocking(true)
            .map_err(|error| ListenerError::Io(error.kind()))?;
        let (inbound_sender, inbound) = mpsc::sync_channel(config.max_clients);
        let stop = Arc::new(AtomicBool::new(false));
        // The wire shutdown request is a listener lifecycle operation, never a
        // workspace mutation. Handing the capability to the service here —
        // rather than from one process entry point — keeps it reachable for
        // the embedded host as well as the headless daemon.
        let mut service = service;
        service.attach_lifecycle(ListenerShutdown {
            stop: Arc::clone(&stop),
        });
        Ok(Self {
            listener,
            service,
            path,
            stop,
            active: Arc::new(AtomicUsize::new(0)),
            inflight: Arc::new(AtomicUsize::new(0)),
            config,
            workers: Vec::new(),
            inbound,
            inbound_sender,
            peer_policy,
            streams: Arc::new(Mutex::new(std::collections::BTreeMap::new())),
            next_connection_id: AtomicUsize::new(1),
            report: RunReport::default(),
            telemetry: backend_engine::Telemetry::disabled(),
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

    /// Enables bounded process telemetry for transport requests.
    #[must_use]
    pub fn with_telemetry(mut self, telemetry: backend_engine::Telemetry) -> Self {
        self.telemetry = telemetry;
        self
    }

    /// Returns an eventually consistent telemetry snapshot.
    #[must_use]
    pub fn telemetry_snapshot(&self) -> backend_engine::TelemetrySnapshot {
        self.telemetry.snapshot()
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
    /// A detached daemon has no parent to reap it, so this loop also enforces
    /// [`ListenerConfig::idle_timeout`]: it stops itself once nothing has been
    /// connected and no owner work has progressed for that window.
    pub fn run(&mut self) -> Result<RunReport, ListenerError> {
        let mut last_progress = Instant::now();
        while !self.is_shutdown() {
            self.accept_available()?;
            if self.drain_owner_once() || self.active.load(Ordering::Acquire) != 0 {
                last_progress = Instant::now();
                continue;
            }
            if self.idle_window_elapsed(last_progress) {
                // Nothing has been connected and no owner work has progressed
                // for the whole window. A detached daemon has no parent to
                // reap it, so this branch is the only thing between one
                // abandoned surface and a socket per workspace that lives
                // until the machine restarts. Setting the stop flag rather
                // than only breaking means a host holding a
                // `ListenerShutdown` observes the retirement instead of
                // waiting on a loop that already ended.
                self.stop.store(true, Ordering::Release);
                break;
            }
            thread::sleep(self.config.poll_interval);
        }
        self.finish_workers();
        self.service.close();
        Ok(self.report)
    }

    /// Reports whether the configured idle window has passed with no client
    /// connected and no owner progress.
    fn idle_window_elapsed(&self, last_progress: Instant) -> bool {
        self.config
            .idle_timeout
            .is_some_and(|idle| last_progress.elapsed() >= idle)
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
        self.report.failures = self
            .report
            .failures
            .saturating_add(reap_finished_workers(&mut self.workers));
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
                    self.spawn_connection_worker(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(ListenerError::Io(error.kind())),
            }
        }
        Ok(())
    }

    /// Hands one authorized stream to its own bounded worker thread.
    fn spawn_connection_worker(&mut self, stream: std::os::unix::net::UnixStream) {
        let sender = self.inbound_sender.clone();
        let stop = Arc::clone(&self.stop);
        let active = Arc::clone(&self.active);
        let inflight = Arc::clone(&self.inflight);
        let streams = Arc::clone(&self.streams);
        let limits = self.config.limits;
        let timeout = self.config.io_timeout;
        let request_idle = self.config.request_idle_timeout;
        let owner_reply = self.config.owner_reply_timeout;
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
                    inflight,
                    streams,
                    connection_id,
                    limits,
                    timeout,
                    request_idle,
                    owner_reply,
                },
            );
        });
        self.workers.push(worker);
    }

    fn drain_owner_once(&mut self) -> bool {
        // Keep owner progress independent of whether clients are currently
        // producing requests.
        let owner_progress = self.service.owner_mut().serve_one();
        match self.inbound.try_recv() {
            Ok(inbound) => {
                let started = Instant::now();
                let result = self.service.handle_payload(&inbound.payload);
                let failed = result.is_err();
                self.telemetry.record_with(|| backend_engine::Observation {
                    family: backend_engine::MetricFamily::Transport,
                    outcome: if failed {
                        backend_engine::MetricOutcome::Failed
                    } else {
                        backend_engine::MetricOutcome::Completed
                    },
                    latency: started.elapsed(),
                    units: u64::try_from(inbound.payload.len()).unwrap_or(u64::MAX),
                });
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
        // A wire shutdown request sets the stop flag while its worker is still
        // waiting to write the acknowledgement.  Drain admitted requests
        // until those workers have handed their replies to the socket; only
        // then is it safe to close listener-owned clones.  The deadline keeps
        // an externally requested shutdown bounded when an owner is wedged.
        let deadline = Instant::now() + self.config.owner_reply_timeout;
        while self.inflight.load(Ordering::Acquire) != 0 && Instant::now() < deadline {
            let _ = self.drain_owner_once();
            thread::sleep(self.config.poll_interval);
        }
        // A worker may be parked in its bounded read deadline while holding a
        // long-lived subscription connection. Closing the listener-owned
        // clones wakes those readers immediately so shutdown can join every
        // worker deterministically instead of waiting for the idle timeout.
        if let Ok(streams) = self.streams.lock() {
            for stream in streams.values() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        // Dropping the sender wakes readers that are waiting to submit their
        // final frame. The worker-owned clone is then released on exit.
        while let Some(worker) = self.workers.pop() {
            let _ = worker.join();
        }
    }
}

/// Classifies an endpoint before any workspace lock is taken.
///
/// Returns `Ok(())` when the path is free — including after removing a socket
/// whose owner is provably gone — [`ListenerError::AlreadyRunning`] when a
/// live owner answered, and [`ListenerError::EndpointOccupied`] when a
/// non-socket sits on the path. This is the same admission `bind` performs;
/// it is exposed so a host can ask the question before it composes an owner.
#[cfg(unix)]
pub(crate) fn sweep_endpoint(path: &Path) -> Result<(), ListenerError> {
    prepare_socket_path(path)
}

#[cfg(unix)]
fn reap_finished_workers(workers: &mut Vec<JoinHandle<()>>) -> usize {
    let mut failures = 0usize;
    let mut index = 0usize;
    while index < workers.len() {
        if !workers[index].is_finished() {
            index += 1;
            continue;
        }
        let worker = workers.swap_remove(index);
        if worker.join().is_err() {
            failures = failures.saturating_add(1);
        }
    }
    failures
}

#[cfg(unix)]
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

#[cfg(not(unix))]
/// Unix endpoints are unavailable on this target.
pub struct UnixListenerService<O>(std::marker::PhantomData<O>);

#[cfg(not(unix))]
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

    /// An owner that takes longer than any per-frame deadline to answer.
    #[derive(Debug)]
    struct SlowOwner(Duration);

    impl OwnerService for SlowOwner {
        fn command(&mut self, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
            thread::sleep(self.0);
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

    #[test]
    fn completed_connection_workers_are_reaped_without_growing_the_handle_set() {
        let mut workers = (0..256).map(|_| thread::spawn(|| {})).collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && workers.iter().any(|worker| !worker.is_finished()) {
            thread::yield_now();
        }
        assert!(workers.iter().all(JoinHandle::is_finished));
        assert_eq!(reap_finished_workers(&mut workers), 0);
        assert!(workers.is_empty());
    }

    fn socket_path(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let leaf = format!("backend-locald-{label}-{nonce}.sock");
        let preferred = std::env::temp_dir().join(&leaf);
        // The per-session macOS temporary directory does not leave room for a
        // bindable `sun_path`, so fall back to `/tmp` when it does not fit.
        if backend_engine::UnixEndpointRef::new(&preferred).is_ok() {
            preferred
        } else {
            Path::new("/tmp").join(leaf)
        }
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
            request_idle_timeout: Duration::from_secs(5),
            owner_reply_timeout: Duration::from_secs(5),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: None,
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let telemetry = backend_engine::Telemetry::enabled();
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"))
            .with_telemetry(telemetry.clone());
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
        let transport = telemetry
            .snapshot()
            .family(backend_engine::MetricFamily::Transport);
        assert_eq!(transport.completed, 1);
        assert_eq!(transport.failed, 0);
        assert_eq!(transport.units, 4);
        let mode = std::fs::metadata(&path)
            .unwrap_or_else(|error| panic!("socket metadata: {error}"))
            .permissions();
        assert_eq!(mode.mode() & 0o777, 0o600);
        drop(listener);
        assert!(!path.exists());
    }

    /// A client that is idle between requests keeps its connection.
    ///
    /// The per-frame read deadline used to be charged for this wait, so a
    /// connection that went one `io_timeout` without sending anything was
    /// closed. A long-lived client — the MCP server, which holds one session
    /// while an agent thinks — then failed on every call after the first with
    /// a connection reset. The wait is bounded separately, by
    /// `request_idle_timeout`.
    ///
    /// The assertion is on the reply body, not on a connection count: a
    /// listener that accepted the connection and then dropped it would report
    /// the same one connection.
    #[test]
    fn a_client_idle_longer_than_the_frame_deadline_is_still_served() {
        let path = socket_path("idle-between-requests");
        let io_timeout = Duration::from_millis(80);
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout,
            request_idle_timeout: Duration::from_secs(5),
            owner_reply_timeout: Duration::from_secs(5),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: None,
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(client_path)
                .unwrap_or_else(|error| panic!("connect: {error}"));
            // Longer than the per-frame deadline: this is a client thinking,
            // not a client trickling a frame.
            thread::sleep(io_timeout * 5);
            let request = crate::protocol::frame(b"late", limits())
                .unwrap_or_else(|error| panic!("frame: {error}"));
            stream
                .write_all(&request)
                .unwrap_or_else(|error| panic!("write: {error}"));
            read_frame(&mut stream, limits())
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !client.is_finished() {
            let _ = listener
                .run_once()
                .unwrap_or_else(|error| panic!("run once: {error}"));
            thread::sleep(Duration::from_millis(1));
        }
        let response = client
            .join()
            .unwrap_or_else(|_| panic!("client thread panicked"))
            .unwrap_or_else(|error| {
                panic!("an idle connection was closed before its request: {error}")
            });
        listener.shutdown();
        assert_eq!(
            response, b"late",
            "the request sent after the idle gap must be served on the same \
             connection"
        );
        drop(listener);
    }

    /// A request the owner is still working on is not a timed-out request.
    ///
    /// The per-frame read deadline used to bound this wait too, so any index
    /// or removal that took longer than `io_timeout` got a `Timeout` reply
    /// and a closed connection — while the owner went on to finish the work.
    /// The surface then reported a failure for something that succeeded.
    ///
    /// The assertion is on the reply body, not on whether a reply arrived: a
    /// `Timeout` error frame is also a reply.
    #[test]
    fn a_slow_owner_reply_outlasts_the_frame_deadline() {
        let path = socket_path("slow-owner");
        let io_timeout = Duration::from_millis(60);
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout,
            request_idle_timeout: Duration::from_secs(5),
            owner_reply_timeout: Duration::from_secs(5),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: None,
        };
        let service = LocaldService::new(SlowOwner(io_timeout * 5), config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(client_path)
                .unwrap_or_else(|error| panic!("connect: {error}"));
            let request = crate::protocol::frame(b"slow", limits())
                .unwrap_or_else(|error| panic!("frame: {error}"));
            stream
                .write_all(&request)
                .unwrap_or_else(|error| panic!("write: {error}"));
            read_frame(&mut stream, limits())
        });
        // Drive the listener until the client has its answer. Shutting down
        // as soon as the owner replied would race the worker's write and
        // prove nothing about the window under test.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !client.is_finished() {
            let _ = listener
                .run_once()
                .unwrap_or_else(|error| panic!("run once: {error}"));
            thread::sleep(Duration::from_millis(1));
        }
        let response = client
            .join()
            .unwrap_or_else(|_| panic!("client thread panicked"))
            .unwrap_or_else(|error| panic!("the slow reply never arrived: {error}"));
        listener.shutdown();
        assert_eq!(
            response, b"slow",
            "a request the owner completed must be answered with its own \
             reply, never with a timeout the owner's work outlived"
        );
        drop(listener);
    }

    #[test]
    fn an_idle_listener_retires_itself_and_removes_its_socket() {
        let path = socket_path("idle-retire");
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout: Duration::from_millis(250),
            request_idle_timeout: Duration::from_secs(5),
            owner_reply_timeout: Duration::from_secs(5),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: Some(Duration::from_millis(150)),
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let started = Instant::now();
        let report = listener
            .run()
            .unwrap_or_else(|error| panic!("run: {error}"));
        assert_eq!(report.connections, 0);
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "the listener retired before its idle window elapsed"
        );
        assert!(
            listener.is_shutdown(),
            "an idle retirement must set the listener's own stop flag"
        );
        drop(listener);
        assert!(!path.exists(), "a retiring listener must unlink its socket");
    }

    #[test]
    fn a_connected_client_holds_the_idle_window_open() {
        let path = socket_path("idle-held-open");
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            // The per-connection read deadline must outlast the idle window,
            // otherwise the worker would retire the connection itself and the
            // assertion below would prove nothing.
            io_timeout: Duration::from_secs(10),
            request_idle_timeout: Duration::from_secs(10),
            owner_reply_timeout: Duration::from_secs(10),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: Some(Duration::from_millis(100)),
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let held = std::os::unix::net::UnixStream::connect(&path)
            .unwrap_or_else(|error| panic!("connect: {error}"));
        let runner = thread::spawn(move || listener.run().map(|report| report.connections));

        // Six idle windows with the client connected. Nothing may retire.
        thread::sleep(Duration::from_millis(600));
        assert!(
            !runner.is_finished(),
            "a listener retired while a client was connected"
        );
        assert!(path.exists());

        drop(held);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !runner.is_finished() {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(
            runner.is_finished(),
            "a listener stayed resident after its last client left"
        );
        assert_eq!(
            runner
                .join()
                .unwrap_or_else(|_| panic!("listener thread panicked"))
                .unwrap_or_else(|error| panic!("run: {error}")),
            1
        );
    }

    #[test]
    fn a_zero_idle_window_is_rejected_and_none_is_admitted() {
        let mut config = ListenerConfig::new(socket_path("idle-validate"));
        assert_eq!(config.idle_timeout, Some(DEFAULT_IDLE_TIMEOUT));
        config.validate().unwrap_or_else(|error| panic!("{error}"));
        config.idle_timeout = None;
        config.validate().unwrap_or_else(|error| panic!("{error}"));
        config.idle_timeout = Some(Duration::ZERO);
        assert_eq!(config.validate(), Err(ListenerError::InvalidConfig));
    }

    #[test]
    fn a_wire_shutdown_request_stops_the_listener_that_bound_the_service() {
        let path = socket_path("wire-shutdown");
        let config = ListenerConfig {
            path: path.clone(),
            limits: limits(),
            io_timeout: Duration::from_secs(10),
            request_idle_timeout: Duration::from_secs(10),
            owner_reply_timeout: Duration::from_secs(10),
            max_clients: 2,
            poll_interval: Duration::from_millis(1),
            idle_timeout: None,
        };
        let service = LocaldService::new(FakeOwner, config.limits)
            .unwrap_or_else(|error| panic!("service: {error}"));
        let mut listener = UnixListenerService::bind(service, config)
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let client_path = path.clone();
        let client = thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(client_path)
                .unwrap_or_else(|error| panic!("connect: {error}"));
            let body =
                crate::protocol::encode_engine_request(11, &EngineRequest::Shutdown, limits())
                    .unwrap_or_else(|error| panic!("encode shutdown: {error}"));
            let request = crate::protocol::frame(&body, limits())
                .unwrap_or_else(|error| panic!("frame: {error}"));
            stream
                .write_all(&request)
                .unwrap_or_else(|error| panic!("write: {error}"));
            read_frame(&mut stream, limits())
                .unwrap_or_else(|error| panic!("read response: {error}"))
        });
        let report = listener
            .run()
            .unwrap_or_else(|error| panic!("run: {error}"));
        assert_eq!(report.frames, 1);
        assert_eq!(report.failures, 0);
        let response = client
            .join()
            .unwrap_or_else(|_| panic!("client thread panicked"));
        assert_eq!(
            crate::protocol::decode_response(&response, limits())
                .unwrap_or_else(|error| panic!("decode response: {error}")),
            crate::protocol::ResponseFrame::Engine {
                request_id: 11,
                status: EngineStatus::Accepted,
            }
        );
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
            request_idle_timeout: Duration::from_millis(100),
            owner_reply_timeout: Duration::from_millis(100),
            max_clients: 1,
            poll_interval: Duration::from_millis(1),
            idle_timeout: None,
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
