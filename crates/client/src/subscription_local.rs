//! Shared local subscription transport for GUI, CLI, MCP, and tests.
//!
//! Requests use locald's bounded `LDC2` control envelope. Successful control
//! replies must carry a typed subscription payload; an acknowledgement without
//! a cursor/read is rejected because it cannot distinguish an empty suffix from
//! a dropped event or reset.

#[cfg(any(unix, windows))]
use crate::lease_contract::{BOOTSTRAP_LEASE, PUBLICATION_CREDIT};
#[cfg(any(unix, windows))]
use crate::reset_hydration::{ResetHydration, ResetHydrationError, ResetPageProgress};
#[cfg(any(unix, windows))]
use crate::subscription::{subscription_read_from_bytes, subscription_read_from_bytes_against};
#[cfg(any(unix, windows))]
use crate::{
    CertifiedSubscriptionTransport, ClientError, MAX_EVENTS, MAX_FRAME, SubscriptionRequest,
    SubscriptionTransport,
};
#[cfg(any(unix, windows))]
use backend_library::{CoverageCapability, Cursor, CursorRead, ViewRoot};
#[cfg(any(unix, windows))]
use backend_replication::{
    LOCAL_CONTROL_MAX_CURSOR, LOCAL_CONTROL_MAX_ERROR, LocalControlClient, LocalControlError,
    LocalControlExchangeDecision, LocalControlExchangeError, LocalControlExchangeProgress,
    LocalControlLimits, LocalControlRequest, LocalControlResponse, LocalSubscriptionId,
    LocalSubscriptionOperation, LocalSubscriptionRequest, LocalSubscriptionResponse,
    ReplicationError,
};
#[cfg(any(unix, windows))]
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(any(unix, windows))]
const CONNECTION_FRAME_BUDGET: usize = 240;

/// Identity for one physical connection, including each replacement after a
/// frame budget or interrupted exchange. A retained clone prevents allocator
/// reuse from making a stale socket appear current.
#[cfg(any(unix, windows))]
#[derive(Clone, Debug)]
pub(crate) struct ConnectionId(Arc<()>);

#[cfg(any(unix, windows))]
impl ConnectionId {
    fn next() -> Self {
        Self(Arc::new(()))
    }
}

#[cfg(any(unix, windows))]
impl PartialEq for ConnectionId {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(any(unix, windows))]
impl Eq for ConnectionId {}

/// A terminal cancel makes its socket permanently unavailable to the
/// transport, even when the owner never acknowledges the cancel.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lifecycle {
    Serving,
    Released,
}

/// Failure starting or completing one resumable local-control request.
#[cfg(any(unix, windows))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalSubscriptionExchangeError {
    /// Connection setup or request admission failed before the exchange began.
    Setup(ClientError),
    /// A complete, correlated response rejected the requested operation.
    Rejected {
        /// Correlation identity of the refused operation.
        request_id: u64,
        /// Producer-provided refusal detail.
        message: String,
    },
    /// The frame was complete but was not a valid response for this operation.
    Invalid(ClientError),
    /// One in-flight request reached a typed terminal condition.
    Exchange(LocalControlExchangeError),
}

#[cfg(any(unix, windows))]
impl std::fmt::Display for LocalSubscriptionExchangeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setup(error) => write!(formatter, "local subscription setup: {error}"),
            Self::Rejected {
                request_id,
                message,
            } => {
                write!(
                    formatter,
                    "local subscription request {request_id} rejected: {message}"
                )
            }
            Self::Invalid(error) => write!(formatter, "local subscription response: {error}"),
            Self::Exchange(error) => error.fmt(formatter),
        }
    }
}

#[cfg(any(unix, windows))]
impl std::error::Error for LocalSubscriptionExchangeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Setup(error) | Self::Invalid(error) => Some(error),
            Self::Rejected { .. } => None,
            Self::Exchange(error) => Some(error),
        }
    }
}

#[cfg(any(unix, windows))]
fn control_limits() -> LocalControlLimits {
    LocalControlLimits {
        max_frame: MAX_FRAME,
        max_cursor: LOCAL_CONTROL_MAX_CURSOR,
        max_error: LOCAL_CONTROL_MAX_ERROR,
    }
}

#[cfg(any(unix, windows))]
fn map_control_error(error: LocalControlError) -> ClientError {
    crate::map_frame(error)
}

#[cfg(any(unix, windows))]
fn subscription_exchange_error(error: LocalSubscriptionExchangeError) -> ClientError {
    match error {
        LocalSubscriptionExchangeError::Setup(error)
        | LocalSubscriptionExchangeError::Invalid(error) => error,
        LocalSubscriptionExchangeError::Rejected { message, .. } => {
            ClientError::Protocol(format!("locald subscription: {message}"))
        }
        LocalSubscriptionExchangeError::Exchange(error) => ClientError::Protocol(error.to_string()),
    }
}

/// A bounded local endpoint subscription adapter.
#[cfg(any(unix, windows))]
pub struct LocalSubscriptionTransport {
    client: LocalControlClient<backend_replication::LocalStream>,
    connection: ConnectionId,
    lifecycle: Lifecycle,
    clock: Arc<dyn crate::monotonic::MonotonicClock>,
    interrupt: Option<crate::TransportInterrupt>,
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
    endpoint: Option<PathBuf>,
    connect_timeout: Duration,
    io_timeout: Duration,
    frames_on_connection: usize,
    next_request_id: u64,
}

