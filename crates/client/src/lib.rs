//! Shared, proof-admitting client for every local product surface.
//!
//! CLI, MCP, and desktop hosts differ only in presentation and lifecycle.
//! This crate owns their single command transport, error algebra, framing
//! bounds, and reply admission path.
#![forbid(unsafe_code)]

mod backoff;
#[cfg(any(unix, windows))]
mod deferred_command;
pub mod lease_contract;
pub mod monotonic;
#[cfg(any(unix, windows))]
mod owner_connection;
#[cfg(any(unix, windows))]
pub use owner_connection::connect_local_owner_with;
#[cfg(any(unix, windows))]
mod query_continuation;
#[cfg(any(unix, windows))]
mod query_preparation;
pub use backoff::Backoff;
#[cfg(any(unix, windows))]
pub use query_preparation::QueryPreparationProgress;
#[cfg(any(unix, windows))]
mod remote_command;
#[cfg(any(unix, windows))]
mod reset_budget;
#[cfg(any(unix, windows))]
mod reset_hydration;
#[cfg(any(unix, windows))]
mod semantic_range_local;
mod subscription;
#[cfg(any(unix, windows))]
mod subscription_local;
#[cfg(any(unix, windows))]
mod subscription_observation;
#[cfg(all(test, any(unix, windows)))]
mod test_socket;

#[cfg(any(unix, windows))]
pub use remote_command::RemoteIndexCommandTransport;
#[cfg(any(unix, windows))]
pub use semantic_range_local::{
    LocalSemanticIndexClient, LocalSemanticRangeTransport, SemanticCatalogSnapshot,
    SemanticImageFetch,
};
pub use subscription::{
    CertifiedSubscriptionTransport, SubscriptionRequest, SubscriptionTransport,
    snapshot_page_from_bytes, snapshot_page_from_value,
};
#[cfg(any(unix, windows))]
pub use subscription_local::{LocalSubscriptionExchangeError, LocalSubscriptionTransport};
#[cfg(any(unix, windows))]
pub use subscription_observation::{
    ObservedPublicationControl, PublicationBudgetKind, PublicationExchangeBudget,
    PublicationExchangeError, PublicationLease, PublicationObservationDecision,
    PublicationObservationProgress, PublicationOperation,
};

use backend_library::{
    AdmittedGraphQueryInput, Command, CommandDto, CommandFailure, CommandMutation, CommandReply,
    CompileExecutionIntent, CoverageCapability, DeclarationRead, DeclarationSelector, DiffRecord,
    DocumentQuery, GraphNeighborhoodQuery, GraphQueryPage, GraphQueryRequest, GraphValue,
    HealthReport, IndexCancelReceipt, IndexJobObservation, IndexJobTerminal, IndexJobTicket,
    IndexOperationKey, IndexOperationObservation, IndexStartResult, NameQuery, OutlineQuery,
    PackageReference, PackageSourceMembershipPageRequestV1, PackageSourceMembershipPageResultV1,
    PageContinuation, PageRequest, PageTerminal, Query, QueryLimit, ReplyAdmissionError, ReplyDto,
    RequestAdmissionError, SemanticGenerationId, SemanticLanguageProfile, SemanticShapeBatch,
    SemanticShapeBudget, SemanticShapeRequest, SemanticVersionRecord, SurfaceCommand, SurfaceReply,
    SymbolAddress, SymbolKey, ViewProjectionError, ViewStateRoot, WireCertificate, WireClaim,
    WireSchema, encode_id, package_key, symbol_key,
};
mod request_context;
pub use request_context::RequestCancellationReason;
#[cfg(any(unix, windows))]
pub use request_context::RequestContext;
mod index_operation;
mod index_progress;
pub use backend_replication::SelectedGenerationStamp as SelectedStamp;
use backend_replication::{
    LocalControlError, LocalControlLimits, ReplicationError, read_frame, write_frame,
};
pub use backend_runtime::{StartupFailure, StartupFailureCause, StartupRecovery};
pub use index_operation::{
    IndexJournalDispatch, JournaledIndexStart, index_with_journal, index_with_journal_recovery,
    surface_with_index_journal,
};
#[cfg(any(unix, windows))]
pub use index_operation::{index_with_journal_recovery_wait, index_with_journal_wait};
pub use index_progress::{
    IndexCompilerProgressUpdate, IndexObservationFingerprint, IndexOperationProgress,
    IndexOperationWaitResult, IndexOperationWaitStop, IndexProgressRead, IndexProgressReference,
    IndexProgressUpdate, MAX_INDEX_OPERATION_WAIT, MAX_INDEX_PROGRESS_WAIT_MILLIS,
};
#[cfg(any(unix, windows))]
pub use index_progress::{
    observe_index_operation_progress, read_index_operation_progress, read_index_progress,
    wait_index_operation, wait_index_operation_with_progress,
};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
#[cfg(any(unix, windows))]
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

/// A bounded cancellation handle for one local transport connection.
///
/// The handle contains only a cloned socket. It never sends or replays a
/// command; an owner may already have accepted a mutation when a caller
/// interrupts its reply wait.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub struct TransportInterrupt(Arc<InterruptState>);

#[cfg(any(unix, windows))]
struct InterruptState {
    stream: Mutex<backend_replication::LocalStream>,
    interrupted: AtomicBool,
}

#[cfg(all(test, unix))]
mod transport_interrupt_tests {
    use super::TransportInterrupt;
    use std::io::Read as _;
    use std::time::{Duration, Instant};

    #[test]
    fn interrupt_releases_a_blocked_local_read() {
        let (mut reader, _owner) = std::os::unix::net::UnixStream::pair().expect("socket pair");
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read deadline");
        let interrupt = TransportInterrupt::new(&reader).expect("clone exact socket");
        let waiting = std::thread::spawn(move || {
            let mut byte = [0_u8; 1];
            reader.read(&mut byte)
        });
        let started = Instant::now();
        interrupt.interrupt();
        let result = waiting.join().expect("join released read");
        assert!(matches!(result, Ok(0) | Err(_)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "socket cancellation waited for its read deadline"
        );
    }

    #[test]
    fn interrupted_handle_closes_a_replacement_socket_too() {
        let (old, _old_owner) = std::os::unix::net::UnixStream::pair().expect("old socket pair");
        let (mut replacement, _new_owner) =
            std::os::unix::net::UnixStream::pair().expect("new socket pair");
        replacement
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("read deadline");
        let interrupt = TransportInterrupt::new(&old).expect("clone old socket");
        interrupt.interrupt();
        interrupt
            .replace(&replacement)
            .expect("replace cancelled socket");
        let mut byte = [0_u8; 1];
        let started = Instant::now();
        let result = replacement.read(&mut byte);
        assert!(matches!(result, Ok(0) | Err(_)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "replacement survived cancellation"
        );
    }
}

#[cfg(any(unix, windows))]
impl TransportInterrupt {
    fn new(stream: &backend_replication::LocalStream) -> Result<Self, ClientError> {
        stream
            .try_clone()
            .map(|stream| {
                Self(Arc::new(InterruptState {
                    stream: Mutex::new(stream),
                    interrupted: AtomicBool::new(false),
                }))
            })
            .map_err(|error| ClientError::Io(error.to_string()))
    }

    fn replace(&self, stream: &backend_replication::LocalStream) -> Result<(), ClientError> {
        let replacement = stream
            .try_clone()
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let mut current = self.0.stream.lock().unwrap_or_else(PoisonError::into_inner);
        *current = replacement;
        if self.0.interrupted.load(Ordering::Acquire) {
            let _ = current.shutdown(std::net::Shutdown::Both);
        }
        Ok(())
    }

    /// Interrupts a blocking read or write on the current connection.
    pub fn interrupt(&self) {
        self.0.interrupted.store(true, Ordering::Release);
        let stream = self.0.stream.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// Maximum admitted local command/reply body.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;
/// Maximum events admitted in one interactive subscription batch.
pub const MAX_EVENTS: usize = backend_library::MAX_SUBSCRIPTION_EVENTS;

/// One closed failure type shared by all command clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    /// An older local owner could not complete a safe pre-command handoff.
    OwnerUpdate {
        /// Wire version actually observed from the correlated health probe.
        found: u16,
        /// Wire version implemented by this matched client.
        supported: u16,
        /// Actionable custody, shutdown or platform limitation.
        detail: String,
    },
    /// The endpoint or stream could not be used.
    Io(String),
    /// The actual automatic owner-startup cause and its runtime recovery policy.
    Startup(StartupFailure),
    /// The connection is gone: the peer closed it, reset it, or stopped
    /// answering on it.
    ///
    /// This is separate from [`ClientError::Io`] because the request itself
    /// may be valid and the client can recover by opening a new connection.
    Disconnected(std::io::ErrorKind),
    /// A connection failed while exchanging an exact pinned search or graph
    /// query, including its first exchange. A reconnect must not silently
    /// submit that read at a new revision.
    QueryPreparationDisconnected {
        /// Original immutable view selected by the read.
        basis: backend_library::ViewRevision,
        /// Actual connection failure, including an expired exchange budget.
        kind: std::io::ErrorKind,
    },
    /// A non-repeatable command was in flight when its connection closed, so
    /// the owner may or may not have admitted it. A replacement connection
    /// does not resolve that ambiguity.
    MutationOutcomeUnknown(std::io::ErrorKind),
    /// A caller-journaled index start has no admitted outcome. Its original
    /// identity and the actual delivery/replacement failure remain resumable.
    IndexOperationDeliveryUnknown {
        /// Key durably saved before the original start was sent.
        operation_key: IndexOperationKey,
        /// Original transport, replacement startup, or receipt admission cause.
        cause: Box<ClientError>,
    },
    /// A remote-index connect or read-only request exceeded its bounded client lease.
    RemoteDeadlineExceeded,
    /// Caller cancellation, never a reconnectable endpoint failure.
    RequestCancelled(RequestCancellationReason),
    /// A frame, DTO, or identity proof failed admission.
    Protocol(String),
    /// A bounded transport allocation was rejected.
    Transport(ReplicationError),
    /// A native reply exceeded its byte/allocation ceiling after request admission.
    /// Pure windowed reads may reduce delivery; request/proof overflow is distinct.
    ResponseTooLarge {
        /// Immutable catalog revision supplied to the refused native request.
        basis: Option<backend_library::ViewRevision>,
    },
    /// Only the reply frame header was consumed. The transport is unusable.
    ResponseFrameTooLarge {
        /// Immutable catalog revision supplied to the refused native request.
        basis: Option<backend_library::ViewRevision>,
    },
    /// The application service rejected an admitted command.
    CommandFailed(CommandFailure),
    /// A successful view reply was not coherent.
    IncoherentView,
    /// A reply was based on a different materialized revision.
    BasisMismatch {
        /// Revision requested by the client.
        expected: ViewStateRoot,
        /// Revision returned by the daemon.
        observed: ViewStateRoot,
    },
    /// A reply did not prove accepted freshness.
    FreshnessMismatch,
    /// The daemon replied to another request.
    RequestMismatch {
        /// Request sent by the client.
        expected: u64,
        /// Request returned by the daemon.
        observed: u64,
    },
    /// A continuation cursor did not identify the returned root.
    CursorMismatch,
    /// A continuation was issued for an older immutable view revision.
    ///
    /// This is intentionally distinct from malformed protocol input: callers
    /// can discard the token and restart the same query against the current
    /// revision without treating the daemon as unhealthy.
    StaleCursor,
    /// The exact selected semantic generation changed during a read or range admission.
    StaleSelection,
    /// The signed remote grant names a product root that is no longer selected.
    StaleRemoteRoot {
        /// Exact view root authorized by the grant.
        expected: [u8; 32],
        /// Current owner view root.
        observed: [u8; 32],
    },
    /// The owner rejected an expired or replaced remote grant.
    StaleRemoteCapability,
    /// The owner revoked this exact remote grant.
    RemoteCapabilityRevoked,
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "local endpoint: {message}"),
            Self::Startup(failure) => write!(formatter, "local startup: {failure}"),
            Self::Disconnected(kind) => {
                write!(formatter, "endpoint disconnected: {kind}")
            }
            Self::QueryPreparationDisconnected { basis, kind } => write!(
                formatter,
                "pinned query for revision {} lost its connection ({kind}); the read was not rebound",
                encode_id(basis.as_bytes())
            ),
            Self::MutationOutcomeUnknown(kind) => {
                write!(
                    formatter,
                    "mutation outcome is unknown after endpoint disconnect: {kind}"
                )
            }
            Self::IndexOperationDeliveryUnknown {
                operation_key,
                cause,
            } => write!(
                formatter,
                "delivery of saved index operation {operation_key} is unconfirmed; it may already be queued or accepted; no receipt has been observed; transport/recovery observation: {cause}"
            ),
            Self::RemoteDeadlineExceeded => {
                formatter.write_str("remote index exceeded the bounded client request deadline")
            }
            Self::RequestCancelled(reason) => write!(formatter, "{reason}"),
            Self::OwnerUpdate {
                found,
                supported,
                detail,
            } => {
                if found == supported {
                    write!(formatter, "NuDox owner build update: {detail}")
                } else {
                    write!(
                        formatter,
                        "NuDox owner update (version {found} → {supported}): {detail}"
                    )
                }
            }
            Self::Protocol(message) => write!(formatter, "protocol: {message}"),
            Self::Transport(error) => write!(formatter, "transport: {error}"),
            Self::ResponseTooLarge { .. } => {
                formatter.write_str("native reply exceeds its bounded byte/allocation ceiling")
            }
            Self::ResponseFrameTooLarge { .. } => formatter
                .write_str("native reply frame exceeds its ceiling; the connection was retired"),
            Self::CommandFailed(failure) => write!(formatter, "command failed: {failure}"),
            Self::IncoherentView => formatter.write_str("daemon returned an incoherent view"),
            Self::BasisMismatch { .. } => {
                formatter.write_str("daemon reply is based on a different revision")
            }
            Self::FreshnessMismatch => formatter.write_str("daemon reply did not prove freshness"),
            Self::RequestMismatch { expected, observed } => {
                write!(formatter, "request id {observed} does not match {expected}")
            }
            Self::CursorMismatch => formatter.write_str("daemon returned an invalid cursor"),
            Self::StaleCursor => {
                formatter.write_str("continuation cursor belongs to an older revision")
            }
            Self::StaleSelection => {
                formatter.write_str("selected semantic generation is no longer current")
            }
            Self::StaleRemoteRoot { .. } => {
                formatter.write_str("remote grant is stale; renew it for the current view root")
            }
            Self::StaleRemoteCapability => {
                formatter.write_str("remote grant is stale or expired; renew the grant")
            }
            Self::RemoteCapabilityRevoked => {
                formatter.write_str("remote grant was revoked by its owner")
            }
        }
    }
}

impl ClientError {
    /// Returns whether no owner answered the dial: the endpoint refused the
    /// connection or its socket file is gone.
    ///
    /// This is the one fact that justifies starting a replacement owner. A
    /// dead daemon leaves its socket file behind, so an absent owner usually
    /// shows up as a refused dial rather than a missing path. Slow or busy
    /// listeners (`TimedOut`, `WouldBlock`) are deliberately not absent: a
    /// live owner that is merely behind must never get a second candidate.
    #[must_use]
    pub fn is_owner_absent(&self) -> bool {
        matches!(self, Self::Disconnected(kind) if is_owner_absent_kind(*kind))
    }

