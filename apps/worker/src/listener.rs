//! Private Unix listener for the pure worker process.

use crate::protocol::WorkerProtocolError;
use crate::service::{JobAdmission, WorkerService};
use backend_engine::{PureRecipeExecutor, Relation, WorkerAttestationSigner};
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[path = "listener/transport.rs"]
mod transport;
#[cfg(any(unix, windows))]
use transport::{clear_active_stream, configure_stream, set_active_stream};
use transport::{
    clear_active_tcp_stream, configure_tcp_stream, prepare_socket_path, set_active_tcp_stream,
    set_private_socket_permissions,
};

/// Configuration for a bounded worker Unix endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerListenerConfig {
    /// Endpoint path.
    pub path: PathBuf,
    /// Stream framing and execution limits.
    pub limits: crate::protocol::WorkerLimits,
}

/// Configuration for a cross-host TCP worker endpoint. TCP has no Unix peer
/// credentials, so both sides must possess the owner-supplied authority key.
#[derive(Clone, Debug)]
pub struct TcpWorkerListenerConfig {
    /// Address to bind. The host may select a loopback address for local
    /// tests or a routable interface for a separate worker host.
    pub address: SocketAddr,
    /// Stream framing and execution limits.
    pub limits: crate::protocol::WorkerLimits,
    /// Credential required by the mutual authority handshake.
    pub authority: backend_engine::TcpAuthority,
    /// Confidentiality policy for the MAC-authenticated, plaintext record
    /// stream.
    pub exposure: TcpExposure,
}

/// Network exposure admitted for a worker TCP listener.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TcpExposure {
    /// Bind only to an operating-system loopback address.
    LoopbackOnly,
    /// The caller asserts that an outer confidential transport such as a
    /// mutually authenticated tunnel protects this listener.
    ExternalProtected,
}

impl TcpWorkerListenerConfig {
    /// Validates the network endpoint and bounded stream policy.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn validate(&self) -> Result<(), WorkerListenerError> {
        self.limits
            .validate()
            .map_err(WorkerListenerError::Protocol)?;
        if self.exposure == TcpExposure::LoopbackOnly && !self.address.ip().is_loopback() {
            return Err(WorkerListenerError::ConfidentialityRequired);
        }
        Ok(())
    }
}

impl WorkerListenerConfig {
    /// Creates a configuration with explicit endpoint path and bounded
    /// protocol defaults.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            limits: crate::protocol::WorkerLimits::default(),
        }
    }

    /// Validates endpoint path and stream limits.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn validate(&self) -> Result<(), WorkerListenerError> {
        self.limits
            .validate()
            .map_err(WorkerListenerError::Protocol)?;
        if backend_engine::UnixEndpointRef::new(&self.path).is_err() {
            return Err(WorkerListenerError::InvalidConfig);
        }
        Ok(())
    }
}

/// Worker listener report.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerRunReport {
    /// Number of accepted client connections.
    pub connections: usize,
    /// Number of canonical frames handled.
    pub frames: usize,
    /// Number of failed connections.
    pub failures: usize,
}

/// Peer authorization policy for a worker Unix endpoint.
///
/// Socket mode `0600` protects the endpoint at the filesystem boundary. The
/// default policy also obtains the accepted stream's effective UID through the
/// shared engine platform adapter and admits only the current process UID.
/// Credential inspection errors fail closed before worker capacity is
/// consumed. A deployment may pass a stricter policy to
/// [`UnixWorkerListener::bind_with_peer_policy`].
pub trait PeerPolicy: Send + Sync + 'static {
    /// Authorizes one accepted stream before worker capacity is consumed.
    ///
    /// # Errors
    /// Returns an error when the peer is not authorized.
    fn authorize(&self, stream: &backend_engine::LocalStream) -> Result<(), PeerPolicyError>;
}

/// Portable worker peer policy.
#[derive(Clone, Copy, Debug, Default)]
pub struct FilesystemPeerPolicy;

impl PeerPolicy for FilesystemPeerPolicy {
    fn authorize(&self, stream: &backend_engine::LocalStream) -> Result<(), PeerPolicyError> {
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

/// Failure returned by a worker peer authorization hook.
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
            Self::Rejected => formatter.write_str("worker peer was rejected"),
            Self::Io(kind) => write!(formatter, "worker peer inspection failed: {kind:?}"),
        }
    }
}

impl std::error::Error for PeerPolicyError {}