#[cfg(any(unix, windows))]
impl LocalSubscriptionTransport {
    /// Connects to a local daemon endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Io`] when the endpoint cannot be connected or
    /// configured, or [`ClientError::Transport`] when its path exceeds the
    /// bounded endpoint limit.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Self::connect_timeout(path, Duration::from_secs(5))
    }

    /// Connects within a caller-specified local dial limit.
    ///
    /// # Errors
    /// Returns an endpoint, authentication, or bounded dial error.
    pub fn connect_timeout(path: impl AsRef<Path>, timeout: Duration) -> Result<Self, ClientError> {
        Self::connect_with_timeouts(path, timeout, Duration::from_secs(30))
    }

    /// Connects with separate dial and socket I/O limits.
    /// # Errors
    /// Returns an authentication, connection, or socket configuration error.
    pub fn connect_with_timeouts(
        path: impl AsRef<Path>,
        timeout: Duration,
        io_timeout: Duration,
    ) -> Result<Self, ClientError> {
        let path = path.as_ref();
        let endpoint = backend_replication::UnixEndpointRef::new(path)
            .map_err(|_| ClientError::Transport(ReplicationError::MessageTooLarge))?;
        let path = endpoint.as_path();
        let stream = connect_stream(path, timeout)?;
        stream
            .set_read_timeout(Some(io_timeout))
            .and_then(|()| stream.set_write_timeout(Some(io_timeout)))
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
            .map_err(crate::map_peer_authentication_error)?;
        let interrupt = Some(crate::TransportInterrupt::new(&stream)?);
        Ok(Self {
            client: LocalControlClient::new(stream, control_limits()),
            connection: ConnectionId::next(),
            lifecycle: Lifecycle::Serving,
            clock: Arc::new(crate::monotonic::SystemClock),
            interrupt,
            peer: Some(peer),
            endpoint: Some(path.to_path_buf()),
            connect_timeout: timeout,
            io_timeout,
            frames_on_connection: 0,
            next_request_id: 1,
        })
    }

    /// Wraps an already connected local stream.
    #[must_use]
    pub fn from_stream(stream: backend_replication::LocalStream) -> Self {
        let timeout = Duration::from_secs(30);
        let _ = stream
            .set_read_timeout(Some(timeout))
            .and_then(|()| stream.set_write_timeout(Some(timeout)));
        let interrupt = crate::TransportInterrupt::new(&stream).ok();
        Self {
            client: LocalControlClient::new(stream, control_limits()),
            connection: ConnectionId::next(),
            lifecycle: Lifecycle::Serving,
            clock: Arc::new(crate::monotonic::SystemClock),
            interrupt,
            peer: None,
            endpoint: None,
            connect_timeout: Duration::from_secs(5),
            io_timeout: timeout,
            frames_on_connection: 0,
            next_request_id: 1,
        }
    }

    /// Replaces the clock used for publication and reset deadlines. Production
    /// transports use the system monotonic clock; deterministic tests may
    /// inject a manual clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn crate::monotonic::MonotonicClock>) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn now(&self) -> Instant {
        self.clock.now()
    }

    pub(crate) fn monotonic_clock(&self) -> Arc<dyn crate::monotonic::MonotonicClock> {
        Arc::clone(&self.clock)
    }

    pub(crate) fn request_deadline(&self) -> Option<Instant> {
        self.now().checked_add(self.io_timeout)
    }

    /// The physical socket currently carrying this transport's requests.
    pub(crate) fn connection(&self) -> ConnectionId {
        self.connection.clone()
    }

    #[cfg(test)]
    pub(crate) fn exhaust_connection_budget_for_test(&mut self) {
        self.frames_on_connection = CONNECTION_FRAME_BUDGET;
    }

    /// Returns a handle for the exact currently connected control socket.
    #[must_use]
    pub fn interrupt_handle(&self) -> Option<crate::TransportInterrupt> {
        self.interrupt.clone()
    }

    fn prepare_request(&mut self) -> Result<(), ClientError> {
        self.prepare_request_with_deadline(None)
    }

    fn prepare_request_until(&mut self, deadline: Instant) -> Result<(), ClientError> {
        self.prepare_request_with_deadline(Some(deadline))
    }

    /// Reconnect work is part of reset setup. Bound the blocking dial by the
    /// clock's remaining allowance when possible, then recheck after each
    /// synchronous setup phase before installing the replacement connection.
    /// The OS dial itself cannot be interrupted by an injected clock jump, so
    /// the configured connect timeout remains a separate upper bound on that
    /// blocking phase.
    fn prepare_request_with_deadline(
        &mut self,
        deadline: Option<Instant>,
    ) -> Result<(), ClientError> {
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        if self.frames_on_connection < CONNECTION_FRAME_BUDGET && !self.client.requires_reconnect()
        {
            return Ok(());
        }
        let endpoint = self.endpoint.as_deref().ok_or_else(|| {
            ClientError::Io("local control connection reached its bounded frame budget".to_owned())
        })?;
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(self.now()));
        if remaining.is_some_and(|remaining| remaining.is_zero()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        let connect_timeout = remaining.map_or(self.connect_timeout, |remaining| {
            self.connect_timeout.min(remaining)
        });
        let stream = connect_stream(endpoint, connect_timeout)?;
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        let remaining_after_dial =
            deadline.map(|deadline| deadline.saturating_duration_since(self.now()));
        if remaining_after_dial.is_some_and(|remaining| remaining.is_zero()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        let io_timeout = remaining_after_dial
            .map_or(self.io_timeout, |remaining| self.io_timeout.min(remaining));
        stream
            .set_read_timeout(Some(io_timeout))
            .and_then(|()| stream.set_write_timeout(Some(io_timeout)))
            .map_err(|error| ClientError::Io(error.to_string()))?;
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .map_err(crate::map_peer_authentication_error)?;
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        let client = LocalControlClient::new(stream, control_limits());
        if let Some(interrupt) = &self.interrupt {
            interrupt.replace(client.stream())?;
        } else {
            self.interrupt = Some(crate::TransportInterrupt::new(client.stream())?);
        }
        if deadline.is_some_and(|deadline| deadline <= self.now()) {
            self.retire_connection();
            return Err(crate::reset_budget::ResetFault::TimeBudget.into());
        }
        self.client = client;
        self.peer = Some(peer);
        self.connection = ConnectionId::next();
        self.frames_on_connection = 0;
        Ok(())
    }

    /// Returns the affine same-user proof bound to this connection.
    #[must_use]
    pub const fn authenticated_peer(&self) -> Option<&backend_replication::AuthenticatedLocalPeer> {
        self.peer.as_ref()
    }

    /// Hydrates the current complete root from bounded producer-certified
    /// pages without first querying or serializing that root in full.
    /// # Errors
    ///
    /// Returns an error when the local peer is unauthenticated or any page,
    /// continuation, descriptor, or final root commitment fails admission.
    pub fn bootstrap_root(&mut self) -> Result<(ViewRoot, Cursor), ClientError> {
        let previous = Cursor::new();
        let mut hydration = ResetHydration::begin(previous, self.now())?;
        let response = self.lease_request_until(
            LocalSubscriptionOperation::Open {
                cursor: Box::new([]),
                credit: PUBLICATION_CREDIT,
                lease_ms: BOOTSTRAP_LEASE.get(),
            },
            hydration.budget().deadline(),
        )?;
        let lease = response.lease();
        let mut cleanup_on = self.connection();
        let root = self.bootstrap_pages(response, lease, &mut hydration, &mut cleanup_on);
        let cleanup =
            self.release_bootstrap_lease_current(lease, cleanup_on, Duration::from_millis(50));
        match root {
            Err(error) => {
                let _ = cleanup;
                Err(error)
            }
            Ok(root) => cleanup.map(|()| root),
        }
    }

    fn bootstrap_pages(
        &mut self,
        mut response: LocalSubscriptionResponse,
        lease: LocalSubscriptionId,
        hydration: &mut ResetHydration,
        cleanup_on: &mut ConnectionId,
    ) -> Result<(ViewRoot, Cursor), ClientError> {
        loop {
            // This operation completed and was correlated before any proof is
            // inspected. It may therefore be the exact cleanup socket even
            // if local admission of the returned page fails.
            *cleanup_on = self.connection();
            if response.lease() != lease {
                return Err(ClientError::Protocol(
                    "bootstrap response changed its lease identity".to_owned(),
                ));
            }
            let LocalSubscriptionResponse::SnapshotPage {
                page,
                next,
                payload,
                ..
            } = response
            else {
                return Err(ClientError::Protocol(
                    "bootstrap lease did not return a snapshot page".to_owned(),
                ));
            };
            let peer = self.peer.as_ref().ok_or_else(|| {
                ClientError::Protocol("bootstrap requires an authenticated local peer".to_owned())
            })?;
            let clock = Arc::clone(&self.clock);
            let progress = hydration
                .admit_page(&page, next.as_deref(), &payload, peer, clock.as_ref())
                .map_err(ResetHydrationError::into_client_error)?;
            match progress {
                ResetPageProgress::Complete { cursor, root, .. } => return Ok((*root, cursor)),
                ResetPageProgress::Continue { continuation, .. } => {
                    hydration.check(clock.as_ref()).map_err(ClientError::from)?;
                    response = self.lease_request_until(
                        LocalSubscriptionOperation::Page {
                            lease,
                            page: continuation,
                            credit: PUBLICATION_CREDIT,
                        },
                        hydration.budget().deadline(),
                    )?;
                }
            }
        }
    }

    fn lease_request_until(
        &mut self,
        operation: LocalSubscriptionOperation,
        deadline: Instant,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        let clock = Arc::clone(&self.clock);
        let mut expired = false;
        match self.lease_request_with_tick(operation, deadline, |_| {
            if clock.now() >= deadline {
                expired = true;
                LocalControlExchangeDecision::Cancel
            } else {
                LocalControlExchangeDecision::Continue
            }
        }) {
            Ok(response) => Ok(response),
            Err(LocalSubscriptionExchangeError::Exchange(exchange))
                if (expired
                    && exchange.failure
                        == backend_replication::LocalControlExchangeFailure::Cancelled)
                    || (clock.now() >= deadline
                        && exchange.failure
                            == backend_replication::LocalControlExchangeFailure::Stalled) =>
            {
                Err(crate::reset_budget::ResetFault::TimeBudget.into())
            }
            Err(error) => Err(subscription_exchange_error(error)),
        }
    }

    /// Releases a one-shot bootstrap lease on its last correlated socket. An
    /// exact acknowledgement restores normal timeouts and leaves the
    /// transport usable; any ambiguous exchange retires the exact socket.
    fn release_bootstrap_lease_current(
        &mut self,
        lease: LocalSubscriptionId,
        cleanup_on: ConnectionId,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        if self.connection != cleanup_on {
            return Err(ClientError::Protocol(
                "bootstrap lease cleanup socket is no longer current".to_owned(),
            ));
        }
        if self.lifecycle == Lifecycle::Released {
            return Err(ClientError::Io(
                "transport already released by a terminal lease cancellation".to_owned(),
            ));
        }
        let request_id = self.next_request_id;
        let Some(next_request_id) = request_id.checked_add(1) else {
            // No cancellation can be correlated without a fresh ID. Retire
            // the exact last-correlated socket and let the finite bootstrap
            // term expire at the owner; never leave this stream reusable.
            self.retire_connection();
            return Err(ClientError::Protocol(
                "subscription request id exhausted".to_owned(),
            ));
        };
        self.next_request_id = next_request_id;
        let request = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id,
            operation: LocalSubscriptionOperation::Cancel { lease },
        });
        let configured = self
            .client
            .stream()
            .set_read_timeout(Some(timeout))
            .and_then(|()| self.client.stream().set_write_timeout(Some(timeout)));
        if let Err(error) = configured {
            self.retire_connection();
            return Err(ClientError::Io(error.to_string()));
        }
        let result = match self
            .client
            .begin_exchange(&request, Instant::now() + timeout)
        {
            Ok(mut exchange) => exchange
                .wait_with(|_| LocalControlExchangeDecision::Continue)
                .map_err(|error| ClientError::Protocol(error.to_string())),
            Err(error) => Err(map_control_error(error)),
        };
        let acknowledged = match &result {
            Ok(LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                request_id: observed,
                lease: acknowledged,
            })) if *observed == request_id && *acknowledged == lease => true,
            _ => false,
        };
        if !acknowledged {
            self.retire_connection();
            return Err(match result {
                Err(error) => error,
                Ok(_) => ClientError::Protocol(
                    "bootstrap lease cancellation was not acknowledged".to_owned(),
                ),
            });
        }
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        if let Err(error) = self
            .client
            .stream()
            .set_read_timeout(Some(self.io_timeout))
            .and_then(|()| {
                self.client
                    .stream()
                    .set_write_timeout(Some(self.io_timeout))
            })
        {
            self.retire_connection();
            return Err(ClientError::Io(error.to_string()));
        }
        Ok(())
    }

    fn request(
        &mut self,
        request: &LocalControlRequest,
    ) -> Result<LocalControlResponse, ClientError> {
        if self.lifecycle == Lifecycle::Released {
            return Err(ClientError::Io(
                "transport already released by a terminal lease cancellation".to_owned(),
            ));
        }
        self.prepare_request()?;
        let response = self.client.request(request).map_err(map_control_error)?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        Ok(response)
    }

    /// Sends exactly one request and resumes its byte offsets across short
    /// readiness timeouts until the exact response arrives or the absolute
    /// deadline/cancellation ends the exchange.
    ///
    /// The stream's configured I/O timeout controls tick granularity. The
    /// callback runs before each potentially blocking socket operation and
    /// after readiness timeouts; keep it short and nonblocking. A stalled,
    /// cancelled, closed, or malformed in-flight exchange retires this socket
    /// immediately, so a later request must reconnect instead of reusing an
    /// ambiguous frame boundary. A stall does not prove the producer rejected
    /// the request; subscription callers must resume their exact retained
    /// lease or reacquire and fully admit a root.
    ///
    /// # Errors
    ///
    /// Returns a setup failure before bytes are sent, or a typed terminal
    /// failure carrying the request correlation and last byte offsets.
    pub fn request_with_tick(
        &mut self,
        request: &LocalControlRequest,
        deadline: Instant,
        tick: impl FnMut(LocalControlExchangeProgress) -> LocalControlExchangeDecision,
    ) -> Result<LocalControlResponse, LocalSubscriptionExchangeError> {
        if self.lifecycle == Lifecycle::Released {
            return Err(LocalSubscriptionExchangeError::Setup(ClientError::Io(
                "transport already released by a terminal lease cancellation".to_owned(),
            )));
        }
        self.prepare_request_until(deadline)
            .map_err(LocalSubscriptionExchangeError::Setup)?;
        let result = {
            let mut exchange = self
                .client
                .begin_exchange(request, deadline)
                .map_err(|error| LocalSubscriptionExchangeError::Setup(map_control_error(error)))?;
            exchange.wait_with(tick)
        };
        match result {
            Ok(response) => {
                self.frames_on_connection = self.frames_on_connection.saturating_add(1);
                Ok(response)
            }
            Err(error) => {
                self.retire_connection();
                Err(LocalSubscriptionExchangeError::Exchange(error))
            }
        }
    }

    fn retire_connection(&mut self) {
        if let Some(interrupt) = self.interrupt.take() {
            interrupt.interrupt();
        }
        self.peer = None;
        self.frames_on_connection = CONNECTION_FRAME_BUDGET;
    }

    /// Sends one bounded request and admits the producer certificate attached
    /// to its successful subscription response.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when framing, certificate decoding, complete
    /// coverage admission, or cursor/event validation fails.
    pub fn subscribe_with_certificate(
        &mut self,
        request: SubscriptionRequest,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        self.exchange(request, capability)
    }

    /// Opens a durable leased subscription at an optional exact cursor.
    ///
    /// The returned lease and cursor are producer values. Callers retain both
    /// and pass the exact cursor back to [`Self::resume_lease`],
    /// [`Self::ack_lease`], or [`Self::renew_lease`] after admitting any
    /// payload carried by a batch.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn open_lease(
        &mut self,
        cursor: Option<Cursor>,
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Open {
            cursor: cursor.map_or_else(Box::<[u8]>::default, |cursor| {
                encode_cursor(cursor).into_boxed_slice()
            }),
            credit,
            lease_ms,
        })
    }

    /// Resumes a producer-issued lease at the caller's exact durable cursor.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn resume_lease(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: Cursor,
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Resume {
            lease,
            cursor: encode_cursor(cursor).into_boxed_slice(),
            credit,
            lease_ms,
        })
    }

    /// Adds bounded event credit to a producer-issued lease.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn credit_lease(
        &mut self,
        lease: LocalSubscriptionId,
        credit: usize,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Credit { lease, credit })
    }

    /// Acknowledges durable consumption through one exact cursor.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn ack_lease(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: Cursor,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Ack {
            lease,
            cursor: encode_cursor(cursor).into_boxed_slice(),
        })
    }

    /// Renews a lease while fencing it to the caller's exact cursor.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn renew_lease(
        &mut self,
        lease: LocalSubscriptionId,
        cursor: Cursor,
        credit: usize,
        lease_ms: u64,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Renew {
            lease,
            cursor: encode_cursor(cursor).into_boxed_slice(),
            credit,
            lease_ms,
        })
    }

    /// Cancels a producer-issued lease and releases owner-side retention.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn cancel_lease(
        &mut self,
        lease: LocalSubscriptionId,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Cancel { lease })
    }

    /// Best-effort terminal release on this exact socket. Never reconnects
    /// or rotates a connection during teardown. The two socket waits each
    /// use the caller's short timeout; native configuration is not preemptible.
    pub(crate) fn cancel_lease_current(
        &mut self,
        lease: LocalSubscriptionId,
        held_on: ConnectionId,
        timeout: Duration,
    ) -> Result<(), ClientError> {
        if self.connection != held_on {
            return Err(ClientError::Protocol(
                "publication lease is not held on this socket".to_owned(),
            ));
        }
        if self.lifecycle == Lifecycle::Released {
            return Err(ClientError::Io(
                "transport already released by a terminal lease cancellation".to_owned(),
            ));
        }
        self.lifecycle = Lifecycle::Released;
        let result = (|| {
            self.client
                .stream()
                .set_read_timeout(Some(timeout))
                .and_then(|()| self.client.stream().set_write_timeout(Some(timeout)))
                .map_err(|error| ClientError::Io(error.to_string()))?;
            let request_id = self.next_request_id;
            self.next_request_id = request_id.checked_add(1).ok_or_else(|| {
                ClientError::Protocol("subscription request id exhausted".to_owned())
            })?;
            let request = LocalControlRequest::Subscription(LocalSubscriptionRequest {
                request_id,
                operation: LocalSubscriptionOperation::Cancel { lease },
            });
            match self.client.request(&request).map_err(map_control_error)? {
                LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                    request_id: observed,
                    lease: acknowledged,
                }) if observed == request_id && acknowledged == lease => Ok(()),
                _ => Err(ClientError::Protocol(
                    "exact-socket lease cancellation was not acknowledged".to_owned(),
                )),
            }
        })();
        // Cancellation is terminal even when its bounded exchange fails. Do
        // not leave an ambiguous frame or an idle socket alive for reuse.
        self.retire_connection();
        result
    }

    /// Requests one bounded snapshot page for a reset descriptor. The page
    /// token is empty for the first page and must otherwise be copied exactly
    /// from the preceding [`LocalSubscriptionResponse::SnapshotPage`] reply.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    pub fn snapshot_page(
        &mut self,
        lease: LocalSubscriptionId,
        page: impl Into<Box<[u8]>>,
        credit: usize,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        self.lease_request(LocalSubscriptionOperation::Page {
            lease,
            page: page.into(),
            credit,
        })
    }

    pub(crate) fn lease_request(
        &mut self,
        operation: LocalSubscriptionOperation,
    ) -> Result<LocalSubscriptionResponse, ClientError> {
        let expected_lease = match &operation {
            LocalSubscriptionOperation::Open { .. } => None,
            LocalSubscriptionOperation::Resume { lease, .. }
            | LocalSubscriptionOperation::Credit { lease, .. }
            | LocalSubscriptionOperation::Ack { lease, .. }
            | LocalSubscriptionOperation::Renew { lease, .. }
            | LocalSubscriptionOperation::Cancel { lease }
            | LocalSubscriptionOperation::Page { lease, .. } => Some(*lease),
        };
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("subscription request id exhausted".to_owned()))?;
        let raw = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id,
            operation,
        });
        let response = self.request(&raw)?;
        match response {
            LocalControlResponse::Subscription(response) => {
                if expected_lease.is_some_and(|expected| response.lease() != expected) {
                    return Err(ClientError::Protocol(
                        "locald subscription response lease mismatch".to_owned(),
                    ));
                }
                Ok(response)
            }
            LocalControlResponse::Rejected { message, .. } => Err(ClientError::Protocol(format!(
                "locald subscription: {message}"
            ))),
            LocalControlResponse::SemanticStaleSelection {
                request_id: observed,
            } => {
                if observed != request_id {
                    return Err(ClientError::Protocol(
                        "subscription response correlation mismatch".to_owned(),
                    ));
                }
                Err(ClientError::StaleSelection)
            }
            LocalControlResponse::SemanticRangeChunk { .. }
            | LocalControlResponse::SemanticMetadataChunk { .. } => Err(ClientError::Protocol(
                "locald returned a semantic response to a subscription request".to_owned(),
            )),
            LocalControlResponse::Accepted { .. }
            | LocalControlResponse::AcceptedPayload { .. }
            | LocalControlResponse::Queued { .. } => Err(ClientError::Protocol(
                "locald leased subscription response omitted its lease state".to_owned(),
            )),
        }
    }

    /// Performs one typed subscription operation using the resumable bounded
    /// exchange path. The decoded producer rejection remains distinct from a
    /// local frame, correlation, or response-validation failure.
    pub(crate) fn lease_request_with_tick(
        &mut self,
        operation: LocalSubscriptionOperation,
        deadline: Instant,
        tick: impl FnMut(LocalControlExchangeProgress) -> LocalControlExchangeDecision,
    ) -> Result<LocalSubscriptionResponse, LocalSubscriptionExchangeError> {
        let expected_lease = match &operation {
            LocalSubscriptionOperation::Open { .. } => None,
            LocalSubscriptionOperation::Resume { lease, .. }
            | LocalSubscriptionOperation::Credit { lease, .. }
            | LocalSubscriptionOperation::Ack { lease, .. }
            | LocalSubscriptionOperation::Renew { lease, .. }
            | LocalSubscriptionOperation::Cancel { lease }
            | LocalSubscriptionOperation::Page { lease, .. } => Some(*lease),
        };
        let request_id = self.next_request_id;
        self.next_request_id = request_id.checked_add(1).ok_or_else(|| {
            LocalSubscriptionExchangeError::Setup(ClientError::Protocol(
                "subscription request id exhausted".to_owned(),
            ))
        })?;
        let raw = LocalControlRequest::Subscription(LocalSubscriptionRequest {
            request_id,
            operation,
        });
        match self.request_with_tick(&raw, deadline, tick)? {
            LocalControlResponse::Subscription(response) => {
                if expected_lease.is_some_and(|expected| response.lease() != expected) {
                    return Err(LocalSubscriptionExchangeError::Invalid(
                        ClientError::Protocol(
                            "locald subscription response lease mismatch".to_owned(),
                        ),
                    ));
                }
                if response.request_id() != request_id {
                    return Err(LocalSubscriptionExchangeError::Invalid(
                        ClientError::Protocol(
                            "subscription response correlation mismatch".to_owned(),
                        ),
                    ));
                }
                Ok(response)
            }
            LocalControlResponse::Rejected {
                request_id: observed,
                message,
            } if observed == request_id => Err(LocalSubscriptionExchangeError::Rejected {
                request_id,
                message,
            }),
            LocalControlResponse::SemanticStaleSelection {
                request_id: observed,
            } if observed == request_id => Err(LocalSubscriptionExchangeError::Invalid(
                ClientError::StaleSelection,
            )),
            _ => Err(LocalSubscriptionExchangeError::Invalid(
                ClientError::Protocol(
                    "locald returned an invalid response to a subscription operation".to_owned(),
                ),
            )),
        }
    }

    fn exchange(
        &mut self,
        request: SubscriptionRequest,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("subscription request id exhausted".to_owned()))?;
        let raw = LocalControlRequest::Subscribe {
            request_id,
            cursor: encode_cursor(request.cursor).into_boxed_slice(),
            credit: request.credit,
        };
        let response = self.request(&raw)?;
        let read = decode_control_response(response, request_id, request, capability)?;
        enforce_credit(read, request.credit)
    }

    fn exchange_against(
        &mut self,
        request: SubscriptionRequest,
        root: &ViewRoot,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| ClientError::Protocol("subscription request id exhausted".to_owned()))?;
        let raw = LocalControlRequest::Subscribe {
            request_id,
            cursor: encode_cursor(request.cursor).into_boxed_slice(),
            credit: request.credit,
        };
        let response = self.request(&raw)?;
        let read = match response {
            LocalControlResponse::AcceptedPayload {
                request_id: observed,
                payload,
            } => {
                if observed != request_id {
                    return Err(ClientError::Protocol(
                        "subscription response correlation mismatch".to_owned(),
                    ));
                }
                subscription_read_from_bytes_against(&payload, request.cursor, root, capability)?
            }
            other => decode_control_response(other, request_id, request, capability)?,
        };
        enforce_credit(read, request.credit)
    }
}

