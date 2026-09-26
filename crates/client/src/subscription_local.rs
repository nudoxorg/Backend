//! Shared local subscription transport for GUI, CLI, MCP, and tests.
//!
//! Requests use locald's bounded `LDC2` control envelope. Successful control
//! replies must carry a typed subscription payload; an acknowledgement without
//! a cursor/read is rejected because it cannot distinguish an empty suffix from
//! a dropped event or reset.

#[cfg(any(unix, windows))]
use crate::subscription::subscription_read_from_bytes;
#[cfg(any(unix, windows))]
use crate::{
    CertifiedSubscriptionTransport, ClientError, MAX_FRAME, SubscriptionRequest,
    SubscriptionTransport,
};
#[cfg(any(unix, windows))]
use backend_library::{CoverageCapability, Cursor, CursorRead, ViewRoot};
#[cfg(any(unix, windows))]
use backend_replication::{
    LOCAL_CONTROL_MAX_CURSOR, LOCAL_CONTROL_MAX_ERROR, LocalControlClient, LocalControlError,
    LocalControlLimits, LocalControlResponse, ReplicationError,
};
#[cfg(any(unix, windows))]
use std::path::{Path, PathBuf};

#[cfg(any(unix, windows))]
mod transport;

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