/// A bounded worker Unix listener. It admits one stream at a time so a pure
/// recipe cannot exceed the configured process/resource envelope through
/// accidental concurrent calls.
#[cfg(any(unix, windows))]
pub struct UnixWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    listener: backend_engine::LocalListener,
    worker: Option<WorkerService<E, S>>,
    admission: A,
    config: WorkerListenerConfig,
    path: PathBuf,
    stop: AtomicBool,
    peer_policy: Arc<dyn PeerPolicy>,
    active_stream: Arc<Mutex<Option<backend_engine::LocalStream>>>,
    report: WorkerRunReport,
    telemetry: backend_engine::Telemetry,
    relation: PhantomData<fn() -> R>,
}

/// A bounded TCP worker listener using the canonical replication stream after
/// a mutual authority handshake. One stream is served at a time so the
/// configured worker resource envelope cannot be multiplied by connections.
pub struct TcpWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    listener: TcpListener,
    worker: Option<WorkerService<E, S>>,
    admission: A,
    config: TcpWorkerListenerConfig,
    stop: AtomicBool,
    active_stream: Arc<Mutex<Option<TcpStream>>>,
    report: WorkerRunReport,
    telemetry: backend_engine::Telemetry,
    relation: PhantomData<fn() -> R>,
}

impl<E, S, R, A> fmt::Debug for TcpWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R> + fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TcpWorkerListener")
            .field("config", &self.config)
            .field("worker", &self.worker)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

impl<E, S, R, A> TcpWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    /// Binds a TCP worker endpoint with mutual authority authentication.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn bind(
        worker: WorkerService<E, S>,
        admission: A,
        config: TcpWorkerListenerConfig,
    ) -> Result<Self, WorkerListenerError> {
        config.validate()?;
        let listener = TcpListener::bind(config.address)
            .map_err(|error| WorkerListenerError::Io(error.kind()))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| WorkerListenerError::Io(error.kind()))?;
        Ok(Self {
            listener,
            worker: Some(worker),
            admission,
            config,
            stop: AtomicBool::new(false),
            active_stream: Arc::new(Mutex::new(None)),
            report: WorkerRunReport::default(),
            telemetry: backend_engine::Telemetry::disabled(),
            relation: PhantomData,
        })
    }

    /// Installs bounded telemetry shared with the process owner.
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

    /// Returns the actual bound address, including an OS-selected port.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn local_addr(&self) -> Result<SocketAddr, WorkerListenerError> {
        self.listener
            .local_addr()
            .map_err(|error| WorkerListenerError::Io(error.kind()))
    }

    /// Requests clean shutdown and wakes a currently active stream.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(stream) = self.active_stream.lock()
            && let Some(stream) = stream.as_ref()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Returns whether shutdown was requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Returns work processed so far.
    #[must_use]
    pub const fn report(&self) -> WorkerRunReport {
        self.report
    }

    /// Runs the authenticated listener until shutdown.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn run(&mut self) -> Result<WorkerRunReport, WorkerListenerError> {
        while !self.is_shutdown() {
            match self.listener.accept() {
                Ok((stream, _address)) => {
                    if configure_tcp_stream(&stream, self.config.limits.io_timeout).is_err() {
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    }
                    set_active_tcp_stream(&self.active_stream, &stream);
                    let Some(max_record) = self.config.limits.transport.max_frame.checked_add(4)
                    else {
                        clear_active_tcp_stream(&self.active_stream);
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    };
                    let mut stream = match self
                        .config
                        .authority
                        .server_handshake_authenticated(stream, max_record)
                    {
                        Ok(stream) => stream,
                        Err(_error) => {
                            clear_active_tcp_stream(&self.active_stream);
                            self.report.failures = self.report.failures.saturating_add(1);
                            continue;
                        }
                    };
                    self.report.connections = self.report.connections.saturating_add(1);
                    let started = Instant::now();
                    let result = self
                        .worker
                        .as_mut()
                        .ok_or(WorkerListenerError::WorkerConsumed)?
                        .serve_stream_socket(&mut stream, &mut self.admission);
                    record_transport(&self.telemetry, started, &result);
                    clear_active_tcp_stream(&self.active_stream);
                    match result {
                        Ok(frames) => {
                            self.report.frames = self.report.frames.saturating_add(frames);
                        }
                        Err(_error) => {
                            self.report.failures = self.report.failures.saturating_add(1);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(WorkerListenerError::Io(error.kind())),
            }
        }
        if let Some(worker) = self.worker.as_mut() {
            worker.close();
        }
        Ok(self.report)
    }

    /// Runs one nonblocking accept/serve iteration.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn run_once(&mut self) -> Result<bool, WorkerListenerError> {
        if self.is_shutdown() {
            return Ok(false);
        }
        let (stream, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) => return Err(WorkerListenerError::Io(error.kind())),
        };
        if configure_tcp_stream(&stream, self.config.limits.io_timeout).is_err() {
            self.report.failures = self.report.failures.saturating_add(1);
            return Ok(true);
        }
        set_active_tcp_stream(&self.active_stream, &stream);
        let Some(max_record) = self.config.limits.transport.max_frame.checked_add(4) else {
            clear_active_tcp_stream(&self.active_stream);
            self.report.failures = self.report.failures.saturating_add(1);
            return Ok(true);
        };
        let mut stream = match self
            .config
            .authority
            .server_handshake_authenticated(stream, max_record)
        {
            Ok(stream) => stream,
            Err(_error) => {
                clear_active_tcp_stream(&self.active_stream);
                self.report.failures = self.report.failures.saturating_add(1);
                return Ok(true);
            }
        };
        self.report.connections = self.report.connections.saturating_add(1);
        let started = Instant::now();
        let result = self
            .worker
            .as_mut()
            .ok_or(WorkerListenerError::WorkerConsumed)?
            .serve_stream_socket(&mut stream, &mut self.admission);
        record_transport(&self.telemetry, started, &result);
        clear_active_tcp_stream(&self.active_stream);
        match result {
            Ok(frames) => self.report.frames = self.report.frames.saturating_add(frames),
            Err(_) => self.report.failures = self.report.failures.saturating_add(1),
        }
        Ok(true)
    }

    /// Returns the worker after clean shutdown state has been observed.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn into_worker(mut self) -> Result<WorkerService<E, S>, WorkerListenerError> {
        self.worker
            .take()
            .map(|mut worker| {
                worker.close();
                worker
            })
            .ok_or(WorkerListenerError::WorkerConsumed)
    }
}