#[cfg(all(test, any(unix, windows)))]
#[allow(clippy::expect_used, clippy::panic)]
mod bootstrap_exhaustion_tests {
    use super::*;
    use backend_replication::{decode_request, encode_response, read_frame, write_frame};

    #[test]
    fn exhausted_bootstrap_cancel_retires_the_exact_socket_without_sending_cancel() {
        let (stream, mut owner) = crate::test_socket::local_pair();
        owner
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("bounded owner read");
        let lease = LocalSubscriptionId::from_bytes([83; 16]);
        let owner_thread = std::thread::spawn(move || {
            let request_body = read_frame(&mut owner, control_limits()).expect("open frame");
            let LocalControlRequest::Subscription(open) =
                decode_request(&request_body, control_limits()).expect("open request")
            else {
                panic!("expected subscription open");
            };
            assert_eq!(open.request_id, u64::MAX - 1);
            assert!(matches!(
                open.operation,
                LocalSubscriptionOperation::Open { lease_ms, .. }
                    if lease_ms == BOOTSTRAP_LEASE.get()
            ));
            write_frame(
                &mut owner,
                &encode_response(
                    &LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                        request_id: open.request_id,
                        lease,
                        page: Box::new([]),
                        next: None,
                        credit: PUBLICATION_CREDIT,
                        payload: Box::from(b"unused without an authenticated peer".as_slice()),
                    }),
                    control_limits(),
                )
                .expect("bounded page response"),
                control_limits(),
            )
            .expect("send page response");

            // Exhaustion happens before a Cancel frame is built or sent. The
            // stream must close promptly; otherwise the known lease relies on
            // its finite bootstrap term for owner-side expiry.
            matches!(
                read_frame(&mut owner, control_limits()),
                Err(backend_replication::LocalControlError::Closed)
            )
        });

        let mut transport = LocalSubscriptionTransport::from_stream(stream);
        transport.next_request_id = u64::MAX - 1;
        let correlated_socket = transport.connection();
        let error = transport
            .bootstrap_root()
            .expect_err("a stream without peer proof cannot hydrate");

        assert!(
            matches!(error, ClientError::Protocol(message) if message.contains("authenticated")),
            "the original hydration failure remains primary: {error:?}"
        );
        assert_eq!(transport.next_request_id, u64::MAX);
        assert_eq!(transport.connection(), correlated_socket);
        assert_eq!(transport.lifecycle, Lifecycle::Serving);
        assert_eq!(transport.frames_on_connection, CONNECTION_FRAME_BUDGET);
        assert!(
            transport.interrupt.is_none(),
            "the exact stream was retired"
        );
        assert!(transport.peer.is_none(), "retirement clears peer authority");
        assert_eq!(
            transport.io_timeout,
            Duration::from_secs(30),
            "request-ID refusal happens before temporary cancel timeouts are installed"
        );

        assert!(
            owner_thread.join().expect("owner thread observes EOF"),
            "no Cancel frame was sent and the exact socket closed"
        );
        let next = transport.request(&LocalControlRequest::Subscription(
            LocalSubscriptionRequest {
                request_id: 1,
                operation: LocalSubscriptionOperation::Cancel { lease },
            },
        ));
        assert!(matches!(
            next,
            Err(ClientError::Io(message)) if message.contains("frame budget")
        ));
    }
}