    /// Returns whether the peer tore the connection down or was never there:
    /// reset, aborted, broken pipe, not connected, end of file, a closed peer
    /// reported as invalid input, or an absent owner
    /// ([`ClientError::is_owner_absent`]).
    ///
    /// Callers use it to decide that opening a fresh connection may help. It
    /// is not proof that the owner is gone: a healthy owner retires a
    /// connection that reached its frame budget, and a dying owner can still
    /// complete a dial before its socket closes. It excludes slow-listener
    /// timeouts and every protocol, authority or cancellation failure.
    #[must_use]
    pub fn is_connection_lost(&self) -> bool {
        matches!(self, Self::Disconnected(kind) if is_connection_lost_kind(*kind))
    }

    /// Returns whether the owner is listening but did not answer inside the
    /// read window: a request timed out (`TimedOut`, or `WouldBlock` where the
    /// platform reports a socket read timeout that way).
    ///
    /// This is the opposite fact from [`ClientError::is_owner_absent`]. A slow
    /// owner is a live owner, so it never authorizes starting another one; it
    /// is not [`ClientError::is_connection_lost`] either, because nothing
    /// proves the peer closed. A caller whose request is a read that can be
    /// repeated may drop the connection, dial the *existing* listener again,
    /// and ask again.
    #[must_use]
    pub fn is_owner_slow(&self) -> bool {
        matches!(self, Self::Disconnected(kind) if is_owner_slow_kind(*kind))
    }

    /// Returns whether the owner is unavailable *for now* in a way an
    /// idempotent read can simply outwait: the connection was lost, a live
    /// owner did not answer in time, or an owner candidate is still starting.
    ///
    /// It says nothing about whether a command took effect, so only a read
    /// that changes nothing (a poll, a pinned query, a keyed status read) may
    /// act on it by asking again. It never authorizes starting an owner: that
    /// takes [`ClientError::is_owner_absent`] alone.
    #[must_use]
    pub fn is_owner_gap(&self) -> bool {
        self.is_connection_lost() || self.is_owner_slow() || self.is_owner_starting()
    }

    /// Returns whether an owner candidate is still starting, so no command was
    /// submitted and the same bounded wait may be repeated.
    #[must_use]
    pub fn is_owner_starting(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(
                self,
                Self::Startup(failure) if failure.cause() == StartupFailureCause::OwnerPending
            )
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

impl From<backend_runtime::RuntimeError> for ClientError {
    fn from(error: backend_runtime::RuntimeError) -> Self {
        Self::Startup(error.into())
    }
}

impl std::error::Error for ClientError {}

/// Fallible command transport shared by every local interface.
pub trait CommandTransport {
    /// Sends one admitted command and returns one admitted reply.
    ///
    /// # Errors
    /// Returns a transport, protocol, correlation, or freshness error.
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError>;

    /// Sends a read with the remaining preparation-wait budget. Socket and
    /// remote transports bound their exchange by this duration; synchronous
    /// transports retain their ordinary request execution contract.
    fn request_with_budget(
        &mut self,
        request: CommandDto,
        _remaining: Duration,
    ) -> Result<ReplyDto, ClientError> {
        self.request(request)
    }

    /// Uses the default search's finite admission scope. Transports without
    /// authenticated local deferred admission retain the absolute initial budget.
    #[cfg(any(unix, windows))]
    fn request_with_search_admission(
        &mut self,
        request: CommandDto,
        remaining: Duration,
        _scope: &RequestContext,
    ) -> Result<ReplyDto, ClientError> {
        self.request_with_budget(request, remaining)
    }

    /// Returns a handle that can interrupt this transport's exact socket.
    /// Connectionless transports return `None`.
    #[cfg(any(unix, windows))]
    fn interrupt_handle(&self) -> Option<TransportInterrupt> {
        None
    }

    /// Carries the caller's request lifetime into asynchronous remote transports.
    #[cfg(any(unix, windows))]
    fn set_request_context(&mut self, _context: Option<RequestContext>) {}

    /// Replaces the underlying connection while retaining this transport's endpoint and grant.
    ///
    /// Connectionless transports may keep their existing state. Implementations that own a
    /// socket or remote session should open a fresh authenticated session here.
    fn reconnect(&mut self) -> Result<(), ClientError> {
        Ok(())
    }

    /// Request frames the owner will still accept on the current connection,
    /// or `None` for a transport with no per-connection frame budget.
    fn frames_remaining(&self) -> Option<usize> {
        None
    }

    /// Opens a fresh authenticated connection to the *same* endpoint within
    /// `budget`, observing the caller's cancellation. It never starts an owner.
    ///
    /// Returns `Ok(false)` for a transport that has no connection to reopen.
    ///
    /// # Errors
    /// Returns the dial or authentication failure of the replacement.
    #[cfg(any(unix, windows))]
    fn reconnect_within(
        &mut self,
        _budget: Duration,
        _context: Option<&RequestContext>,
    ) -> Result<bool, ClientError> {
        Ok(false)
    }
}

/// Command transport with an explicit coverage-capability path.
pub trait CertifiedCommandTransport: CommandTransport {
    /// Sends one command while retaining externally admitted coverage.
    ///
    /// # Errors
    /// Returns a transport, protocol, correlation, coverage, or freshness error.
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError>;
}

/// Compatibility seam for an embedded owner.
pub trait LocalEngine {
    /// Executes one already admitted command.
    fn execute(&mut self, request: CommandDto) -> ReplyDto;
}

/// Borrowed in-process transport with no state authority of its own.
pub struct InProcessTransport<'a, E: LocalEngine + ?Sized> {
    engine: &'a mut E,
}

impl<'a, E: LocalEngine + ?Sized> InProcessTransport<'a, E> {
    /// Borrows an embedded command owner.
    pub fn new(engine: &'a mut E) -> Self {
        Self { engine }
    }
}

impl<E: LocalEngine + ?Sized> CommandTransport for InProcessTransport<'_, E> {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        admit_request(&request)?;
        let reply = self.engine.execute(request.clone());
        admit_reply(&request, reply)
    }
}

impl<E: LocalEngine + ?Sized> CertifiedCommandTransport for InProcessTransport<'_, E> {
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        _capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError> {
        self.request(request)
    }
}

/// One authenticated Unix command connection.
#[cfg(any(unix, windows))]
pub struct UnixCommandTransport {
    stream: backend_replication::LocalStream,
    interrupt: Option<TransportInterrupt>,
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
    endpoint: Option<std::path::PathBuf>,
    connect_timeout: Duration,
    io_timeout: Duration,
    /// Request frames written on the current connection. The owner retires a
    /// connection after `frame_budget` frames, so these two say when the next
    /// frame would be refused.
    frames_sent: usize,
    /// Frames the owner serves per connection ([`connection_frame_budget`]).
    frame_budget: usize,
}

#[cfg(any(unix, windows))]
impl UnixCommandTransport {
    /// Connects and authenticates the local endpoint owner.
    ///
    /// # Errors
    /// Returns an error when the endpoint is invalid, unavailable, or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(path, LOCAL_CONNECT_TIMEOUT, CLIENT_REQUEST_TIMEOUT)
    }

    /// Connects and authenticates the local endpoint owner within `timeout`.
    ///
    /// # Errors
    /// Returns an error when the endpoint is invalid, unavailable, times out, or cannot authenticate.
    pub fn connect_timeout(path: impl AsRef<Path>, timeout: Duration) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(path, timeout, CLIENT_REQUEST_TIMEOUT)
    }

    /// Connects and authenticates the endpoint with independent dial and I/O deadlines.
    ///
    /// # Errors
    /// Returns an error when the endpoint is invalid, unavailable, times out, or cannot authenticate.
    pub fn connect_with_timeouts(
        path: impl AsRef<Path>,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Self, ClientError> {
        Self::connect_with_context(path, connect_timeout, io_timeout, None)
    }

    /// Connects and authenticates inside one caller's cancellation lifetime.
    /// # Errors
    /// Returns cancellation or the original bounded dial/authentication error.
    pub fn connect_with_context(
        path: impl AsRef<Path>,
        connect_timeout: Duration,
        io_timeout: Duration,
        context: Option<&RequestContext>,
    ) -> Result<Self, ClientError> {
        Self::connect_before(path, connect_timeout, io_timeout, context, None)
    }

    // The owner compatibility handshake shares one absolute startup deadline
    // across dial, authentication and its later read-only probe. Ordinary
    // transports retain their existing independent dial/I/O budget semantics.
    fn connect_before(
        path: impl AsRef<Path>,
        connect_timeout: Duration,
        io_timeout: Duration,
        context: Option<&RequestContext>,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self, ClientError> {
        let endpoint = backend_replication::UnixEndpointRef::new(path.as_ref())
            .map_err(|_| ClientError::Transport(ReplicationError::MessageTooLarge))?;
        let path = endpoint.as_path();
        let started = std::time::Instant::now();
        let stream = loop {
            if let Some(context) = context {
                context.check()?;
            }
            let remaining = connect_timeout.saturating_sub(started.elapsed());
            let remaining = Self::connection_budget(remaining, deadline, context)?;
            if remaining.is_zero() {
                return Err(map_endpoint_connect_error(
                    std::io::ErrorKind::TimedOut.into(),
                ));
            }
            let slice = if context.is_some() {
                remaining.min(Duration::from_millis(25))
            } else {
                remaining
            };
            match backend_replication::connect_local_timeout(path, slice) {
                Ok(stream) => break stream,
                Err(error)
                    if context.is_some()
                        && matches!(
                            error.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(error) => {
                    if let Some(context) = context {
                        context.check()?;
                    }
                    return Err(map_endpoint_connect_error(error));
                }
            }
        };
        let interrupt = Some(TransportInterrupt::new(&stream)?);
        if let Some(context) = context {
            context.bind_transport(interrupt.clone())?;
        }
        // Authentication also touches the new stream, so establish its I/O
        // deadlines before asking the local peer proof helper to inspect it.
        let authentication_budget = Self::connection_budget(io_timeout, deadline, context)?;
        configure_timeout(&stream, authentication_budget)?;
        let peer = match backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path) {
            Ok(peer) => peer,
            Err(error) => {
                if let Some(context) = context {
                    context.check()?;
                }
                Self::connection_budget(io_timeout, deadline, context)?;
                return Err(map_peer_authentication_error(error));
            }
        };
        if let Some(context) = context {
            context.check()?;
        }
        Self::connection_budget(io_timeout, deadline, context)?;
        Ok(Self {
            stream,
            interrupt,
            peer: Some(peer),
            endpoint: Some(path.to_path_buf()),
            connect_timeout,
            io_timeout,
            frames_sent: 0,
            frame_budget: connection_frame_budget(),
        })
    }

    fn connection_budget(
        ceiling: Duration,
        deadline: Option<std::time::Instant>,
        context: Option<&RequestContext>,
    ) -> Result<Duration, ClientError> {
        let mut remaining = ceiling;
        if let Some(context) = context {
            context.check()?;
            if let Some(budget) = context.remaining_read_budget()? {
                remaining = remaining.min(budget);
            }
        }
        if let Some(deadline) = deadline {
            remaining = remaining.min(deadline.saturating_duration_since(std::time::Instant::now()));
            if remaining.is_zero() {
                return Err(ClientError::RemoteDeadlineExceeded);
            }
        }
        Ok(remaining)
    }

    /// Wraps a connected stream for tests and embedded transports.
    #[must_use]
    pub fn from_stream(stream: backend_replication::LocalStream) -> Self {
        let _ = configure(&stream);
        let interrupt = TransportInterrupt::new(&stream).ok();
        Self {
            stream,
            interrupt,
            peer: None,
            endpoint: None,
            connect_timeout: LOCAL_CONNECT_TIMEOUT,
            io_timeout: CLIENT_REQUEST_TIMEOUT,
            frames_sent: 0,
            frame_budget: connection_frame_budget(),
        }
    }

    /// Decodes one reply against a caller-owned exact expectation.
    ///
    /// # Errors
    /// Returns an error when framing, decoding, or exact reply admission fails.
    pub fn request_against(
        &mut self,
        request: &CommandDto,
        expected: &ReplyDto,
    ) -> Result<ReplyDto, ClientError> {
        let accepted = request.clone();
        configure_request_with_timeouts(
            &self.stream,
            request,
            self.io_timeout,
            CLIENT_MUTATION_TIMEOUT,
        )?;
        let body = encode_request(request)?;
        let body = self.exchange_command(request, &body)?;
        let reply = ReplyDto::decode_against(&body, expected).map_err(ClientError::Protocol)?;
        admit_reply(&accepted, reply)
    }

    fn decode(&self, body: &[u8]) -> Result<ReplyDto, ClientError> {
        match self.peer.as_ref() {
            Some(peer) => backend_library::decode_reply_body_with_verifier(body, peer)
                .map_err(ClientError::Protocol),
            None => backend_library::decode_reply_body(body).map_err(ClientError::Protocol),
        }
    }
}

#[cfg(any(unix, windows))]
impl CommandTransport for UnixCommandTransport {
    fn interrupt_handle(&self) -> Option<TransportInterrupt> {
        self.interrupt.clone()
    }

    fn frames_remaining(&self) -> Option<usize> {
        Some(self.frame_budget.saturating_sub(self.frames_sent))
    }

    fn reconnect_within(
        &mut self,
        budget: Duration,
        context: Option<&RequestContext>,
    ) -> Result<bool, ClientError> {
        let Some(endpoint) = self.endpoint.clone() else {
            return Ok(false);
        };
        let replacement = Self::connect_with_context(
            endpoint,
            self.connect_timeout.min(budget),
            self.io_timeout,
            context,
        )?;
        // The replacement already bound its own interrupt to `context`, so it
        // is adopted whole rather than retargeting the retired one.
        self.stream = replacement.stream;
        self.interrupt = replacement.interrupt;
        self.peer = replacement.peer;
        self.endpoint = replacement.endpoint;
        self.frames_sent = 0;
        Ok(true)
    }

    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        configure_request_with_timeouts(
            &self.stream,
            &request,
            self.io_timeout,
            CLIENT_MUTATION_TIMEOUT,
        )?;
        let body = encode_request(&request)?;
        let body = self.exchange_command(&request, &body)?;
        let reply = self.decode(&body)?;
        admit_reply(&request, reply)
    }

    fn request_with_budget(
        &mut self,
        request: CommandDto,
        remaining: Duration,
    ) -> Result<ReplyDto, ClientError> {
        let started = std::time::Instant::now();
        if remaining.is_zero() {
            return Err(ClientError::RemoteDeadlineExceeded);
        }
        configure_request_with_timeouts(
            &self.stream,
            &request,
            self.io_timeout.min(remaining),
            CLIENT_MUTATION_TIMEOUT,
        )?;
        let body = encode_request(&request)?;
        let remaining = remaining.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ClientError::RemoteDeadlineExceeded);
        }
        let body = self.exchange_command_with_budget(&request, &body, remaining)?;
        let reply = self.decode(&body)?;
        admit_reply(&request, reply)
    }

    fn request_with_search_admission(
        &mut self,
        request: CommandDto,
        remaining: Duration,
        scope: &RequestContext,
    ) -> Result<ReplyDto, ClientError> {
        let started = std::time::Instant::now();
        if remaining.is_zero() {
            return Err(ClientError::RemoteDeadlineExceeded);
        }
        configure_request_with_timeouts(
            &self.stream,
            &request,
            self.io_timeout.min(remaining),
            CLIENT_MUTATION_TIMEOUT,
        )?;
        let body = encode_request(&request)?;
        let remaining = remaining.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(ClientError::RemoteDeadlineExceeded);
        }
        let body =
            self.exchange_command_with_search_admission(&request, &body, remaining, scope)?;
        scope.check()?;
        let reply = self.decode(&body)?;
        admit_reply(&request, reply)
    }

    fn reconnect(&mut self) -> Result<(), ClientError> {
        let endpoint = self.endpoint.clone().ok_or_else(|| {
            ClientError::Protocol(
                "stream-backed command transport has no reconnect endpoint".to_owned(),
            )
        })?;
        let connect_timeout = self.connect_timeout;
        let io_timeout = self.io_timeout;
        let replacement = Self::connect_with_timeouts(endpoint, connect_timeout, io_timeout)?;
        if let Some(interrupt) = &self.interrupt {
            interrupt.replace(&replacement.stream)?;
        }
        self.stream = replacement.stream;
        self.peer = replacement.peer;
        self.endpoint = replacement.endpoint;
        self.frames_sent = 0;
        Ok(())
    }
}

