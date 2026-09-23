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
    LocalControlLimits, LocalControlRequest, LocalControlResponse, LocalSubscriptionId,
    LocalSubscriptionOperation, LocalSubscriptionRequest, LocalSubscriptionResponse,
    ReplicationError,
};
#[cfg(any(unix, windows))]
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(any(unix, windows))]
const CONNECTION_FRAME_BUDGET: usize = 240;

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
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
    endpoint: Option<PathBuf>,
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
        let path = path.as_ref();
        let endpoint = backend_replication::UnixEndpointRef::new(path)
            .map_err(|_| ClientError::Transport(ReplicationError::MessageTooLarge))?;
        let path = endpoint.as_path();
        let stream = connect_stream(path)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
            .map_err(crate::map_peer_authentication_error)?;
        Ok(Self {
            client: LocalControlClient::new(stream, control_limits()),
            peer: Some(peer),
            endpoint: Some(path.to_path_buf()),
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
        Self {
            client: LocalControlClient::new(stream, control_limits()),
            peer: None,
            endpoint: None,
            frames_on_connection: 0,
            next_request_id: 1,
        }
    }

    fn prepare_request(&mut self) -> Result<(), ClientError> {
        if self.frames_on_connection < CONNECTION_FRAME_BUDGET {
            return Ok(());
        }
        let endpoint = self.endpoint.as_deref().ok_or_else(|| {
            ClientError::Io("local control connection reached its bounded frame budget".to_owned())
        })?;
        let stream = connect_stream(endpoint)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint)
            .map_err(crate::map_peer_authentication_error)?;
        self.client = LocalControlClient::new(stream, control_limits());
        self.peer = Some(peer);
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
        self.prepare_request()?;
        let response = self.client.request(request).map_err(map_control_error)?;
        self.frames_on_connection = self.frames_on_connection.saturating_add(1);
        Ok(response)
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

    fn lease_request(
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
            LocalControlResponse::Accepted { .. }
            | LocalControlResponse::AcceptedPayload { .. }
            | LocalControlResponse::Queued { .. } => Err(ClientError::Protocol(
                "locald leased subscription response omitted its lease state".to_owned(),
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
        if let Some(read) =
            self.peer_admitted_reset(&response, request_id, request, capability.clone())?
        {
            return enforce_credit(read, request.credit);
        }
        let read = decode_control_response(response, request_id, request, capability)?;
        enforce_credit(read, request.credit)
    }

    /// Admits a complete reset through this connection's authenticated peer
    /// when the caller holds no coverage capability.
    ///
    /// A resumed cursor can legitimately be answered by a reset, most plainly
    /// after the owner restarted and its retained suffix is gone. A complete
    /// view is admitted only against a capability issued by the trusted
    /// producer boundary; for a local transport that boundary is the
    /// same-user peer authenticated when the connection was made, exactly as
    /// [`Self::bootstrap_root`] uses it. Without this, a caller that had not
    /// retained a capability for the new root (and none can, across a
    /// restart) could never resume, only fail. A caller-supplied capability
    /// still takes precedence, and an unauthenticated stream still refuses.
    fn peer_admitted_reset(
        &self,
        response: &LocalControlResponse,
        request_id: u64,
        request: SubscriptionRequest,
        capability: Option<CoverageCapability>,
    ) -> Result<Option<CursorRead>, ClientError> {
        let (None, Some(peer)) = (capability.as_ref(), self.peer.as_ref()) else {
            return Ok(None);
        };
        let LocalControlResponse::AcceptedPayload {
            request_id: observed,
            payload,
        } = response
        else {
            return Ok(None);
        };
        if *observed != request_id || !is_reset_page(payload) {
            return Ok(None);
        }
        let claim = snapshot_page_from_bytes_with_verifier(payload, request.cursor, None, peer)?;
        if claim.next_after().is_some() {
            return Err(ClientError::Protocol(
                "paged reset requires a durable snapshot lease".to_owned(),
            ));
        }
        SnapshotHydrator::start(request.cursor, claim)
            .map_err(ClientError::Protocol)?
            .finish()
            .map(Some)
            .map_err(ClientError::Protocol)
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
        if let Some(read) =
            self.peer_admitted_reset(&response, request_id, request, capability.clone())?
        {
            return enforce_credit(read, request.credit);
        }
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
fn is_reset_page(payload: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|kind| kind == "reset_page")
}

#[cfg(any(unix, windows))]
fn connect_stream(path: &Path) -> Result<backend_replication::LocalStream, ClientError> {
    let stream = backend_replication::LocalStream::connect(path)
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