#[cfg(any(unix, windows))]
fn connect_stream(
    path: &Path,
    timeout: Duration,
) -> Result<backend_replication::LocalStream, ClientError> {
    let stream = backend_replication::connect_local_timeout(path, timeout)
        .map_err(crate::map_endpoint_connect_error)?;
    crate::configure(&stream)?;
    Ok(stream)
}

#[cfg(any(unix, windows))]
impl SubscriptionTransport for LocalSubscriptionTransport {
    fn subscribe(&mut self, request: SubscriptionRequest) -> Result<CursorRead, ClientError> {
        self.exchange(request, None)
    }
}

#[cfg(any(unix, windows))]
fn encode_cursor(cursor: Cursor) -> Vec<u8> {
    cursor.encode_control().into_vec()
}

#[cfg(any(unix, windows))]
fn decode_control_response(
    response: LocalControlResponse,
    request_id: u64,
    request: SubscriptionRequest,
    capability: Option<CoverageCapability>,
) -> Result<CursorRead, ClientError> {
    let observed_request_id = response.request_id();
    if observed_request_id != request_id {
        return Err(ClientError::Protocol(
            "subscription response correlation mismatch".to_owned(),
        ));
    }
    match response {
        LocalControlResponse::AcceptedPayload { payload, .. } => {
            subscription_read_from_bytes(&payload, request.cursor, capability)
        }
        LocalControlResponse::Accepted { .. } => Err(ClientError::Protocol(
            "locald subscription response omitted its cursor/read payload".to_owned(),
        )),
        LocalControlResponse::Rejected { message, .. } => Err(ClientError::Protocol(format!(
            "locald subscription: {message}"
        ))),
        LocalControlResponse::Queued { .. } => Err(ClientError::Protocol(
            "invalid locald subscription response status".to_owned(),
        )),
        LocalControlResponse::Subscription(_) => Err(ClientError::Protocol(
            "leased subscription response requires the lease adapter".to_owned(),
        )),
        LocalControlResponse::SemanticStaleSelection { .. } => Err(ClientError::StaleSelection),
        LocalControlResponse::SemanticRangeChunk { .. }
        | LocalControlResponse::SemanticMetadataChunk { .. } => Err(ClientError::Protocol(
            "locald returned a semantic response to a subscription request".to_owned(),
        )),
    }
}

