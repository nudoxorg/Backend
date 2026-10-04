//! Shared local subscription transport for GUI, CLI, MCP, and tests.
//!
//! Requests use locald's bounded `LDC2` control envelope. Successful control
//! replies must carry a typed subscription payload; an acknowledgement without
//! a cursor/read is rejected because it cannot distinguish an empty suffix from
//! a dropped event or reset.

#[cfg(any(unix, windows))]
use crate::subscription::{
    snapshot_page_from_bytes_with_verifier, subscription_read_from_bytes,
    subscription_read_from_bytes_against,
};
#[cfg(any(unix, windows))]
use crate::{
    CertifiedSubscriptionTransport, ClientError, MAX_EVENTS, MAX_FRAME, SubscriptionRequest,
    SubscriptionTransport,
};
#[cfg(any(unix, windows))]
use backend_library::{CoverageCapability, Cursor, CursorRead, SnapshotHydrator, ViewRoot};
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
        if self.frames_on_connection < CONNECTION_FRAME_BUDGET && !self.client.requires_reconnect() {
            return Ok(());
        }
        let endpoint = self.endpoint.as_deref().ok_or_else(|| {
            ClientError::Io("local control connection reached its bounded frame budget".to_owned())
        })?;
        let stream = connect_stream(endpoint, self.connect_timeout)?;
        stream
            .set_read_timeout(Some(self.io_timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.io_timeout)))
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .map_err(crate::map_peer_authentication_error)?;
        let client = LocalControlClient::new(stream, control_limits());
        if let Some(interrupt) = &self.interrupt {
            interrupt.replace(client.stream())?;
        } else {
            self.interrupt = Some(crate::TransportInterrupt::new(client.stream())?);
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
        let mut response = self.open_lease(None, MAX_EVENTS, 30_000)?;
        let mut expected_page: Box<[u8]> = Box::new([]);
        let mut hydrator: Option<SnapshotHydrator> = None;
        loop {
            let LocalSubscriptionResponse::SnapshotPage {
                lease,
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
            if page != expected_page {
                return Err(ClientError::Protocol(
                    "bootstrap snapshot page continuation mismatch".to_owned(),
                ));
            }
            let peer = self.peer.as_ref().ok_or_else(|| {
                ClientError::Protocol("bootstrap requires an authenticated local peer".to_owned())
            })?;
            let claim = snapshot_page_from_bytes_with_verifier(&payload, previous, None, peer)?;
            let admitted_next = claim.next_token().map_err(ClientError::Protocol)?;
            if admitted_next.as_deref() != next.as_deref() {
                return Err(ClientError::Protocol(
                    "bootstrap snapshot continuation is not authenticated".to_owned(),
                ));
            }
            let current = match hydrator.take() {
                Some(mut current) => {
                    current.push_page(claim).map_err(ClientError::Protocol)?;
                    current
                }
                None => SnapshotHydrator::start(previous, claim).map_err(ClientError::Protocol)?,
            };
            if current.is_complete() {
                return match current.finish().map_err(ClientError::Protocol)? {
                    CursorRead::Reset { cursor, root, .. } => Ok((*root, cursor)),
                    CursorRead::Events { .. } => Err(ClientError::Protocol(
                        "bootstrap snapshot completed as an event batch".to_owned(),
                    )),
                };
            }
            expected_page = next.ok_or_else(|| {
                ClientError::Protocol("bootstrap snapshot omitted its continuation".to_owned())
            })?;
            hydrator = Some(current);
            response = self.snapshot_page(lease, expected_page.to_vec(), MAX_EVENTS)?;
        }
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
        self.prepare_request()
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
    use backend_replication::{LocalControlExchangeFailure, LocalControlExchangePhase};
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

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
        let next = transport.request_with_tick(&request, Instant::now() + Duration::from_secs(1), |_| {
            LocalControlExchangeDecision::Continue
        });
        assert!(matches!(
            next,
            Err(LocalSubscriptionExchangeError::Setup(ClientError::Io(_)))
        ));
    }
}