#[cfg(any(unix, windows))]
impl CertifiedCommandTransport for UnixCommandTransport {
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError> {
        configure_request_with_timeouts(
            &self.stream,
            &request,
            self.io_timeout,
            CLIENT_MUTATION_TIMEOUT,
        )?;
        let body = encode_request(&request)?;
        let body = self.exchange_command(&request, &body)?;
        let reply = if capability.is_some() {
            ReplyDto::decode_with_certificate(&body, capability.clone())
                .map_err(ClientError::Protocol)?
        } else {
            self.decode(&body)?
        };
        admit_reply_with_capability(&request, reply, capability)
    }
}

/// One connected, revision-aware local product session.
///
/// The session obtains the current immutable root and its producer proof only
/// for commands that need freshness. Callers never assemble basis flags or
/// identity certificates themselves.
#[cfg(any(unix, windows))]
pub struct Session {
    endpoint: std::path::PathBuf,
    transport: Box<dyn CommandTransport + Send>,
    request_context: Option<RequestContext>,
    search_wait_scope: Option<RequestContext>,
    next_request_id: u64,
    admitted_owner: Option<backend_library::Cursor>,
    admitted_source: Option<backend_library::SemanticObject>,
    prepared_query: Option<CommandDto>,
    continuations: BTreeMap<backend_library::Cursor, RetainedContinuation>,
    preparation_progress: Option<Box<dyn FnMut(QueryPreparationProgress) -> bool + Send>>,
}

/// A producer-admitted page and the distinct full owner revision that issued it.
#[cfg(any(unix, windows))]
struct RetainedContinuation {
    certificate: WireCertificate,
    owner: backend_library::Cursor,
    next_request: Option<CommandDto>,
    selected_coordinate: Option<PinnedSelectedCoordinate>,
}

/// The producer certificate state needed to resume one bounded page after a
/// transport reconnect.
///
/// A continuation is an owner-issued identity, not a property of one socket.
/// Keeping the certificate alongside the typed cursor lets a replacement
/// session re-admit the same page request against its current revision. The
/// map is intentionally private so callers cannot manufacture a continuation
/// by inserting raw identities.
#[cfg(any(unix, windows))]
#[derive(Default)]
pub struct SessionContinuationState {
    continuations: BTreeMap<backend_library::Cursor, RetainedContinuation>,
}

/// One admitted health revision retained long enough to build a dependent
/// query request without copying the view.
#[cfg(any(unix, windows))]
pub struct Revision {
    /// Current immutable product view root.
    pub root: ViewStateRoot,
    /// Exact owner source object whose producer coverage admitted this root.
    source: backend_library::SemanticObject,
    certificate: WireCertificate,
    cursor: backend_library::Cursor,
}

#[cfg(any(unix, windows))]
impl Revision {
    /// Returns the exact owner cursor paired with this immutable root.
    #[must_use]
    pub const fn cursor(&self) -> backend_library::Cursor {
        self.cursor
    }

    /// Returns the exact producer-coverage source object paired with this root.
    #[must_use]
    pub const fn source(&self) -> backend_library::SemanticObject {
        self.source
    }
}