#[cfg(any(unix, windows))]
fn enforce_credit(read: CursorRead, credit: usize) -> Result<CursorRead, ClientError> {
    if let CursorRead::Events { events, .. } = &read
        && events.len() > credit
    {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    Ok(read)
}

#[cfg(any(unix, windows))]
impl CertifiedSubscriptionTransport for LocalSubscriptionTransport {
    fn subscribe_with_certificate(
        &mut self,
        request: SubscriptionRequest,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        Self::subscribe_with_certificate(self, request, capability)
    }

    fn subscribe_with_certificate_against(
        &mut self,
        request: SubscriptionRequest,
        root: &ViewRoot,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        self.exchange_against(request, root, capability)
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::panic)]
mod exchange_tests {
    use super::*;
    use backend_replication::{
        LocalControlExchangeFailure, LocalControlExchangePhase, decode_request, encode_response,
        read_frame, write_frame,
    };
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;

    #[test]
    fn bootstrap_releases_a_known_lease_on_the_same_socket_and_restores_transport_timeouts() {
        let lease = LocalSubscriptionId::from_bytes([19; 16]);
        let cursor = Cursor::new();
        let (stream, mut owner) = UnixStream::pair().expect("socket pair");
        owner
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("owner read timeout");
        let server = std::thread::spawn(move || {
            let first = read_frame(&mut owner, control_limits()).expect("open frame");
            let LocalControlRequest::Subscription(open) =
                decode_request(&first, control_limits()).expect("open request")
            else {
                panic!("expected subscription open");
            };
            let (request_id, operation) = (open.request_id, open.operation);
            assert!(matches!(
                operation,
                LocalSubscriptionOperation::Open {
                    credit,
                    lease_ms,
                    ..
                } if credit == PUBLICATION_CREDIT && lease_ms == BOOTSTRAP_LEASE.get()
            ));
            write_frame(
                &mut owner,
                &encode_response(
                    &LocalControlResponse::Subscription(LocalSubscriptionResponse::SnapshotPage {
                        request_id,
                        lease,
                        page: Box::new([]),
                        next: None,
                        credit: PUBLICATION_CREDIT,
                        payload: Box::from(b"not a producer proof".as_slice()),
                    }),
                    control_limits(),
                )
                .expect("bootstrap page response"),
                control_limits(),
            )
            .expect("send bootstrap page");

            let cancel = read_frame(&mut owner, control_limits()).expect("cancel frame");
            let LocalControlRequest::Subscription(cancel) =
                decode_request(&cancel, control_limits()).expect("cancel request")
            else {
                panic!("expected subscription cancel");
            };
            assert!(
                matches!(cancel.operation, LocalSubscriptionOperation::Cancel { lease: id } if id == lease)
            );
            write_frame(
                &mut owner,
                &encode_response(
                    &LocalControlResponse::Subscription(LocalSubscriptionResponse::Cancelled {
                        request_id: cancel.request_id,
                        lease,
                    }),
                    control_limits(),
                )
                .expect("cancel response"),
                control_limits(),
            )
            .expect("send cancel acknowledgement");

            let renew = read_frame(&mut owner, control_limits()).expect("reused socket frame");
            let LocalControlRequest::Subscription(renew) =
                decode_request(&renew, control_limits()).expect("renew request")
            else {
                panic!("expected subscription renewal");
            };
            assert!(
                matches!(renew.operation, LocalSubscriptionOperation::Renew { lease: id, .. } if id == lease)
            );
            write_frame(
                &mut owner,
                &encode_response(
                    &LocalControlResponse::Subscription(LocalSubscriptionResponse::Renewed {
                        request_id: renew.request_id,
                        lease,
                        cursor: cursor.encode_control(),
                        credit: PUBLICATION_CREDIT,
                        lease_ms: BOOTSTRAP_LEASE.get(),
                    }),
                    control_limits(),
                )
                .expect("renew response"),
                control_limits(),
            )
            .expect("send renewal");
        });
        let mut transport = LocalSubscriptionTransport::from_stream(stream);
        let error = transport
            .bootstrap_root()
            .expect_err("unauthenticated stream cannot admit the returned proof");
        assert!(
            matches!(error, ClientError::Protocol(message) if message.contains("authenticated"))
        );
        transport
            .renew_lease(lease, cursor, PUBLICATION_CREDIT, BOOTSTRAP_LEASE.get())
            .expect("successful bootstrap cleanup leaves the transport usable");
        server.join().expect("owner");
    }

    #[test]
    fn bootstrap_observes_an_injected_clock_expiry_during_a_partial_frame() {
        use crate::monotonic::ManualClock;

        let clock = ManualClock::new();
        let owner_clock = Arc::clone(&clock);
        let (stream, mut owner) = UnixStream::pair().expect("socket pair");
        let server = std::thread::spawn(move || {
            let request = read_frame(&mut owner, control_limits()).expect("open request");
            let _ = decode_request(&request, control_limits()).expect("open control");
            owner
                .write_all(&1_u32.to_be_bytes()[..1])
                .expect("partial response header");
            owner_clock.advance(crate::lease_contract::RESET_BASE_TIME);
            let mut byte = [0_u8; 1];
            assert_eq!(owner.read(&mut byte).expect("observe retired socket"), 0);
        });
        let mut transport = LocalSubscriptionTransport::from_stream(stream).with_clock(clock);
        let error = transport
            .bootstrap_root()
            .expect_err("manual time expires while a response header is incomplete");
        assert!(matches!(error, ClientError::Protocol(message) if message.contains("time budget")));
        server.join().expect("owner");
    }

    #[test]
    fn terminal_exchange_closes_exact_socket_and_fails_closed_without_endpoint() {
        let (stream, mut peer) = UnixStream::pair().expect("local socket pair");
        let mut transport = LocalSubscriptionTransport::from_stream(stream);
        let request = LocalControlRequest::Subscribe {
            request_id: 42,
            cursor: Box::from(*b"cursor"),
            credit: 1,
        };
        let error = transport
            .request_with_tick(&request, Instant::now() + Duration::from_secs(1), |_| {
                LocalControlExchangeDecision::Cancel
            })
            .expect_err("exchange cancelled before writing");
        assert!(matches!(
            error,
            LocalSubscriptionExchangeError::Exchange(exchange)
                if exchange.failure == LocalControlExchangeFailure::Cancelled
                    && exchange.progress.phase == LocalControlExchangePhase::Sending
                    && exchange.progress.write_offset == 0
        ));
        let mut byte = [0_u8; 1];
        assert_eq!(peer.read(&mut byte).expect("peer observes close"), 0);
        let next =
            transport.request_with_tick(&request, Instant::now() + Duration::from_secs(1), |_| {
                LocalControlExchangeDecision::Continue
            });
        assert!(matches!(
            next,
            Err(LocalSubscriptionExchangeError::Setup(ClientError::Io(_)))
        ));
    }
}