#[cfg(any(unix, windows))]
impl<E, S, R, A> fmt::Debug for UnixWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R> + fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UnixWorkerListener")
            .field("path", &self.path)
            .field("worker", &self.worker)
            .field("config", &self.config)
            .field("report", &self.report)
            .finish_non_exhaustive()
    }
}

#[cfg(any(unix, windows))]
impl<E, S, R, A> UnixWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    /// Binds a private worker endpoint.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn bind(
        worker: WorkerService<E, S>,
        admission: A,
        config: WorkerListenerConfig,
    ) -> Result<Self, WorkerListenerError> {
        Self::bind_with_peer_policy(worker, admission, config, Arc::new(FilesystemPeerPolicy))
    }

    /// Binds a worker endpoint with an explicit peer credential policy.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn bind_with_peer_policy(
        worker: WorkerService<E, S>,
        admission: A,
        config: WorkerListenerConfig,
        peer_policy: Arc<dyn PeerPolicy>,
    ) -> Result<Self, WorkerListenerError> {
        config.validate()?;
        prepare_socket_path(&config.path)?;
        let listener = backend_engine::LocalListener::bind(&config.path)
            .map_err(|error| WorkerListenerError::Io(error.kind()))?;
        set_private_socket_permissions(&config.path)?;
        listener
            .set_nonblocking(true)
            .map_err(|error| WorkerListenerError::Io(error.kind()))?;
        Ok(Self {
            path: config.path.clone(),
            listener,
            worker: Some(worker),
            admission,
            config,
            stop: AtomicBool::new(false),
            peer_policy,
            active_stream: Arc::new(Mutex::new(None)),
            report: WorkerRunReport::default(),
            telemetry: backend_engine::Telemetry::disabled(),
            relation: PhantomData,
        })
    }

    /// Installs bounded telemetry shared with the process owner.
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

    /// Returns the bound endpoint path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Requests a clean endpoint shutdown.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(stream) = self.active_stream.lock()
            && let Some(stream) = stream.as_ref()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Returns whether shutdown was requested.
    #[must_use]
    pub fn is_shutdown(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Returns work processed so far.
    #[must_use]
    pub const fn report(&self) -> WorkerRunReport {
        self.report
    }

    /// Runs until shutdown. The listener remains nonblocking between clients,
    /// while each stream has the configured bounded read/write timeout.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn run(&mut self) -> Result<WorkerRunReport, WorkerListenerError> {
        while !self.is_shutdown() {
            match self.listener.accept() {
                Ok((mut stream, _address)) => {
                    if self.peer_policy.authorize(&stream).is_err() {
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    }
                    if configure_stream(&stream, self.config.limits.io_timeout).is_err() {
                        self.report.failures = self.report.failures.saturating_add(1);
                        continue;
                    }
                    set_active_stream(&self.active_stream, &stream);
                    self.report.connections = self.report.connections.saturating_add(1);
                    let started = Instant::now();
                    let result = self
                        .worker
                        .as_mut()
                        .ok_or(WorkerListenerError::WorkerConsumed)?
                        .serve_stream_socket(&mut stream, &mut self.admission);
                    record_transport(&self.telemetry, started, &result);
                    clear_active_stream(&self.active_stream);
                    match result {
                        Ok(frames) => {
                            self.report.frames = self.report.frames.saturating_add(frames);
                        }
                        Err(_error) => {
                            self.report.failures = self.report.failures.saturating_add(1);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => return Err(WorkerListenerError::Io(error.kind())),
            }
        }
        if let Some(worker) = self.worker.as_mut() {
            worker.close();
        }
        Ok(self.report)
    }

    /// Runs one nonblocking accept/serve iteration.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn run_once(&mut self) -> Result<bool, WorkerListenerError> {
        if self.is_shutdown() {
            return Ok(false);
        }
        match self.listener.accept() {
            Ok((mut stream, _address)) => {
                if self.peer_policy.authorize(&stream).is_err() {
                    self.report.failures = self.report.failures.saturating_add(1);
                    return Ok(true);
                }
                configure_stream(&stream, self.config.limits.io_timeout)?;
                set_active_stream(&self.active_stream, &stream);
                self.report.connections = self.report.connections.saturating_add(1);
                let started = Instant::now();
                let result = self
                    .worker
                    .as_mut()
                    .ok_or(WorkerListenerError::WorkerConsumed)?
                    .serve_stream_socket(&mut stream, &mut self.admission);
                record_transport(&self.telemetry, started, &result);
                clear_active_stream(&self.active_stream);
                match result {
                    Ok(frames) => self.report.frames = self.report.frames.saturating_add(frames),
                    Err(_error) => self.report.failures = self.report.failures.saturating_add(1),
                }
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(error) => Err(WorkerListenerError::Io(error.kind())),
        }
    }

    /// Returns the worker after clean shutdown state has been observed.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn into_worker(mut self) -> Result<WorkerService<E, S>, WorkerListenerError> {
        self.worker
            .take()
            .map(|mut worker| {
                worker.close();
                worker
            })
            .ok_or(WorkerListenerError::WorkerConsumed)
    }
}

fn record_transport<E>(
    telemetry: &backend_engine::Telemetry,
    started: Instant,
    result: &Result<usize, E>,
) {
    telemetry.record_with(|| backend_engine::Observation {
        family: backend_engine::MetricFamily::Transport,
        outcome: if result.is_ok() {
            backend_engine::MetricOutcome::Completed
        } else {
            backend_engine::MetricOutcome::Failed
        },
        latency: started.elapsed(),
        units: result
            .as_ref()
            .map_or(0, |frames| u64::try_from(*frames).unwrap_or(u64::MAX)),
    });
}

#[cfg(any(unix, windows))]
impl<E, S, R, A> Drop for UnixWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.path);
    }
}