#[cfg(any(unix, windows))]
impl Session {
    /// Connects one revision-aware session.
    ///
    /// # Errors
    /// Returns an error when the local endpoint is unavailable or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(path, LOCAL_CONNECT_TIMEOUT, CLIENT_REQUEST_TIMEOUT)
    }

    /// Connects one revision-aware session with independent dial and I/O
    /// limits. A cancelled caller can interrupt subsequent socket I/O.
    ///
    /// # Errors
    /// Returns an error when the endpoint cannot be admitted within its bound.
    pub fn connect_with_timeouts(
        path: impl AsRef<Path>,
        connect_timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Self, ClientError> {
        let endpoint = path.as_ref().to_path_buf();
        Ok(Self {
            transport: Box::new(UnixCommandTransport::connect_with_timeouts(
                &endpoint,
                connect_timeout,
                io_timeout,
            )?),
            endpoint,
            request_context: None,
            search_wait_scope: None,
            next_request_id: 1,
            admitted_owner: None,
            admitted_source: None,
            prepared_query: None,
            continuations: BTreeMap::new(),
            preparation_progress: None,
        })
    }

    /// Connects a session inside one request, including dial and authentication.
    /// # Errors
    /// Returns cancellation or the original connection admission failure.
    pub fn connect_with_context(
        path: impl AsRef<Path>,
        context: RequestContext,
    ) -> Result<Self, ClientError> {
        let endpoint = path.as_ref().to_path_buf();
        let transport = UnixCommandTransport::connect_with_context(
            &endpoint,
            LOCAL_CONNECT_TIMEOUT,
            CLIENT_REQUEST_TIMEOUT,
            Some(&context),
        )?;
        let mut session = Self::from_transport(endpoint, transport);
        session.set_request_context(Some(context))?;
        Ok(session)
    }

    /// Builds the same revision-aware product session over another admitted command transport.
    pub fn from_transport(
        endpoint: impl Into<std::path::PathBuf>,
        transport: impl CommandTransport + Send + 'static,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            transport: Box::new(transport),
            request_context: None,
            search_wait_scope: None,
            next_request_id: 1,
            admitted_owner: None,
            admitted_source: None,
            prepared_query: None,
            continuations: BTreeMap::new(),
            preparation_progress: None,
        }
    }

    /// Borrows the existing caller scope so bounded read retries preserve its
    /// absolute deadline and cancellation rather than renewing a request.
    #[must_use]
    pub fn request_context(&self) -> Option<&RequestContext> {
        self.request_context.as_ref()
    }

    /// Installs or retires the context for one caller-owned request.
    /// # Errors
    /// Returns cancellation if this request stopped before transport admission.
    pub fn set_request_context(
        &mut self,
        context: Option<RequestContext>,
    ) -> Result<(), ClientError> {
        self.transport.set_request_context(context.clone());
        self.request_context = context;
        if let Some(context) = &self.request_context {
            context.bind_transport(self.interrupt_handle())?;
        }
        Ok(())
    }

    fn check_request_context(&self) -> Result<(), ClientError> {
        self.request_context
            .as_ref()
            .map_or(Ok(()), RequestContext::check)
    }

    fn request_result<T>(&self, result: Result<T, ClientError>) -> Result<T, ClientError> {
        self.check_request_context()?;
        result
    }

    /// Returns the endpoint this session was connected to.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Returns how many more request frames the owner will accept on this
    /// connection, or `None` when the transport has no frame budget.
    ///
    /// The owner closes a connection once it has served its budget, whatever
    /// the next frame is. A caller about to send a command that must not be
    /// replayed should open a fresh connection first when this is small:
    /// writing it to a connection the owner is about to retire leaves its
    /// admission unknowable.
    #[must_use]
    pub fn connection_frames_remaining(&self) -> Option<usize> {
        self.transport.frames_remaining()
    }

    /// Returns a connection interrupt for a caller's request lifetime.
    /// In-process or remote transports may not expose one.
    #[must_use]
    pub fn interrupt_handle(&self) -> Option<TransportInterrupt> {
        self.transport.interrupt_handle()
    }

    /// Encodes an owner-issued query continuation for this authenticated MCP
    /// session. The bytes include the immutable owner identity and offset;
    /// the session retains the matching producer certificate so a reconnect
    /// can re-admit it without trusting raw cursor bytes.
    #[must_use]
    pub fn encode_page_continuation(&self, continuation: PageContinuation) -> String {
        let mut token = String::from("pc1-");
        for byte in continuation.cursor().encode_query().iter().copied() {
            use fmt::Write as _;
            let _ = write!(token, "{byte:02x}");
        }
        token
    }

    /// Decodes and admits a query continuation retained by this session
    /// against the current owner revision.
    ///
    /// # Errors
    /// Returns a protocol error when the token is malformed or was not issued
    /// by this admitted session. The current owner revision is checked
    /// separately so a stale root is reported as [`ClientError::StaleCursor`].
    pub fn decode_page_continuation(
        &mut self,
        token: &str,
    ) -> Result<PageContinuation, ClientError> {
        if token.starts_with("pc2-") || token.starts_with("pc3-") || token.starts_with("pc4-") {
            return self.decode_portable_query(token);
        }
        let encoded = token
            .strip_prefix("pc1-")
            .ok_or_else(|| ClientError::Protocol("unknown continuation token schema".to_owned()))?;
        if encoded.len() != backend_library::CURSOR_QUERY_BYTES.saturating_mul(2)
            && encoded.len() != backend_library::CURSOR_WINDOW_QUERY_BYTES.saturating_mul(2)
        {
            return Err(ClientError::Protocol(
                "malformed continuation token".to_owned(),
            ));
        }
        let bytes = decode_hex(encoded)
            .ok_or_else(|| ClientError::Protocol("malformed continuation token".to_owned()))?;
        // The recipe is query-specific, so it deliberately cannot be checked
        // against the ordinary owner cursor. Only a cursor retained from an
        // owner-admitted page may supply that typed recipe; raw token bytes
        // never become a new identity here. This is also what keeps a real
        // search, name, graph, or Trustfall cursor distinct from the view
        // cursor returned by `revision()`.
        let cursor = self
            .continuations
            .keys()
            .copied()
            .find(|cursor| cursor.encode_query().as_ref() == bytes.as_slice())
            .ok_or_else(|| ClientError::Protocol("unknown continuation token".to_owned()))?;
        // A page cursor commits to its projected rows, not the complete owner
        // view. Its certificate remains the authority for that projection;
        // freshness is checked against the separately admitted issuing owner.
        let issuing_owner = self.continuations[&cursor].owner;
        let owner = self.revision()?.cursor();
        if cursor.query_offset() == 0 || !issuing_owner.matches_owner(owner) {
            return Err(ClientError::StaleCursor);
        }
        Ok(PageContinuation::from_cursor(cursor))
    }

    /// Takes the owner-admitted continuation certificates before a product is
    /// replaced by a reconnecting transport.
    #[must_use]
    pub fn take_continuation_state(&mut self) -> SessionContinuationState {
        SessionContinuationState {
            continuations: std::mem::take(&mut self.continuations),
        }
    }

    /// Restores continuation certificates into a freshly authenticated
    /// session. They remain subject to the current owner-root check when used.
    pub fn restore_continuation_state(&mut self, state: SessionContinuationState) {
        self.continuations = state.continuations;
    }

    /// Replaces this session's connection with a fresh one to the same
    /// endpoint.
    ///
    /// Request numbering belongs to the connection, while remembered page
    /// continuations belong to the owner revision. Request IDs restart on the
    /// replacement stream; retained continuation certificates are admitted
    /// again against the replacement session's current owner root before a
    /// page request is sent.
    ///
    /// # Errors
    /// Returns an error when the endpoint is unavailable or cannot
    /// authenticate.
    pub fn reconnect(&mut self) -> Result<(), ClientError> {
        self.transport.reconnect()?;
        self.next_request_id = 1;
        Ok(())
    }

    /// Reads the current admitted product revision.
    ///
    /// # Errors
    /// Returns an error when the bounded revision reply fails transport or proof admission.
    pub fn revision(&mut self) -> Result<Revision, ClientError> {
        let reply = self.send_success(Command::Revision, None)?;
        self.admit_revision(reply)
    }

    fn revision_with_budget(&mut self, remaining: Duration) -> Result<Revision, ClientError> {
        let reply = self.send_success_with_budget(Command::Revision, None, Some(remaining))?;
        self.admit_revision(reply)
    }

    fn admit_revision(&mut self, reply: ReplyDto) -> Result<Revision, ClientError> {
        let certificate = reply.certificate().cloned().ok_or_else(|| {
            ClientError::Protocol("revision reply omitted its certificate".to_owned())
        })?;
        let CommandReply::Revision(receipt) = reply.reply else {
            return Err(ClientError::Protocol(
                "revision reply changed shape".to_owned(),
            ));
        };
        // The producer-root claim proves this revision while decoding the
        // reply. Dependent request codecs intentionally accept only a root
        // commitment, so retain the admitted identity in that narrower form
        // instead of requiring callers to reconstruct an authority claim.
        let certificate = certificate.with_claim_once(WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: encode_id(receipt.root().as_bytes()),
        });
        self.admitted_owner = Some(receipt.cursor());
        self.admitted_source = Some(receipt.source());
        Ok(Revision {
            root: receipt.root(),
            source: receipt.source(),
            certificate,
            cursor: receipt.cursor(),
        })
    }

    /// Reads the owner's constant-size health and readiness state.
    ///
    /// A legacy peer may still send a complete health view. The client lowers
    /// that compatibility reply into the same bounded report at this boundary.
    ///
    /// # Errors
    /// Returns an error when the health reply fails admission or changes shape.
    pub fn health(&mut self) -> Result<HealthReport, ClientError> {
        health_from_reply(self.send_success(Command::Health, None)?)
    }

    /// Lists indexed projects.
    ///
    /// # Errors
    /// Returns an error when the request or reply fails admission.
    pub fn packages(&mut self) -> Result<ReplyDto, ClientError> {
        self.send_success(Command::Packages, None)
    }

    /// Reads one bounded package page at the current immutable revision.
    ///
    /// # Errors
    /// Returns an error when revision, limit, transport, or reply admission fails.
    pub fn package_page(
        &mut self,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let page = page_request(revision.root, limit, continuation)?;
        self.send_success(
            Command::PackagePage(page),
            Some(self.page_certificate(revision.certificate, continuation)),
        )
    }

    /// Indexes or refreshes one project path.
    ///
    /// # Errors
    /// Returns an error when the coordinate or daemon reply fails admission.
    pub fn index(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        self.index_with_execution_intent(coordinate, CompileExecutionIntent::Interactive)
    }

    /// Indexes or refreshes one project path with an explicit compile execution intent.
    ///
    /// Interactive is latency-protecting; Background permits bounded remote calibration.
    ///
    /// # Errors
    /// Returns an error when the coordinate or daemon reply fails admission.
    pub fn index_with_execution_intent(
        &mut self,
        coordinate: &str,
        execution_intent: CompileExecutionIntent,
    ) -> Result<ReplyDto, ClientError> {
        let package = package_key(coordinate);
        self.send_success(
            Command::Add {
                package,
                execution_intent,
            },
            Some(key_certificate(
                WireSchema::Package,
                package.as_bytes(),
                coordinate,
            )),
        )
    }

    /// Removes one indexed project path and its selected files.
    ///
    /// # Errors
    /// Returns an error when the coordinate or daemon reply fails admission.
    pub fn remove(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let package = package_key(coordinate);
        self.send_success(
            Command::Remove { package },
            Some(key_certificate(
                WireSchema::Package,
                package.as_bytes(),
                coordinate,
            )),
        )
    }

    /// Searches names and documents at the current immutable revision.
    ///
    /// # Errors
    /// Returns an error when the limit, revision, request, or reply fails admission.
    pub fn search(&mut self, text: &str, limit: u16) -> Result<ReplyDto, ClientError> {
        self.with_default_search_wait(|this| {
            let deadline = query_preparation::QueryReadDeadline::new(
                CLIENT_REQUEST_TIMEOUT,
                this.search_wait_scope
                    .as_ref()
                    .or(this.request_context.as_ref()),
            )?;
            let revision = this.revision_with_budget(deadline.remaining()?)?;
            let limit = QueryLimit::new(limit).ok_or_else(|| {
                ClientError::Protocol("query limit is outside its bound".to_owned())
            })?;
            this.send_success_with_budget(
                Command::Search(Query::new(text, revision.root, limit)),
                Some(revision.certificate),
                Some(deadline.remaining()?),
            )
        })
    }

    /// Reads or resumes one bounded search page at the current revision.
    ///
    /// The continuation is accepted only when the daemon's producer
    /// certificate and this session's remembered page claim match it. A raw
    /// caller cursor therefore cannot fabricate a result or restart a query
    /// against a different view root.
    pub fn search_page(
        &mut self,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.with_default_search_wait(|this| {
            this.search_page_with_budget(text, limit, continuation, CLIENT_REQUEST_TIMEOUT)
        })
    }

    /// Reads a complete native catalog window under unchanged issued credit.
    /// Continued adaptive reads require an admitted exact next query command.
    ///
    /// # Errors
    /// Refuses unprepared, stale or unrelated cursors, cancellation, and native reply failures.
    pub fn search_page_window(
        &mut self,
        text: &str,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.with_default_search_wait(|this| {
            let deadline = query_preparation::QueryReadDeadline::new(
                CLIENT_REQUEST_TIMEOUT,
                this.search_wait_scope
                    .as_ref()
                    .or(this.request_context.as_ref()),
            )?;
            this.prepare_query_window(window, continuation, Some(deadline.remaining()?))?;
            if this.prepared_query.is_some() {
                return this.resume_prepared_query_window(
                    "Search",
                    text,
                    window.issued().get(),
                    continuation,
                    Some(deadline.remaining()?),
                    Some(window),
                );
            }
            let revision = this.revision_with_budget(deadline.remaining()?)?;
            let query = Query::new(text, revision.root, window.issued())
                .with_window(window)
                .ok_or_else(|| ClientError::Protocol("search window changed credit".to_owned()))?;
            let query = if let Some(cursor) = continuation {
                query.with_cursor(cursor.cursor())
            } else {
                query
            };
            this.send_success_with_budget(
                Command::Search(query),
                Some(this.page_certificate(revision.certificate, continuation)),
                Some(deadline.remaining()?),
            )
        })
    }

    fn with_default_search_wait<T>(
        &mut self,
        call: impl FnOnce(&mut Self) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        let scope = self.request_context.clone().unwrap_or_default();
        scope.begin_default_search(CLIENT_REQUEST_TIMEOUT)?;
        let previous = self.search_wait_scope.replace(scope);
        let result = call(self);
        self.search_wait_scope = previous;
        result
    }

    fn search_page_with_budget(
        &mut self,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
        budget: Duration,
    ) -> Result<ReplyDto, ClientError> {
        let deadline = query_preparation::QueryReadDeadline::new(
            budget,
            self.search_wait_scope
                .as_ref()
                .or(self.request_context.as_ref()),
        )?;
        // A cursor issued by the useful name stage cannot be sent as a full
        // Search command, even after richer search becomes ready. Reopen the
        // retained exact next command with the existing owner/proof admission.
        if self.prepared_query.is_none()
            && continuation.is_some_and(|continuation| {
                self.continuations
                    .get(&continuation.cursor())
                    .and_then(|retained| retained.next_request.as_ref())
                    .is_some_and(|request| {
                        matches!(&request.command, Command::SearchPublishedNames(_))
                    })
            })
        {
            let limit = QueryLimit::new(limit).ok_or_else(|| {
                ClientError::Protocol("query limit is outside its bound".to_owned())
            })?;
            self.prepare_query_window(
                backend_library::QueryWindow::fixed(limit),
                continuation,
                Some(deadline.remaining()?),
            )?;
        }
        if self.prepared_query.is_some() {
            return self.resume_prepared_query_with_budget(
                "Search",
                text,
                limit,
                continuation,
                Some(deadline.remaining()?),
            );
        }
        let revision = self.revision_with_budget(deadline.remaining()?)?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        let query = Query::new(text, revision.root, limit);
        let query = match continuation {
            Some(cursor) => query.with_cursor(cursor.cursor()),
            None => query,
        };
        self.send_success_with_budget(
            Command::Search(query),
            Some(self.page_certificate(revision.certificate, continuation)),
            Some(deadline.remaining()?),
        )
    }

    /// Searches declaration names at the current immutable revision.
    ///
    /// # Errors
    /// Returns an error when the limit, revision, request, or reply fails admission.
    pub fn names(&mut self, text: &str, limit: u16) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        self.send_success(
            Command::Name(NameQuery::new(text, revision.root, limit)),
            Some(revision.certificate),
        )
    }

    /// Reads or resumes one bounded name-resolution page at the current revision.
    pub fn names_page(
        &mut self,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if self.prepared_query.is_some() {
            return self.resume_prepared_query("Name", text, limit, continuation);
        }
        let revision = self.revision()?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        let query = NameQuery::new(text, revision.root, limit);
        let query = match continuation {
            Some(cursor) => query.with_cursor(cursor.cursor()),
            None => query,
        };
        self.send_success(
            Command::Name(query),
            Some(self.page_certificate(revision.certificate, continuation)),
        )
    }

    /// Reads a complete name window, independently bounding this delivery.
    /// Continued adaptive reads require an admitted exact next query command.
    ///
    /// # Errors
    /// Refuses unprepared, stale or unrelated cursors, cancellation, and native reply failures.
    pub fn names_page_window(
        &mut self,
        text: &str,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.prepare_query_window(window, continuation, None)?;
        if self.prepared_query.is_some() {
            return self.resume_prepared_query_window(
                "Name",
                text,
                window.issued().get(),
                continuation,
                None,
                Some(window),
            );
        }
        let revision = self.revision()?;
        let query = NameQuery::new(text, revision.root, window.issued())
            .with_window(window)
            .ok_or_else(|| ClientError::Protocol("name window changed credit".to_owned()))?;
        let query = if let Some(cursor) = continuation {
            query.with_cursor(cursor.cursor())
        } else {
            query
        };
        self.send_success(
            Command::Name(query),
            Some(self.page_certificate(revision.certificate, continuation)),
        )
    }

    /// Reads one declaration document by its canonical coordinate.
    ///
    /// # Errors
    /// Returns an error when the revision, coordinate, request, or reply fails admission.
    pub fn document(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        self.send_success(
            Command::Document(DocumentQuery::new(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads a document for a producer-admitted symbol selected from a prior
    /// result row. The opaque wire locator is resolved again against the exact
    /// current view before the owner executes the query.
    ///
    /// # Errors
    /// Returns an error when the revision changed, the symbol is absent, or
    /// request/reply admission fails.
    pub fn document_symbol(&mut self, symbol: SymbolKey) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let certificate = selected_symbol_certificate(revision.certificate, symbol);
        self.send_success(
            Command::Document(DocumentQuery::selected(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads a public declaration operand without promoting selected bytes to a key.
    /// A supplied coordinate is checked by the owner against the selected row label.
    ///
    /// # Errors
    /// Refuses absent/stale row membership, mismatched labels, and admission failures.
    pub fn declaration(
        &mut self,
        selector: &DeclarationSelector,
        read: DeclarationRead,
    ) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let certificate = declaration_certificate(revision.certificate, selector);
        let address = selector.address();
        let command = match read {
            DeclarationRead::Document => {
                Command::Document(DocumentQuery::addressed(address, revision.root))
            }
            DeclarationRead::Source => {
                Command::Source(DocumentQuery::addressed(address, revision.root))
            }
            DeclarationRead::Graph => {
                Command::Graph(GraphNeighborhoodQuery::addressed(address, revision.root))
            }
            DeclarationRead::Related => {
                Command::Related(GraphNeighborhoodQuery::addressed(address, revision.root))
            }
        };
        self.send_success(command, Some(certificate))
    }

    /// Reads only the admitted document owner's immediate members at its exact
    /// immutable source. A revision change refuses this composite read rather
    /// than re-resolving its coordinate or substituting a new owner.
    ///
    /// # Errors
    /// Returns owner, transport, source-basis or continuation admission failures.
    pub fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        if revision.root != document.basis {
            return Err(ClientError::CommandFailed(
                backend_library::CommandFailure::WrongBasis {
                    expected: document.basis.into(),
                    observed: revision.root.into(),
                },
            ));
        }
        let source = self.admitted_source_basis()?;
        if document.source_basis() != Some(source)
            || !document.selection.as_ref().is_some_and(|selection| {
                selection.symbol() == document.symbol && selection.source() == source
            })
        {
            return Err(ClientError::StaleCursor);
        }
        // A bounded owner revision supplies a root commitment, not the full
        // relation preimage needed to serialize a source Basis. Check that
        // admitted source locally, then retain it for reply admission below.
        let owner = DocumentQuery::selected(document.symbol, document.basis);
        let certificate = selected_symbol_certificate(revision.certificate, document.symbol);
        let page = page_request(document.basis, limit, continuation)?;
        self.send_success(
            Command::MembersPage { owner, page },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Reads a graph window while retaining the exact selected ID and optional label.
    ///
    /// # Errors
    /// Refuses changed continuation operands or any ordinary owner/window admission failure.
    pub fn graph_page_declaration(
        &mut self,
        selector: &DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.prepare_query_window(window, continuation, None)?;
        if let Some(prepared) = &self.prepared_query {
            if !declaration_coordinate_claims_match(prepared.certificate(), selector) {
                return Err(ClientError::Protocol(
                    "graph continuation changed declaration coordinate assertion".to_owned(),
                ));
            }
            return self.resume_prepared_graph_window(
                selector.address(),
                window.issued().get(),
                continuation,
                Some(window),
            );
        }
        let revision = self.revision()?;
        let certificate = declaration_certificate(revision.certificate, selector);
        let page = page_request(revision.root, window.issued().get(), continuation)?
            .with_window(window)
            .ok_or_else(|| ClientError::Protocol("graph window changed credit".to_owned()))?;
        self.send_success(
            Command::GraphPage {
                symbol: selector.address(),
                page,
            },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Reads captured source for one declaration coordinate.
    ///
    /// # Errors
    /// Returns an error when the revision, coordinate, transport, or reply fails admission.
    pub fn source(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        self.send_success(
            Command::Source(DocumentQuery::new(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads one project outline by its canonical coordinate.
    ///
    /// # Errors
    /// Returns an error when the revision, coordinate, request, or reply fails admission.
    pub fn outline(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let package = package_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package.as_bytes()),
            value: coordinate.to_owned(),
        });
        self.send_success(
            Command::Outline(OutlineQuery::new(package, revision.root)),
            Some(certificate),
        )
    }

    /// Reads one bounded flat outline page at the current immutable revision.
    ///
    /// # Errors
    /// Returns an error when revision, coordinate, limit, transport, or reply admission fails.
    pub fn outline_page(
        &mut self,
        coordinate: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let credit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("invalid outline credit".to_owned()))?;
        self.outline_page_with_window(coordinate, backend_library::QueryWindow::fixed(credit), continuation)
    }

    /// Reads a complete native outline window under unchanged issued credit.
    ///
    /// # Errors
    /// Refuses unrelated/unprepared cursors, stale authority and reply failures.
    pub fn outline_page_window(
        &mut self,
        coordinate: &str,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.prepare_query_window(window, continuation, None)?;
        self.outline_page_with_window(coordinate, window, continuation)
    }

    /// Resumes an outline directly from a compact owner-rehydrated PC5 token.
    /// The token's page-root digest remains untrusted until the owner rebuilds
    /// its exact predecessor rows before normal dispatch.
    ///
    /// # Errors
    /// Refuses malformed, stale, foreign-scope, wrong-package, or non-outline
    /// tokens and propagates the ordinary request/reply admission failures.
    pub fn outline_page_from_query_token(
        &mut self,
        coordinate: &str,
        window: backend_library::QueryWindow,
        token: &str,
    ) -> Result<ReplyDto, ClientError> {
        let (command, certificate) =
            self.outline_witness_request(coordinate, window, token)?;
        self.send_success(command, Some(certificate))
    }

    fn outline_page_with_window(
        &mut self,
        coordinate: &str,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if self.prepared_query.is_some() {
            return self.resume_prepared_outline(package_key(coordinate), window, continuation);
        }
        let revision = self.revision()?;
        let package = package_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package.as_bytes()),
            value: coordinate.to_owned(),
        });
        let page = page_request(revision.root, window.issued().get(), continuation)?
            .with_window(window).ok_or_else(|| ClientError::Protocol("outline delivery changed issued credit".to_owned()))?;
        self.send_success(
            Command::OutlinePage { package, page },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Reads one declaration's bounded graph neighborhood.
    ///
    /// # Errors
    /// Returns an error when the revision, coordinate, request, or reply fails admission.
    pub fn graph(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        self.send_success(
            Command::Graph(GraphNeighborhoodQuery::new(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads incoming and outgoing relations for one declaration coordinate.
    ///
    /// # Errors
    /// Returns an error when the revision, coordinate, transport, or reply fails admission.
    pub fn related(&mut self, coordinate: &str) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        self.send_success(
            Command::Related(GraphNeighborhoodQuery::new(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads a graph neighborhood for a producer-admitted selected symbol.
    ///
    /// # Errors
    /// Returns an error when the revision changed, the symbol is absent, or
    /// request/reply admission fails.
    pub fn graph_symbol(&mut self, symbol: SymbolKey) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let certificate = selected_symbol_certificate(revision.certificate, symbol);
        self.send_success(
            Command::Graph(GraphNeighborhoodQuery::selected(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads incoming and outgoing relations for a producer-admitted symbol.
    ///
    /// # Errors
    /// Returns an error when the revision changed, the symbol is absent, or
    /// request/reply admission fails.
    pub fn related_symbol(&mut self, symbol: SymbolKey) -> Result<ReplyDto, ClientError> {
        let revision = self.revision()?;
        let certificate = selected_symbol_certificate(revision.certificate, symbol);
        self.send_success(
            Command::Related(GraphNeighborhoodQuery::selected(symbol, revision.root)),
            Some(certificate),
        )
    }

    /// Reads one bounded graph-neighborhood page at the current immutable revision.
    ///
    /// # Errors
    /// Returns an error when revision, coordinate, limit, transport, or reply admission fails.
    pub fn graph_page(
        &mut self,
        coordinate: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if self.prepared_query.is_some() {
            return self.resume_prepared_graph(
                SymbolAddress::canonical(symbol_key(coordinate)),
                limit,
                continuation,
            );
        }
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        let page = page_request(revision.root, limit, continuation)?;
        self.send_success(
            Command::GraphPage {
                symbol: SymbolAddress::canonical(symbol),
                page,
            },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Reads a plain graph window from the exact admitted input frontier.
    /// Continued adaptive reads require an admitted exact next query command.
    ///
    /// # Errors
    /// Refuses unprepared, stale or unrelated cursors, cancellation, and native reply failures.
    pub fn graph_page_window(
        &mut self,
        coordinate: &str,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.prepare_query_window(window, continuation, None)?;
        if self.prepared_query.is_some() {
            return self.resume_prepared_graph_window(
                SymbolAddress::canonical(symbol_key(coordinate)),
                window.issued().get(),
                continuation,
                Some(window),
            );
        }
        let revision = self.revision()?;
        let symbol = symbol_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.as_bytes()),
            value: coordinate.to_owned(),
        });
        let page = page_request(revision.root, window.issued().get(), continuation)?
            .with_window(window)
            .ok_or_else(|| ClientError::Protocol("graph window changed credit".to_owned()))?;
        self.send_success(
            Command::GraphPage {
                symbol: SymbolAddress::canonical(symbol),
                page,
            },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Reads a bounded graph page for a producer-admitted selected symbol.
    ///
    /// # Errors
    /// Returns an error when the revision, selected symbol, limit, transport,
    /// continuation, or reply fails admission.
    pub fn graph_page_symbol(
        &mut self,
        symbol: SymbolKey,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if self.prepared_query.is_some() {
            return self.resume_prepared_graph(
                SymbolAddress::selected(symbol),
                limit,
                continuation,
            );
        }
        let revision = self.revision()?;
        let certificate = selected_symbol_certificate(revision.certificate, symbol);
        let page = page_request(revision.root, limit, continuation)?;
        self.send_success(
            Command::GraphPage {
                symbol: SymbolAddress::selected(symbol),
                page,
            },
            Some(self.page_certificate(certificate, continuation)),
        )
    }

    /// Executes or resumes one bounded structured graph query remotely.
    ///
    /// # Errors
    ///
    /// Returns an error when query admission, revision, transport, proof, or
    /// reply-shape validation fails.
    pub fn graph_query(
        &mut self,
        query: String,
        variables: BTreeMap<String, GraphValue>,
        limit: u16,
        continuation: Option<PageContinuation>,
        cancel: bool,
    ) -> Result<GraphQueryPage, ClientError> {
        let input = AdmittedGraphQueryInput::new(query, variables)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        self.graph_query_admitted(input, limit, continuation, cancel)
    }

    /// Executes or resumes a graph query from an already admitted input.
    ///
    /// The input is expected to have been admitted before this method is
    /// called. In particular, this lets a caller validate once before looking
    /// up a revision and then bind the same canonical value to that revision.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision, page limit, transport, proof, or
    /// reply-shape validation fails.
    pub fn graph_query_admitted(
        &mut self,
        input: AdmittedGraphQueryInput,
        limit: u16,
        continuation: Option<PageContinuation>,
        cancel: bool,
    ) -> Result<GraphQueryPage, ClientError> {
        let deadline = query_preparation::QueryReadDeadline::new(
            CLIENT_REQUEST_TIMEOUT,
            self.request_context.as_ref(),
        )?;
        if let Some(prepared) = self.prepared_query.take() {
            let Command::GraphQuery(query) = &prepared.command else {
                return Err(ClientError::Protocol(
                    "continuation changed query family".to_owned(),
                ));
            };
            if query.input() != &input
                || query.page().limit().get() != limit
                || query.page().continuation() != continuation
            {
                return Err(ClientError::Protocol(
                    "continuation changed graph query arguments".to_owned(),
                ));
            }
        }
        let revision = self.revision_with_budget(deadline.remaining()?)?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        let mut request = GraphQueryRequest::bind(input, revision.root, limit);
        if let Some(continuation) = continuation {
            request = request.with_continuation(continuation);
        }
        if cancel {
            request = request.cancelled();
        }
        request
            .start_offset(revision.cursor())
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let issuing_owner = revision.cursor();
        let page_contract = request.clone();
        // The owner cursor remains the authority claim. The query's distinct
        // projection recipe is canonical from this admitted input, view and
        // limit, including when a fresh session resumes an opaque token.
        let certificate = revision.certificate.with_claim_once(WireClaim::KeyBytes {
            schema: WireSchema::ViewRecipe,
            id: encode_id(request.recipe().as_bytes()),
            value: request.recipe_preimage(),
        });
        let reply = self.send_success_with_budget(
            Command::GraphQuery(request),
            Some(certificate),
            Some(deadline.remaining()?),
        )?;
        let CommandReply::GraphQueryPage(page) = reply.reply else {
            return Err(ClientError::Protocol(
                "graph query reply changed shape".to_owned(),
            ));
        };
        page_contract
            .admit_page_against(&page, issuing_owner)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(page)
    }

    /// Executes one typed daemon-owned product command.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or reply-shape admission fails.
    pub fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        if let SurfaceCommand::SemanticShapes { request } = command {
            let keys = request
                .symbols()
                .iter()
                .copied()
                .map(SymbolAddress::Selected)
                .collect::<Vec<_>>();
            let budget = request
                .budget()
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            let source = match request.source() {
                backend_library::SemanticShapeSource::Inline(source) => source.clone(),
                backend_library::SemanticShapeSource::Reference(reference) => {
                    self.semantic_source(reference.clone())?
                }
            };
            let (_, admitted_request, reply) = self.read_semantic_shapes(
                source,
                request.source_epoch(),
                keys.into_boxed_slice(),
                budget,
            )?;
            let export = backend_library::SemanticShapeExport::from_admitted_reply(
                &reply,
                &admitted_request,
            )
            .map_err(ClientError::Protocol)?;
            return Ok(SurfaceReply::SemanticShapes(export));
        }
        let source_read = match &command {
            SurfaceCommand::SemanticSource { request } => {
                Some((request.reference.source.clone(), request.range.clone()))
            }
            _ => None,
        };
        let version_read = match &command {
            SurfaceCommand::SemanticVersions {
                package,
                limit,
                cursor,
            } => Some((package.clone(), *limit, cursor.clone())),
            _ => None,
        };
        let expected = command.id();
        let reply = if matches!(&command, SurfaceCommand::IndexSearch { .. }) {
            self.send_index_search_ready(command)?
        } else {
            self.send_success(Command::Surface(command), None)?
        };
        let CommandReply::Surface(reply) = reply.reply else {
            return Err(ClientError::Protocol(
                "surface reply changed shape".to_owned(),
            ));
        };
        if let Some((source, range)) = source_read {
            let SurfaceReply::SemanticSource(chunk) = &reply else {
                return Err(ClientError::Protocol(
                    "semantic source reply changed shape".to_owned(),
                ));
            };
            chunk
                .admit_range(&source, &range)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
        } else {
            reply
                .admit(expected)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
        }
        if let Some((package, limit, cursor)) = version_read {
            let SurfaceReply::SemanticVersionsPage(page) = &reply else {
                return Err(ClientError::Protocol(
                    "paged semantic-versions command requires a native descriptor page".to_owned(),
                ));
            };
            page.admit_request(&package, limit, cursor.as_ref())
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
        }
        Ok(reply)
    }

    /// Starts one package index job and returns its exact owner-issued ticket or terminal receipt.
    ///
    /// Keep a separate [`Session`] for [`Self::await_index_job`] and
    /// [`Self::cancel_index_job`] when the UI needs to await and cancel concurrently.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the typed start reply fails.
    pub fn start_index_job(
        &mut self,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<IndexStartResult, ClientError> {
        match self.surface(SurfaceCommand::IndexStart {
            package,
            execution_intent,
        })? {
            SurfaceReply::IndexStarted(result) => Ok(result),
            _ => Err(ClientError::Protocol(
                "index start reply changed shape".to_owned(),
            )),
        }
    }

    /// Starts or replays one index operation under a caller-persisted key.
    ///
    /// Persist `operation_key` before calling this method. If the transport is
    /// interrupted, query [`Self::index_operation_status`] with the same key;
    /// this method never invents a replacement key or retries another
    /// operation on the caller's behalf.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the typed reply
    /// shape fails.
    pub fn start_index_operation(
        &mut self,
        operation_key: IndexOperationKey,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<IndexOperationObservation, ClientError> {
        match self.surface(SurfaceCommand::IndexOperationStart {
            operation_key,
            package,
            execution_intent,
        })? {
            SurfaceReply::IndexOperationStarted(observation) => Ok(observation),
            _ => Err(ClientError::Protocol(
                "durable index start reply changed shape".to_owned(),
            )),
        }
    }

    /// Reads durable status for one exact caller-owned index operation key.
    ///
    /// `Unknown` means that no retained operation was found; it never means
    /// that indexing succeeded. Existing keys are not implicitly restarted.
    /// If the transport disconnects, a caller may explicitly reconnect this
    /// same session and repeat only this exact keyed read. This method does not
    /// reconnect or retry on its own.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the typed reply
    /// shape fails.
    pub fn index_operation_status(
        &mut self,
        operation_key: IndexOperationKey,
    ) -> Result<IndexOperationObservation, ClientError> {
        match self.surface(SurfaceCommand::IndexOperationStatus { operation_key })? {
            SurfaceReply::IndexOperationStatus(observation) => Ok(observation),
            _ => Err(ClientError::Protocol(
                "durable index status reply changed shape".to_owned(),
            )),
        }
    }

    /// Waits for the terminal receipt of one exact owner-issued index ticket.
    ///
    /// # Errors
    /// Returns an error when the ticket is unknown, request admission fails, or the owner reply
    /// violates its typed contract.
    pub fn await_index_job(
        &mut self,
        ticket: IndexJobTicket,
    ) -> Result<IndexJobTerminal, ClientError> {
        match self.surface(SurfaceCommand::IndexAwait { ticket })? {
            SurfaceReply::IndexTerminal(terminal) => Ok(terminal),
            _ => Err(ClientError::Protocol(
                "index await reply changed shape".to_owned(),
            )),
        }
    }

    /// Reads the immediate observation and next bounded progress page for one exact index job.
    ///
    /// Pass the returned `next_sequence` on the next call. A `truncated` page means older
    /// events aged out of the owner's bounded buffer before they were read. Terminal and
    /// unknown-ticket states are returned directly and never inferred from an empty page.
    ///
    /// # Errors
    /// Returns an error when request admission fails or the owner reply violates its typed
    /// contract.
    pub fn index_job_progress(
        &mut self,
        ticket: IndexJobTicket,
        after_sequence: u64,
    ) -> Result<IndexJobObservation, ClientError> {
        match self.surface(SurfaceCommand::IndexProgress {
            ticket,
            after_sequence,
        })? {
            SurfaceReply::IndexProgress(page) => Ok(page),
            _ => Err(ClientError::Protocol(
                "index progress reply changed shape".to_owned(),
            )),
        }
    }

    /// Requests cancellation of one exact owner-issued index ticket.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the typed cancellation reply fails.
    pub fn cancel_index_job(
        &mut self,
        ticket: IndexJobTicket,
    ) -> Result<IndexCancelReceipt, ClientError> {
        match self.surface(SurfaceCommand::IndexCancel { ticket })? {
            SurfaceReply::IndexCancellation(receipt) => Ok(receipt),
            _ => Err(ClientError::Protocol(
                "index cancellation reply changed shape".to_owned(),
            )),
        }
    }

    /// Compares two indexed package versions through their complete semantic
    /// compiler publications.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the typed diff
    /// reply contract fails.
    pub fn diff(
        &mut self,
        from: PackageReference,
        to: PackageReference,
    ) -> Result<Box<[DiffRecord]>, ClientError> {
        match self.surface(SurfaceCommand::Diff { from, to })? {
            SurfaceReply::Diff(rows) => Ok(rows),
            _ => Err(ClientError::Protocol(
                "semantic diff reply changed shape".to_owned(),
            )),
        }
    }

    /// Lists immutable compiler generations retained for one exact package.
    ///
    /// # Errors
    /// Returns an error when package syntax, transport, or semantic history
    /// reply admission fails.
    pub fn semantic_versions(
        &mut self,
        package: PackageReference,
    ) -> Result<Box<[SemanticVersionRecord]>, ClientError> {
        let mut cursor = None;
        let mut records = Vec::new();
        loop {
            let reply = self.surface(SurfaceCommand::SemanticVersions {
                package: package.clone(),
                limit: 1,
                cursor,
            })?;
            let SurfaceReply::SemanticVersionsPage(page) = reply else {
                return Err(ClientError::Protocol(
                    "semantic versions reply changed shape".to_owned(),
                ));
            };
            for entry in &page.entries {
                let backend_library::SemanticVersionDescriptor::Available { reference } = entry
                else {
                    return Err(ClientError::Protocol(
                        "exact semantic source retention is unavailable".to_owned(),
                    ));
                };
                records.push(self.semantic_source(reference.clone())?);
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        Ok(records.into_boxed_slice())
    }

    /// Reads a bounded native version descriptor page with exact root bindings.
    ///
    /// # Errors
    /// Refuses stale/foreign continuations and malformed native page replies.
    pub fn semantic_versions_page(
        &mut self,
        package: PackageReference,
        limit: u16,
        cursor: Option<backend_library::SemanticVersionsCursor>,
    ) -> Result<backend_library::SemanticVersionsPage, ClientError> {
        match self.surface(SurfaceCommand::SemanticVersions {
            package,
            limit,
            cursor,
        })? {
            SurfaceReply::SemanticVersionsPage(page) => Ok(page),
            _ => Err(ClientError::Protocol(
                "semantic versions page changed shape".to_owned(),
            )),
        }
    }

    /// Reconstructs one complete captured record through exact native ranges.
    ///
    /// # Errors
    /// Refuses gaps, reordered or foreign chunks, changed extents and digest
    /// mismatch; each source read retains the same portable owner reference.
    pub fn semantic_source(
        &mut self,
        reference: backend_library::SemanticSourceReference,
    ) -> Result<SemanticVersionRecord, ClientError> {
        let mut assembly = backend_library::SemanticSourceAssembly::new(reference.source.clone())
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        loop {
            let range = backend_library::SemanticSourceRange {
                offset: assembly
                    .offset()
                    .map_err(|error| ClientError::Protocol(error.to_string()))?,
                ..Default::default()
            };
            let admitted = self.semantic_source_range(&reference, range)?;
            let complete = admitted.complete();
            assembly
                .append_admitted(admitted)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            if complete {
                break;
            }
        }
        assembly
            .finish()
            .map_err(|error| ClientError::Protocol(error.to_string()))
    }

    fn semantic_source_range(
        &mut self,
        reference: &backend_library::SemanticSourceReference,
        range: backend_library::SemanticSourceRange,
    ) -> Result<backend_library::AdmittedSemanticSourceChunk, ClientError> {
        let command = SurfaceCommand::SemanticSource {
            request: backend_library::SemanticSourceRequest {
                reference: reference.clone(),
                range: range.clone(),
            },
        };
        command
            .admit()
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let reply = self.send_success(Command::Surface(command), None)?;
        let CommandReply::Surface(SurfaceReply::SemanticSource(chunk)) = reply.reply else {
            return Err(ClientError::Protocol(
                "semantic source range reply changed shape".to_owned(),
            ));
        };
        let admitted = chunk
            .admit_decoded()
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        admitted
            .correlate(&reference.source, &range)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(admitted)
    }

    /// Reads one bounded page of exact source File rows selected by a local Project.
    ///
    /// Continuations are stateless and bound to the same package, selected
    /// source relation root, source version, and canonical last file key.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or the page reply
    /// contract fails.
    pub fn package_source_membership_page(
        &mut self,
        request: PackageSourceMembershipPageRequestV1,
    ) -> Result<PackageSourceMembershipPageResultV1, ClientError> {
        match self.surface(SurfaceCommand::PackageSourceMembership { request })? {
            SurfaceReply::PackageSourceMembershipPage(page) => Ok(page),
            _ => Err(ClientError::Protocol(
                "package source membership reply changed shape".to_owned(),
            )),
        }
    }

    /// Selects one exact retained compiler generation for product projection.
    ///
    /// # Errors
    /// Returns an error when the target profile, generation, transport, or
    /// owner reply fails admission.
    pub fn select_semantic_version(
        &mut self,
        package: PackageReference,
        coordinate: backend_library::PackageCoordinate,
        profile: SemanticLanguageProfile,
        generation: SemanticGenerationId,
    ) -> Result<SemanticVersionRecord, ClientError> {
        match self.surface(SurfaceCommand::SelectSemanticVersion {
            package,
            coordinate,
            profile,
            generation,
        })? {
            SurfaceReply::SemanticVersionSelected(record) => Ok(record),
            _ => Err(ClientError::Protocol(
                "semantic version selection reply changed shape".to_owned(),
            )),
        }
    }

    /// Reads compiler-owned declaration shapes from one exact selected semantic
    /// generation, with the request and reply pinned to the current view root.
    /// The caller's budget can only tighten the shared product limits. Symbols
    /// must be keys already admitted from this product view; the owner resolves
    /// each one again before opening the selected compiler image.
    ///
    /// # Errors
    /// Returns an error when the selected generation, view, symbols, response
    /// budget, transport, or proof fails admission.
    pub fn semantic_shapes(
        &mut self,
        source: SemanticVersionRecord,
        source_epoch: backend_library::SemanticSourceEpoch,
        symbols: &[SymbolKey],
        budget: SemanticShapeBudget,
    ) -> Result<SemanticShapeBatch, ClientError> {
        self.read_semantic_shapes(
            source,
            source_epoch,
            symbols
                .iter()
                .copied()
                .map(SymbolAddress::selected)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            budget,
        )
        .map(|(batch, _, _)| batch)
    }

    fn read_semantic_shapes(
        &mut self,
        source: SemanticVersionRecord,
        source_epoch: backend_library::SemanticSourceEpoch,
        symbols: Box<[SymbolAddress]>,
        budget: SemanticShapeBudget,
    ) -> Result<(SemanticShapeBatch, SemanticShapeRequest, ReplyDto), ClientError> {
        if symbols.is_empty() || symbols.len() > backend_library::MAX_SEMANTIC_SHAPE_BATCH {
            return Err(ClientError::Protocol(
                backend_library::SemanticShapeError::BatchBound.to_string(),
            ));
        }
        let revision = self.revision()?;
        let certificate = symbols
            .iter()
            .fold(revision.certificate, |certificate, symbol| {
                certificate.with_claim_once(WireClaim::KeyCommitment {
                    schema: WireSchema::Symbol,
                    id: encode_id(&symbol.claimed_bytes()),
                })
            });
        let request =
            SemanticShapeRequest::new(revision.root, source, source_epoch, symbols, budget)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let reply =
            self.send_success(Command::SemanticShapes(request.clone()), Some(certificate))?;
        let CommandReply::SemanticShapes(batch) = &reply.reply else {
            return Err(ClientError::Protocol(
                "semantic-shape reply changed shape".to_owned(),
            ));
        };
        batch
            .admit_against(&request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok((batch.clone(), request, reply))
    }

    fn send(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
    ) -> Result<ReplyDto, ClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("request identity exhausted".to_owned()))?;
        let request = match certificate {
            Some(certificate) => CommandDto::new(request_id, command).with_certificate(certificate),
            None => CommandDto::new(request_id, command),
        };
        self.check_request_context()?;
        let read_budget = self
            .request_context
            .as_ref()
            .map(RequestContext::remaining_read_budget)
            .transpose()?
            .flatten();
        let result = match read_budget {
            Some(remaining) => self.transport.request_with_budget(request, remaining),
            None => self.transport.request(request),
        };
        self.request_result(result)
    }

    fn send_success(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
    ) -> Result<ReplyDto, ClientError> {
        self.send_success_with_budget(command, certificate, None)
    }

    fn send_with_budget(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
        remaining: Duration,
    ) -> Result<ReplyDto, ClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("request identity exhausted".to_owned()))?;
        let mut request = CommandDto::new(request_id, command);
        if let Some(certificate) = certificate {
            request = request.with_certificate(certificate);
        }
        self.check_request_context()?;
        let read_budget = self
            .request_context
            .as_ref()
            .map(RequestContext::remaining_read_budget)
            .transpose()?
            .flatten();
        let remaining = read_budget.map_or(remaining, |budget| remaining.min(budget));
        let result = self.transport.request_with_budget(request, remaining);
        self.request_result(result)
    }

    fn admitted_source_basis(&self) -> Result<backend_library::Basis, ClientError> {
        let owner = self.admitted_owner.ok_or(ClientError::StaleCursor)?;
        Ok(backend_library::Basis {
            root: owner.root(),
            object: self.admitted_source.ok_or(ClientError::StaleCursor)?,
            branch: owner.branch(),
            log: owner.log(),
            schema: owner.schema(),
        })
    }

    fn send_success_with_budget(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
        remaining: Option<Duration>,
    ) -> Result<ReplyDto, ClientError> {
        let started = std::time::Instant::now();
        let members_source = matches!(&command, Command::MembersPage { .. })
            .then(|| self.admitted_source_basis())
            .transpose()?;
        let selected_assertion = selected_coordinate_assertion(&command, certificate.as_ref())?;
        // A small graph page may legitimately omit its center. Authenticate
        // that selected row through a document at this already admitted source,
        // never through a refreshed revision or an echoed coordinate claim.
        let pinned_center = if let (Some(assertion), Command::GraphPage { page, .. }) =
            (&selected_assertion, &command)
        {
            let owner = self.admitted_owner.ok_or(ClientError::StaleCursor)?;
            let source = backend_library::Basis {
                root: owner.root(),
                object: self.admitted_source.ok_or(ClientError::StaleCursor)?,
                branch: owner.branch(),
                log: owner.log(),
                schema: owner.schema(),
            };
            if !page.basis().matches(source.root) {
                return Err(ClientError::StaleCursor);
            }
            let retained = page
                .continuation()
                .and_then(|cursor| self.continuations.get(&cursor.cursor()))
                .and_then(|retained| retained.selected_coordinate.as_ref())
                .filter(|center| center.matches(assertion, source))
                .cloned();
            Some(if let Some(center) = retained {
                center
            } else {
                let document = self.send_success_with_budget(
                    // The owner revision is a bounded commitment, not the
                    // complete relation preimage required to encode a Basis.
                    // Pin the ordinary document request to that revision and
                    // compare its admitted reply to the full source below.
                    Command::Document(DocumentQuery::addressed(assertion.symbol, source.root)),
                    certificate.clone(),
                    remaining.map(|budget| budget.saturating_sub(started.elapsed())),
                )?;
                assertion.admit_document(&document, source)?
            })
        } else {
            None
        };
        let portable = matches!(
            &command,
            Command::Search(_)
                | Command::SearchPublishedNames(_)
                | Command::Name(_)
                | Command::GraphQuery(_)
                | Command::GraphPage { .. }
                | Command::OutlinePage { .. }
        )
        .then(|| (command.clone(), certificate.clone()));
        let remaining = remaining.map(|budget| budget.saturating_sub(started.elapsed()));
        let reply = require_command_success(match remaining {
            Some(remaining) => {
                self.send_query_ready_with_budget(command, certificate, remaining)?
            }
            None => self.send_query_ready(command, certificate)?,
        })?;
        if let Some(assertion) = selected_assertion {
            assertion.admit_payload(&reply, pinned_center.as_ref())?;
        }
        if let Some(source) = members_source {
            admit_member_page_source(&reply, source)?;
        }
        self.remember_continuation(&reply, pinned_center);
        if let Some((command, Some(certificate))) = portable {
            self.retain_portable_query(command, &certificate, &reply)?;
        }
        Ok(reply)
    }

    fn page_certificate(
        &self,
        mut certificate: WireCertificate,
        continuation: Option<PageContinuation>,
    ) -> WireCertificate {
        let Some(continuation) = continuation else {
            return certificate;
        };
        if let Some(previous) = self.continuations.get(&continuation.cursor()) {
            for claim in previous
                .certificate
                .claims
                .iter()
                .filter(|claim| claim_describes_cursor(claim, continuation.cursor()))
            {
                certificate = certificate.with_claim_once(claim.clone());
            }
        }
        with_page_continuation_claim(certificate, Some(continuation))
    }

    fn remember_continuation(
        &mut self,
        reply: &ReplyDto,
        selected_coordinate: Option<PinnedSelectedCoordinate>,
    ) {
        let terminal = match &reply.reply {
            CommandReply::ProjectionPage(page) => page.terminal,
            CommandReply::PublishedNameSearch(page) => {
                page.snapshot.next.map_or(PageTerminal::Complete, |cursor| {
                    PageTerminal::More(PageContinuation::from_cursor(cursor))
                })
            }
            CommandReply::GraphQueryPage(page) => page.terminal,
            CommandReply::Names(snapshot) | CommandReply::Search(snapshot) => {
                snapshot.next.map_or(PageTerminal::Complete, |cursor| {
                    PageTerminal::More(PageContinuation::from_cursor(cursor))
                })
            }
            _ => return,
        };
        let PageTerminal::More(continuation) = terminal else {
            return;
        };
        let Some(certificate) = reply.certificate().cloned() else {
            return;
        };
        let Some(owner) = self.admitted_owner else {
            return;
        };
        self.continuations.clear();
        self.continuations.insert(
            continuation.cursor(),
            RetainedContinuation {
                certificate,
                owner,
                next_request: None,
                selected_coordinate,
            },
        );
    }
}

fn claim_describes_cursor(claim: &WireClaim, cursor: backend_library::Cursor) -> bool {
    let recipe = encode_id(cursor.recipe().as_bytes());
    let version = encode_id(cursor.version().as_bytes());
    let branch = encode_id(cursor.branch().as_bytes());
    let log = encode_id(cursor.log().as_bytes());
    let root = encode_id(cursor.root().as_bytes());
    match claim {
        WireClaim::Key { schema, id, .. } | WireClaim::KeyBytes { schema, id, .. } => {
            (*schema == WireSchema::ViewRecipe && id == &recipe)
                || (*schema == WireSchema::Branch && id == &branch)
                || (*schema == WireSchema::Log && id == &log)
        }
        WireClaim::Version { schema, id, .. } => {
            *schema == WireSchema::ViewVersion && id == &version
        }
        WireClaim::Root { schema, id, .. } => *schema == WireSchema::ViewRelation && id == &root,
        WireClaim::KeyCommitment { .. }
        | WireClaim::RowIdentity { .. }
        | WireClaim::RootCommitment { .. }
        | WireClaim::Intent { .. }
        | WireClaim::Delta { .. }
        | WireClaim::Cursor { .. }
        | WireClaim::Coverage { .. } => false,
    }
}

fn page_request(
    basis: ViewStateRoot,
    limit: u16,
    continuation: Option<PageContinuation>,
) -> Result<PageRequest, ClientError> {
    let limit = QueryLimit::new(limit)
        .ok_or_else(|| ClientError::Protocol("page limit is outside its bound".to_owned()))?;
    let page = PageRequest::new(basis, limit);
    Ok(continuation.map_or(page, |continuation| page.with_continuation(continuation)))
}

fn with_page_continuation_claim(
    certificate: WireCertificate,
    continuation: Option<PageContinuation>,
) -> WireCertificate {
    let Some(continuation) = continuation else {
        return certificate;
    };
    let cursor = continuation.cursor();
    certificate.with_claim_once(WireClaim::Cursor {
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        branch: encode_id(cursor.branch().as_bytes()),
        log: encode_id(cursor.log().as_bytes()),
        schema: cursor.schema(),
        root: encode_id(cursor.root().as_bytes()),
        sequence: cursor.sequence(),
    })
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut chars = value.bytes();
    while let (Some(high), Some(low)) = (chars.next(), chars.next()) {
        let high = (high as char).to_digit(16)? as u8;
        let low = (low as char).to_digit(16)? as u8;
        bytes.push(high << 4 | low);
    }
    Some(bytes)
}

fn admit_member_page_source(
    reply: &ReplyDto,
    source: backend_library::Basis,
) -> Result<(), ClientError> {
    let CommandReply::ProjectionPage(page) = &reply.reply else {
        return Err(ClientError::Protocol(
            "member page changed reply shape".to_owned(),
        ));
    };
    if page.snapshot.root.basis() != source {
        return Err(ClientError::StaleCursor);
    }
    Ok(())
}

fn health_from_reply(reply: ReplyDto) -> Result<HealthReport, ClientError> {
    let legacy_cursor = reply.health_cursor();
    match reply.reply {
        CommandReply::Readiness(report) => Ok(report),
        CommandReply::Health(root) => {
            let cursor = legacy_cursor.ok_or_else(|| {
                ClientError::Protocol("legacy health reply omitted its cursor".to_owned())
            })?;
            Ok(HealthReport::from_root(&root, cursor))
        }
        _ => Err(ClientError::Protocol(
            "health reply changed shape".to_owned(),
        )),
    }
}

fn require_command_success(reply: ReplyDto) -> Result<ReplyDto, ClientError> {
    match &reply.reply {
        CommandReply::Failed(CommandFailure::ReplyTooLarge { basis }) => {
            Err(ClientError::ResponseTooLarge {
                basis: Some(*basis),
            })
        }
        CommandReply::Failed(failure) => Err(ClientError::CommandFailed(failure.clone())),
        // `Error` remains decodable for peers using the pre-typed reply schema.
        CommandReply::Error(message) => Err(ClientError::Protocol(message.clone())),
        _ => Ok(reply),
    }
}

fn key_certificate(schema: WireSchema, id: &[u8; 32], value: &str) -> WireCertificate {
    WireCertificate::new().with_claim(WireClaim::Key {
        schema,
        id: encode_id(id),
        value: value.to_owned(),
    })
}

// A peer on the same DTO version may decode the label claim without enforcing
// it. Only the admitted selected payload can discharge that assertion.
#[cfg(any(unix, windows))]
struct SelectedCoordinateAssertion {
    symbol: SymbolAddress,
    coordinate: String,
}

#[cfg(any(unix, windows))]
#[derive(Clone)]
struct PinnedSelectedCoordinate {
    symbol: SymbolKey,
    source: backend_library::Basis,
    coordinate: String,
}

#[cfg(any(unix, windows))]
impl PinnedSelectedCoordinate {
    fn matches(
        &self,
        assertion: &SelectedCoordinateAssertion,
        source: backend_library::Basis,
    ) -> bool {
        assertion.symbol.matches(self.symbol)
            && assertion.coordinate == self.coordinate
            && self.source == source
    }
}

#[cfg(any(unix, windows))]
impl SelectedCoordinateAssertion {
    fn admit_document(
        &self,
        reply: &ReplyDto,
        source: backend_library::Basis,
    ) -> Result<PinnedSelectedCoordinate, ClientError> {
        self.admit_payload(reply, None)?;
        let (CommandReply::Document(document) | CommandReply::Page(document)) = &reply.reply else {
            return Err(ClientError::Protocol(
                "selected document changed shape".to_owned(),
            ));
        };
        if document.source_basis() != Some(source) || document.basis != source.root {
            return Err(ClientError::StaleCursor);
        }
        Ok(PinnedSelectedCoordinate {
            symbol: document.symbol,
            source,
            coordinate: self.coordinate.clone(),
        })
    }

    fn admit_payload(
        &self,
        reply: &ReplyDto,
        pinned_center: Option<&PinnedSelectedCoordinate>,
    ) -> Result<(), ClientError> {
        let coordinate = match &reply.reply {
            CommandReply::Document(document) | CommandReply::Page(document) => document
                .selection
                .as_ref()
                .filter(|selection| {
                    self.symbol.matches(document.symbol)
                        && selection.symbol() == document.symbol
                        && Some(selection.source()) == document.source_basis()
                })
                .map(backend_library::DocumentSelection::coordinate),
            CommandReply::Graph(snapshot) => self.center_coordinate(&snapshot.root),
            CommandReply::ProjectionPage(page) => {
                self.center_coordinate(&page.snapshot.root).or_else(|| {
                    pinned_center
                        .filter(|center| {
                            self.symbol.matches(center.symbol)
                                && center.source == page.snapshot.root.basis()
                        })
                        .map(|center| center.coordinate.as_str())
                })
            }
            _ => None,
        };
        if coordinate != Some(self.coordinate.as_str()) {
            return Err(ClientError::Protocol(
                "selected reply omitted or mismatched its asserted coordinate".to_owned(),
            ));
        }
        Ok(())
    }

    fn center_coordinate<'a>(&self, root: &'a backend_library::ViewRoot) -> Option<&'a str> {
        root.row_refs().find_map(|row| match row.id {
            backend_library::RowId::Symbol(symbol) if self.symbol.matches(symbol) => {
                Some(row.label.as_str())
            }
            _ => None,
        })
    }
}

#[cfg(any(unix, windows))]
fn selected_coordinate_assertion(
    command: &Command,
    certificate: Option<&WireCertificate>,
) -> Result<Option<SelectedCoordinateAssertion>, ClientError> {
    let symbol = match command {
        Command::Document(query) | Command::Source(query) => query.symbol(),
        Command::Graph(query) | Command::Related(query) => query.symbol(),
        Command::GraphPage { symbol, .. } => *symbol,
        _ => return Ok(None),
    };
    if !symbol.is_selected() {
        return Ok(None);
    }
    let mut coordinates = certificate
        .into_iter()
        .flat_map(|certificate| &certificate.claims)
        .filter_map(|claim| match claim {
            WireClaim::Key {
                schema: WireSchema::Symbol,
                value,
                ..
            } => Some(value),
            _ => None,
        });
    let Some(coordinate) = coordinates.next() else {
        return Ok(None);
    };
    if coordinates.next().is_some() {
        return Err(ClientError::Protocol(
            "selected declaration has multiple coordinate assertions".to_owned(),
        ));
    }
    Ok(Some(SelectedCoordinateAssertion {
        symbol,
        coordinate: coordinate.clone(),
    }))
}
#[cfg(any(unix, windows))]
fn declaration_coordinate_claims_match(
    certificate: Option<&WireCertificate>,
    selector: &DeclarationSelector,
) -> bool {
    let labels: Vec<_> = certificate
        .into_iter()
        .flat_map(|certificate| certificate.claims.iter())
        .filter_map(|claim| match claim {
            WireClaim::Key {
                schema: WireSchema::Symbol,
                value,
                ..
            } => Some(value.as_str()),
            _ => None,
        })
        .collect();
    labels.as_slice() == selector.coordinate().as_slice()
}

#[cfg(any(unix, windows))]
fn declaration_certificate(
    mut certificate: WireCertificate,
    selector: &DeclarationSelector,
) -> WireCertificate {
    if let Some(symbol) = selector.selected_id() {
        certificate = certificate.with_claim_once(WireClaim::KeyCommitment {
            schema: WireSchema::Symbol,
            id: encode_id(symbol.bytes()),
        });
    }
    if let Some(coordinate) = selector.coordinate() {
        certificate = certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Symbol,
            id: encode_id(symbol_key(coordinate).as_bytes()),
            value: coordinate.to_owned(),
        });
    }
    certificate
}

#[cfg(any(unix, windows))]
fn selected_symbol_certificate(certificate: WireCertificate, symbol: SymbolKey) -> WireCertificate {
    certificate.with_claim_once(WireClaim::KeyCommitment {
        schema: WireSchema::Symbol,
        id: encode_id(symbol.as_bytes()),
    })
}

#[cfg(any(unix, windows))]
fn configure(stream: &backend_replication::LocalStream) -> Result<(), ClientError> {
    configure_timeout(stream, CLIENT_REQUEST_TIMEOUT)
}

#[cfg(all(test, any(unix, windows)))]
fn configure_request(
    stream: &backend_replication::LocalStream,
    request: &CommandDto,
) -> Result<(), ClientError> {
    configure_request_with_timeouts(
        stream,
        request,
        CLIENT_REQUEST_TIMEOUT,
        CLIENT_MUTATION_TIMEOUT,
    )
}

#[cfg(any(unix, windows))]
fn configure_request_with_timeouts(
    stream: &backend_replication::LocalStream,
    request: &CommandDto,
    read_timeout: Duration,
    mutation_timeout: Duration,
) -> Result<(), ClientError> {
    // These mutations acknowledge accepted work; they do not await its
    // publication. An interrupted acknowledgement leaves the caller's exact
    // durable operation claim available for reconciliation.
    let timeout = match &request.command {
        Command::Surface(
            SurfaceCommand::IndexStart { .. }
            | SurfaceCommand::IndexOperationStart { .. }
            | SurfaceCommand::IndexCancel { .. },
        ) => read_timeout,
        _ => match backend_library::command_spec(request.command.id()).mutation {
            CommandMutation::Write => mutation_timeout,
            CommandMutation::Read => read_timeout,
        },
    };
    configure_timeout(stream, timeout)
}

#[cfg(any(unix, windows))]
fn configure_timeout(
    stream: &backend_replication::LocalStream,
    timeout: Duration,
) -> Result<(), ClientError> {
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| {
            if is_disconnect(error.kind()) {
                ClientError::Disconnected(error.kind())
            } else {
                ClientError::Io(error.to_string())
            }
        })
}

/// Lowers a failed endpoint dial into the same connection-lost class as a
/// reset on an already-open stream. This matters during a daemon restart:
/// the endpoint can exist while no listener is bound for a short interval,
/// and the reconnect policy must be allowed to retry that bounded race.
/// A refused dial or a missing socket is `Disconnected` for exactly that
/// reason; see [`ClientError::is_owner_absent`].
#[cfg(any(unix, windows))]
fn map_endpoint_connect_error(error: std::io::Error) -> ClientError {
    if is_disconnect(error.kind()) || is_owner_absent_kind(error.kind()) {
        ClientError::Disconnected(error.kind())
    } else {
        ClientError::Io(error.to_string())
    }
}

/// Authentication happens after a successful dial, so a daemon that exits in
/// the small interval between those operations can look like a security
/// failure even though the only thing that changed was the peer's lifetime.
/// Preserve real owner/permission failures as ordinary I/O errors, while
/// classifying endpoint disappearance and peer teardown as a reconnectable
/// disconnect.
#[cfg(any(unix, windows))]
fn map_peer_authentication_error(
    error: backend_replication::LocalPeerAuthenticationError,
) -> ClientError {
    use backend_replication::LocalPeerAuthenticationError as AuthenticationError;
    use backend_replication::PeerCredentialError;

    let disconnected = |kind| ClientError::Disconnected(kind);
    match error {
        AuthenticationError::EndpointIo(kind)
            if is_disconnect(kind) || is_owner_absent_kind(kind) =>
        {
            disconnected(kind)
        }
        AuthenticationError::PeerAddress => disconnected(std::io::ErrorKind::ConnectionAborted),
        AuthenticationError::PeerCredentials(PeerCredentialError::Io(kind))
            if is_disconnect(kind) || is_owner_absent_kind(kind) =>
        {
            disconnected(kind)
        }
        other => ClientError::Io(format!("local peer authentication failed: {other}")),
    }
}

/// Bounded lease for one read-only command, including health and discovery.
///
/// Every request re-arms this value, so a long mutation cannot make a later
/// health or query call wait on the mutation lease. Direct remote-index setup,
/// handshake, send, and response read share this same absolute deadline.
pub const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Request frames one local connection carries before the owner retires it.
///
/// The owner closes the connection after this many frames, whatever the next
/// frame is, so a client that counts frames can open a fresh connection before
/// writing a command whose admission it could not afterwards establish.
pub const DEFAULT_CONNECTION_FRAME_BUDGET: usize = 256;

/// Hidden test hook that lowers [`DEFAULT_CONNECTION_FRAME_BUDGET`] so
/// connection retirement can be exercised deterministically on real binaries.
/// The owner reads it when it builds its listener, and a client reads it to
/// count against the same budget; both see it when they share an environment.
pub const CONNECTION_FRAME_BUDGET_ENV: &str = "BACKEND_LOCALD_MAX_FRAMES_PER_CONNECTION";

/// Parses the [`CONNECTION_FRAME_BUDGET_ENV`] value. Only a nonzero integer
/// overrides the default; zero and malformed text leave it in force.
#[must_use]
pub fn parse_connection_frame_budget(value: Option<&str>) -> Option<usize> {
    value?
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|budget| *budget != 0)
}

/// Returns the per-connection frame budget this process counts against.
#[must_use]
pub fn connection_frame_budget() -> usize {
    static BUDGET: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BUDGET.get_or_init(|| {
        parse_connection_frame_budget(std::env::var(CONNECTION_FRAME_BUDGET_ENV).ok().as_deref())
            .unwrap_or(DEFAULT_CONNECTION_FRAME_BUDGET)
    })
}

/// Owner lease for durable writes that may synchronously compile or acquire a
/// package before publishing their receipt.
const CLIENT_MUTATION_TIMEOUT: Duration = Duration::from_mins(15);

fn limits() -> LocalControlLimits {
    LocalControlLimits {
        max_frame: MAX_FRAME,
        ..LocalControlLimits::default()
    }
}

fn map_frame(error: LocalControlError) -> ClientError {
    match error {
        LocalControlError::FrameTooLarge => {
            ClientError::Transport(ReplicationError::MessageTooLarge)
        }
        LocalControlError::Io(kind) if is_disconnect(kind) => ClientError::Disconnected(kind),
        LocalControlError::Io(kind) => {
            ClientError::Io(format!("local control I/O failed: {kind:?}"))
        }
        LocalControlError::Closed => ClientError::Disconnected(std::io::ErrorKind::NotConnected),
        // The framing layer reports `Truncated` only for an unexpected
        // end of file, which is a peer that stopped mid-frame rather than a
        // frame this client failed to understand.
        LocalControlError::Truncated => {
            ClientError::Disconnected(std::io::ErrorKind::UnexpectedEof)
        }
        other => ClientError::Protocol(other.to_string()),
    }
}

/// Returns whether one stream error kind means this connection is gone.
///
/// `WouldBlock` and `TimedOut` are both here because a socket read timeout
/// surfaces as either depending on the platform, and the local listener closes
/// a connection whose read timeout expires without writing anything back.
/// macOS also reports `InvalidInput` when a socket option is applied to a
/// Unix stream whose peer has already closed. That is a dead connection at
/// this boundary; treating it as a plain endpoint I/O fault prevents the MCP
/// reconnect wrapper from replacing the stale stream.
const fn is_disconnect(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::NotConnected
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::InvalidInput
    )
}

/// Dial outcomes that mean no listener is bound: a refused connection (the
/// socket file outlives a killed daemon) or a missing socket file.
///
/// Kept apart from [`is_disconnect`] on purpose. `is_disconnect` classifies
/// I/O on a stream that was open, where a refusal cannot occur and where a
/// `Disconnected` command is reported as `MutationOutcomeUnknown`; a refused
/// dial happens before any byte is sent.
pub(crate) const fn is_owner_absent_kind(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    )
}

/// Read-timeout outcomes: a socket read timeout surfaces as either kind
/// depending on the platform. The owner is alive and did not answer in time.
pub(crate) const fn is_owner_slow_kind(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

/// Every kind that is a peer teardown rather than a slow peer: stream loss
/// plus an absent owner. `TimedOut` and `WouldBlock` are excluded because a
/// listener that is merely slow or busy is still the owner.
pub(crate) const fn is_connection_lost_kind(kind: std::io::ErrorKind) -> bool {
    is_owner_absent_kind(kind)
        || matches!(
            kind,
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::InvalidInput
        )
}

fn encode_request(request: &CommandDto) -> Result<Vec<u8>, ClientError> {
    admit_request(request)?;
    backend_library::encode_command_body(request).map_err(ClientError::Protocol)
}

fn write_body(writer: &mut impl Write, body: &[u8]) -> Result<(), ClientError> {
    write_frame(writer, body, limits()).map_err(map_frame)
}

fn read_body(reader: &mut impl Read) -> Result<Vec<u8>, ClientError> {
    read_frame(reader, limits()).map_err(|error| match error {
        LocalControlError::FrameTooLarge => ClientError::ResponseFrameTooLarge { basis: None },
        other => map_frame(other),
    })
}

/// Admits one bounded command.
///
/// # Errors
/// Returns an error when request text or encoded memory exceeds its contract.
pub fn admit_request(request: &CommandDto) -> Result<(), ClientError> {
    backend_library::admit_request(request).map_err(|error| match error {
        RequestAdmissionError::EmptyText | RequestAdmissionError::InvalidSurface => {
            ClientError::Protocol(error.to_string())
        }
        RequestAdmissionError::TextTooLarge => {
            ClientError::Transport(ReplicationError::MessageTooLarge)
        }
    })
}

/// Admits one reply without an external coverage capability.
///
/// # Errors
/// Returns an error when correlation, proof, freshness, or memory admission fails.
pub fn admit_reply(request: &CommandDto, reply: ReplyDto) -> Result<ReplyDto, ClientError> {
    admit_reply_with_capability(request, reply, None)
}

/// Admits reply identity, freshness, correlation, and memory bounds.
///
/// # Errors
/// Returns an error when correlation, proof, coverage, freshness, or memory admission fails.
pub fn admit_reply_with_capability(
    request: &CommandDto,
    reply: ReplyDto,
    capability: Option<CoverageCapability>,
) -> Result<ReplyDto, ClientError> {
    backend_library::admit_reply_with_capability(request, &reply, capability).map_err(map_reply)?;
    if backend_library::reply_memory_bound(&reply) > MAX_FRAME {
        return Err(ClientError::ResponseTooLarge {
            basis: query_continuation::basis(&request.command),
        });
    }
    let encoded =
        serde_json::to_vec(&reply).map_err(|error| ClientError::Protocol(error.to_string()))?;
    if encoded.len() > MAX_FRAME {
        return Err(ClientError::ResponseTooLarge {
            basis: query_continuation::basis(&request.command),
        });
    }
    Ok(reply)
}

fn map_reply(error: ReplyAdmissionError) -> ClientError {
    match error {
        ReplyAdmissionError::RequestMismatch { expected, observed } => {
            ClientError::RequestMismatch { expected, observed }
        }
        ReplyAdmissionError::Protocol(message) => ClientError::Protocol(message),
        ReplyAdmissionError::Projection(error) => match error {
            ViewProjectionError::Incoherent => ClientError::IncoherentView,
            ViewProjectionError::UnsupportedSchema => {
                ClientError::Protocol("unsupported view schema".to_owned())
            }
            ViewProjectionError::CursorMismatch => ClientError::CursorMismatch,
            ViewProjectionError::FreshnessMismatch => ClientError::FreshnessMismatch,
            ViewProjectionError::BasisMismatch { expected, observed } => {
                ClientError::BasisMismatch { expected, observed }
            }
            ViewProjectionError::MissingCoverage => ClientError::Protocol(
                "view projection requires producer-admitted complete coverage".to_owned(),
            ),
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// A killed daemon leaves its socket file behind, so "no owner" is a
    /// refused dial, not a missing path. This reproduces that exact state with
    /// a bound-then-dropped listener (std never unlinks on drop).
    #[cfg(unix)]
    #[test]
    fn a_dead_listeners_refused_dial_is_an_absent_owner_and_a_lost_connection() {
        let path = std::path::PathBuf::from(format!(
            "/tmp/backend-client-refused-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind then drop"));
        assert!(path.exists(), "the dead listener's socket file remains");

        let refused = Session::connect(&path)
            .err()
            .expect("a socket file with no listener refuses the dial");
        assert_eq!(
            refused,
            ClientError::Disconnected(std::io::ErrorKind::ConnectionRefused),
            "a refused dial is retryable owner absence, not a plain I/O fault"
        );
        assert!(refused.is_owner_absent());
        assert!(refused.is_connection_lost());

        let with_context = Session::connect_with_context(&path, RequestContext::new())
            .err()
            .expect("the scoped dial reports the same absence");
        assert!(with_context.is_owner_absent());

        std::fs::remove_file(&path).expect("remove dead socket file");
        let missing = Session::connect(&path)
            .err()
            .expect("no socket file at all");
        assert!(missing.is_owner_absent(), "{missing}");
    }

    #[test]
    fn connection_loss_is_one_closed_classification() {
        use std::io::ErrorKind;
        for kind in [ErrorKind::ConnectionRefused, ErrorKind::NotFound] {
            let error = ClientError::Disconnected(kind);
            assert!(error.is_owner_absent(), "{kind:?}");
            assert!(error.is_connection_lost(), "{kind:?} is also lost");
        }
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::UnexpectedEof,
            ErrorKind::InvalidInput,
        ] {
            let error = ClientError::Disconnected(kind);
            assert!(error.is_connection_lost(), "{kind:?}");
            assert!(
                !error.is_owner_absent(),
                "{kind:?} is a live-peer teardown; it never proves the owner is gone"
            );
        }
        // Slow or busy listeners are still the owner and never authorize a
        // replacement; neither does any non-peer failure.
        for kind in [ErrorKind::TimedOut, ErrorKind::WouldBlock] {
            let error = ClientError::Disconnected(kind);
            assert!(!error.is_connection_lost(), "{kind:?}");
            assert!(!error.is_owner_absent(), "{kind:?}");
            assert!(
                error.is_owner_slow(),
                "{kind:?} is a live owner that did not answer"
            );
        }
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
            ErrorKind::Other,
        ] {
            let error = ClientError::Disconnected(kind);
            assert!(!error.is_connection_lost(), "{kind:?}");
            assert!(!error.is_owner_absent(), "{kind:?}");
            assert!(!error.is_owner_slow(), "{kind:?}");
        }
        for error in [
            ClientError::Io("Connection refused (os error 111)".to_owned()),
            ClientError::Protocol("proof".to_owned()),
            ClientError::RemoteDeadlineExceeded,
            ClientError::MutationOutcomeUnknown(ErrorKind::ConnectionReset),
            ClientError::QueryPreparationDisconnected {
                basis: backend_library::view_state_root(&[]).into(),
                kind: ErrorKind::ConnectionReset,
            },
            ClientError::RequestCancelled(RequestCancellationReason::Explicit),
        ] {
            assert!(!error.is_connection_lost(), "{error}");
            assert!(!error.is_owner_absent(), "{error}");
            assert!(!error.is_owner_slow(), "{error}");
            assert!(!error.is_owner_gap(), "{error}");
        }
        // An owner gap is what an idempotent read may outwait: lost, slow or
        // starting; never a refusal of the request itself.
        for kind in [
            ErrorKind::ConnectionReset,
            ErrorKind::BrokenPipe,
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionRefused,
            ErrorKind::TimedOut,
            ErrorKind::WouldBlock,
        ] {
            assert!(ClientError::Disconnected(kind).is_owner_gap(), "{kind:?}");
        }
        assert!(!ClientError::Disconnected(ErrorKind::PermissionDenied).is_owner_gap());
    }

    /// A refusal only exists before a byte is sent. Frame-level I/O keeps its
    /// own set, so a command in flight still reports an unknown outcome.
    #[test]
    fn a_refusal_is_not_a_frame_level_disconnect() {
        assert!(!is_disconnect(std::io::ErrorKind::ConnectionRefused));
        assert!(!is_disconnect(std::io::ErrorKind::NotFound));
        assert_eq!(
            map_frame(LocalControlError::Io(std::io::ErrorKind::ConnectionRefused)),
            ClientError::Io("local control I/O failed: ConnectionRefused".to_owned())
        );
        assert_eq!(
            map_endpoint_connect_error(std::io::ErrorKind::ConnectionRefused.into()),
            ClientError::Disconnected(std::io::ErrorKind::ConnectionRefused)
        );
    }

    #[test]
    fn runtime_startup_conversion_preserves_typed_cause_and_original_diagnostic() {
        use backend_runtime::RuntimeError;
        use std::io::{Error, ErrorKind};
        let cases = [
            (
                RuntimeError::Io(Error::from(ErrorKind::NotFound)),
                StartupFailureCause::Io(ErrorKind::NotFound),
                StartupRecovery::RepairConfiguration,
            ),
            (
                RuntimeError::EndpointUnavailable {
                    endpoint: "/socket".into(),
                    source: Error::from(ErrorKind::NotFound),
                },
                StartupFailureCause::EndpointUnavailable(ErrorKind::NotFound),
                StartupRecovery::RetryOriginalCommand,
            ),
            (
                RuntimeError::MissingExecutable("/debug/deps/backend-locald".into()),
                StartupFailureCause::MissingCompanion,
                StartupRecovery::RepairCompanion,
            ),
            (
                RuntimeError::Io(Error::new(
                    ErrorKind::PermissionDenied,
                    "private state parent /state refused",
                )),
                StartupFailureCause::Io(ErrorKind::PermissionDenied),
                StartupRecovery::RepairConfiguration,
            ),
            (
                RuntimeError::Io(Error::from(ErrorKind::ConnectionRefused)),
                StartupFailureCause::Io(ErrorKind::ConnectionRefused),
                StartupRecovery::RetryOriginalCommand,
            ),
        ];
        for (error, cause, recovery) in cases {
            let detail = error.to_string();
            let client = ClientError::from(error);
            let ClientError::Startup(observation) = &client else {
                panic!("runtime startup was flattened: {client}");
            };
            assert_eq!(observation.cause(), cause);
            assert_eq!(observation.recovery(), recovery);
            assert_eq!(observation.detail(), detail);
            assert_eq!(client.to_string(), format!("local startup: {detail}"));
        }
    }

    #[test]
    fn typed_command_failure_survives_the_client_boundary() {
        let reply = ReplyDto::new(
            7,
            CommandReply::Failed(CommandFailure::MutationRequiresOwner),
        );

        assert_eq!(
            require_command_success(reply),
            Err(ClientError::CommandFailed(
                CommandFailure::MutationRequiresOwner
            ))
        );
    }

    #[test]
    fn legacy_error_reply_remains_compatible() {
        let reply = ReplyDto::new(7, CommandReply::Error("legacy failure".to_owned()));

        assert_eq!(
            require_command_success(reply),
            Err(ClientError::Protocol("legacy failure".to_owned()))
        );
    }

    #[test]
    fn bounded_readiness_is_the_primary_health_reply() {
        let library = backend_library::Library::new();
        let reply = library.execute_dto(CommandDto::new(9, Command::Health));

        let report = health_from_reply(reply).expect("typed readiness");
        assert_eq!(report.row_count(), 0);
        assert_eq!(report.revision().root(), library.revision_root());
    }

    #[test]
    fn legacy_full_health_view_lowers_to_a_bounded_report() {
        let library = backend_library::Library::new();
        let reply = ReplyDto::health(9, library.view().clone(), library.cursor());

        let report = health_from_reply(reply).expect("legacy health");
        assert_eq!(report.row_count(), 0);
        assert_eq!(report.revision().root(), library.revision_root());
    }

    #[test]
    fn page_requests_preserve_opaque_continuations() {
        let basis = backend_library::view_state_root(&[]);
        let continuation = PageContinuation::from_cursor(backend_library::Cursor::new());
        let page = page_request(basis, 7, Some(continuation)).expect("page request");

        assert_eq!(page.limit().get(), 7);
        assert_eq!(page.continuation(), Some(continuation));
        assert!(page_request(basis, 0, None).is_err());
    }

    #[test]
    fn follow_up_page_request_carries_its_opaque_cursor_claim() {
        let basis = backend_library::view_state_root(&[]);
        let continuation = PageContinuation::from_cursor(backend_library::Cursor::new());
        let page = page_request(basis, 3, Some(continuation)).expect("page request");
        let certificate = with_page_continuation_claim(
            WireCertificate::new().with_claim(WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id: encode_id(basis.as_bytes()),
            }),
            Some(continuation),
        );
        let request = CommandDto::new(11, Command::PackagePage(page)).with_certificate(certificate);
        let encoded = serde_json::to_value(&request).expect("encode page request");
        assert!(
            encoded["certificate"]["claims"]
                .as_array()
                .expect("certificate claims")
                .iter()
                .any(|claim| claim["kind"] == "cursor")
        );
    }

    #[cfg(unix)]
    #[test]
    fn each_request_rearms_a_bounded_deadline_for_its_own_lease() {
        let path = std::path::PathBuf::from(format!(
            "/tmp/backend-client-timeout-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("timeout listener");
        let connector_path = path.clone();
        let connector = std::thread::spawn(move || {
            std::os::unix::net::UnixStream::connect(connector_path).expect("timeout client")
        });
        let (_peer, _) = listener.accept().expect("timeout peer");
        let client = connector.join().expect("join timeout client");
        let transport = UnixCommandTransport::from_stream(client);
        let health = CommandDto::new(1, Command::Health);
        configure_request(&transport.stream, &health).expect("health timeout");
        assert_eq!(
            transport
                .stream
                .read_timeout()
                .expect("health read timeout"),
            Some(CLIENT_REQUEST_TIMEOUT)
        );

        let add = CommandDto::new(
            2,
            Command::Add {
                package: package_key("/tmp/project"),
                execution_intent: CompileExecutionIntent::Interactive,
            },
        );
        configure_request(&transport.stream, &add).expect("mutation timeout");
        assert_eq!(
            transport
                .stream
                .read_timeout()
                .expect("mutation read timeout"),
            Some(CLIENT_MUTATION_TIMEOUT)
        );

        configure_request(&transport.stream, &health).expect("health rearm");
        assert_eq!(
            transport
                .stream
                .read_timeout()
                .expect("rearmed read timeout"),
            Some(CLIENT_REQUEST_TIMEOUT)
        );

        let accepted = CommandDto::new(
            3,
            Command::Surface(SurfaceCommand::IndexOperationStart {
                operation_key: IndexOperationKey::from_bytes([0x51; 32]).expect("durable key"),
                package: PackageReference::parse("/tmp/project").expect("package"),
                execution_intent: CompileExecutionIntent::Interactive,
            }),
        );
        configure_request(&transport.stream, &accepted).expect("acceptance rearm");
        assert_eq!(
            transport
                .stream
                .read_timeout()
                .expect("acceptance read bound"),
            Some(CLIENT_REQUEST_TIMEOUT)
        );
        assert_eq!(
            transport
                .stream
                .write_timeout()
                .expect("acceptance write bound"),
            Some(CLIENT_REQUEST_TIMEOUT)
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn bounded_local_connect_keeps_authenticated_peer_and_io_deadlines() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::path::PathBuf::from(format!(
            "/tmp/backend-client-connect-timeout-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener =
            std::os::unix::net::UnixListener::bind(&path).expect("authenticated timeout listener");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("make listener owner-private");
        let (accepted, accepted_rx) = std::sync::mpsc::channel();
        let (release_peers, release_peers_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut peers = Vec::new();
            for _ in 0..2 {
                let (peer, _) = listener.accept().expect("accept authenticated client");
                accepted.send(()).expect("signal accepted client");
                peers.push(peer);
            }
            release_peers_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("test releases authenticated peers after checking deadlines");
            drop(peers);
        });

        let io_timeout = Duration::from_millis(125);
        let mut client =
            UnixCommandTransport::connect_with_timeouts(&path, Duration::from_secs(1), io_timeout)
                .expect("connect and authenticate inside dial deadline");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server accepted client");
        assert!(client.peer.is_some(), "owner peer proof must be retained");
        assert_eq!(
            client.stream.read_timeout().expect("read timeout"),
            Some(io_timeout)
        );
        assert_eq!(
            client.stream.write_timeout().expect("write timeout"),
            Some(io_timeout)
        );
        configure_request_with_timeouts(
            &client.stream,
            &CommandDto::new(1, Command::Revision),
            io_timeout,
            CLIENT_MUTATION_TIMEOUT,
        )
        .expect("re-arm the same bounded read deadline for a real request");
        assert_eq!(
            client.stream.read_timeout().expect("re-armed read timeout"),
            Some(io_timeout)
        );

        client.reconnect().expect("reconnect with same deadlines");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("server accepted reconnected client");
        assert!(client.peer.is_some(), "reconnect must re-authenticate peer");
        assert_eq!(
            client
                .stream
                .read_timeout()
                .expect("reconnected read timeout"),
            Some(io_timeout)
        );

        drop(client);
        release_peers
            .send(())
            .expect("release authenticated test peers");
        server.join().expect("join authenticated owner");
        std::fs::remove_file(path).expect("remove test socket");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_closed_unix_peer_is_a_disconnect_before_the_next_request() {
        let path = std::path::PathBuf::from(format!(
            "/tmp/backend-client-stale-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("stale listener");
        let connector_path = path.clone();
        let connector = std::thread::spawn(move || {
            std::os::unix::net::UnixStream::connect(connector_path).expect("stale client")
        });
        let (peer, _) = listener.accept().expect("stale peer");
        let client = connector.join().expect("join stale client");
        drop(peer);

        let request = CommandDto::new(1, Command::Health);
        assert_eq!(
            configure_request(&client, &request),
            Err(ClientError::Disconnected(std::io::ErrorKind::InvalidInput))
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(any(unix, windows))]
mod owner_upgrade;