impl<E, S, R, A> Drop for TcpWorkerListener<E, S, R, A>
where
    E: PureRecipeExecutor,
    S: WorkerAttestationSigner,
    R: Relation + Send,
    A: JobAdmission<R>,
{
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(stream) = self.active_stream.lock()
            && let Some(stream) = stream.as_ref()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Worker listener failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkerListenerError {
    /// Invalid path or worker limit configuration.
    InvalidConfig,
    /// A non-socket path occupies the endpoint.
    EndpointOccupied,
    /// Another worker currently owns the endpoint.
    AlreadyRunning,
    /// Filesystem or socket I/O failed.
    Io(io::ErrorKind),
    /// Canonical worker protocol admission failed.
    Protocol(WorkerProtocolError),
    /// TCP authority possession handshake failed.
    Authentication(backend_engine::TcpHandshakeError),
    /// A routable plaintext TCP listener lacked an explicit outer secure
    /// transport declaration.
    ConfidentialityRequired,
    /// The listener's worker has already been taken by `into_worker`.
    WorkerConsumed,
}

impl fmt::Display for WorkerListenerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig => formatter.write_str("invalid worker listener configuration"),
            Self::EndpointOccupied => {
                formatter.write_str("worker endpoint is occupied by a non-socket")
            }
            Self::AlreadyRunning => formatter.write_str("worker endpoint is already in use"),
            Self::Io(kind) => write!(formatter, "worker listener I/O failed: {kind:?}"),
            Self::Protocol(error) => error.fmt(formatter),
            Self::Authentication(error) => error.fmt(formatter),
            Self::ConfidentialityRequired => formatter
                .write_str("routable worker TCP requires an explicitly protected outer transport"),
            Self::WorkerConsumed => formatter.write_str("worker service was already consumed"),
        }
    }
}

impl std::error::Error for WorkerListenerError {}

#[cfg(all(test, unix))]
#[path = "listener/tests.rs"]
mod tests;

#[cfg(not(any(unix, windows)))]
/// Worker Unix listeners are unavailable on non-Unix targets.
pub struct UnixWorkerListener<E, S, R, A>(std::marker::PhantomData<(E, S, R, A)>);
