//! Shared subscription requests and strict producer-proof admission.

use crate::{ClientError, MAX_EVENTS};
use backend_library::{
    CoverageCapability, Cursor, CursorRead, ProducerObservationVerifier, SnapshotHydrator,
    SnapshotPageClaim, ViewRoot, ViewStateRoot,
};
use backend_replication::ReplicationError;
use serde_json::Value;

/// One bounded subscription request sent to the daemon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionRequest {
    /// Cursor immediately before the requested suffix.
    pub cursor: Cursor,
    /// Maximum number of events the daemon may return.
    pub credit: usize,
}

impl SubscriptionRequest {
    /// Creates a bounded request.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Transport`] when the requested credit is zero or
    /// exceeds the interactive subscription bound.
    pub fn new(cursor: Cursor, credit: usize) -> Result<Self, ClientError> {
        if credit == 0 || credit > MAX_EVENTS {
            return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
        }
        Ok(Self { cursor, credit })
    }
}

/// Fallible subscription transport used by local product composition roots.
pub trait SubscriptionTransport {
    /// Requests at most the supplied credit and returns an explicit reset when
    /// the event suffix cannot be chained.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when the endpoint rejects the bounded request,
    /// cannot decode a reply, or returns an invalid cursor/root transition.
    fn subscribe(&mut self, request: SubscriptionRequest) -> Result<CursorRead, ClientError>;
}

/// A subscription transport that can admit producer certificates and checked
/// complete-view coverage supplied by the composition root.
pub trait CertifiedSubscriptionTransport: SubscriptionTransport {
    /// Requests one bounded suffix and verifies the producer certificate.
    ///
    /// A `None` capability admits identity-bearing events and incomplete view
    /// roots. A complete reset/view requires the source coverage capability
    /// returned by the trusted producer boundary; a local transport whose
    /// same-user peer was authenticated at connect is that boundary, and
    /// admits a reset through it when no capability is supplied.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError`] when framing, decoding, certificate admission,
    /// or cursor chaining fails.
    fn subscribe_with_certificate(
        &mut self,
        request: SubscriptionRequest,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError>;

    /// Requests and decodes a suffix against the reducer's retained root.
    /// Implementations can override this to admit compact stateful events;
    /// the default preserves compatibility with transports that return the
    /// standalone event form.
    /// # Errors
    ///
    /// Returns an error when the transport payload or checked state is invalid.
    fn subscribe_with_certificate_against(
        &mut self,
        request: SubscriptionRequest,
        root: &ViewRoot,
        capability: Option<CoverageCapability>,
    ) -> Result<CursorRead, ClientError> {
        let _ = root;
        self.subscribe_with_certificate(request, capability)
    }
}

pub(crate) fn subscription_read_from_bytes(
    bytes: &[u8],
    previous: Cursor,
    capability: Option<CoverageCapability>,
) -> Result<CursorRead, ClientError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| ClientError::Protocol(error.to_string()))?;
    if value.get("kind").and_then(Value::as_str) == Some("reset_page") {
        let claim = snapshot_page_from_value_with_capability(&value, previous, None, capability)?;
        if claim.next_after().is_some() {
            return Err(ClientError::Protocol(
                "paged reset requires a durable snapshot lease".to_owned(),
            ));
        }
        return SnapshotHydrator::start(previous, claim)
            .map_err(ClientError::Protocol)?
            .finish()
            .map_err(ClientError::Protocol);
    }
    backend_library::SubscriptionDto::decode(bytes, previous, capability)
        .map_err(ClientError::Protocol)
}

/// Admits a subscription payload against the reducer's current root. Compact
/// view events use that root as their checked transition base and never carry
/// a duplicate serialized view.
pub(crate) fn subscription_read_from_bytes_against(
    bytes: &[u8],
    previous: Cursor,
    root: &ViewRoot,
    capability: Option<CoverageCapability>,
) -> Result<CursorRead, ClientError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| ClientError::Protocol(error.to_string()))?;
    if value.get("kind").and_then(Value::as_str) == Some("reset_page") {
        return subscription_read_from_bytes(bytes, previous, capability);
    }
    backend_library::SubscriptionDto::decode_against_root(bytes, previous, root, capability)
        .map_err(ClientError::Protocol)
}

/// Admits one bounded reset page without requiring the target visible root to
/// have been queried in full first.
///
/// The returned claim is safe to pass to [`SnapshotHydrator::start`].  Its
/// visible root remains deferred until all continuation pages have been
/// admitted and the complete relation has been recomputed.
/// # Errors
///
/// Returns an error when the transport payload or checked state is invalid.
pub fn snapshot_page_from_value(
    value: &Value,
    previous: Cursor,
    expected_source_root: Option<ViewStateRoot>,
) -> Result<SnapshotPageClaim, ClientError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?;
    SnapshotPageClaim::decode(&bytes, previous, expected_source_root, None)
        .map_err(ClientError::Protocol)
}

pub(crate) fn snapshot_page_from_value_with_capability(
    value: &Value,
    previous: Cursor,
    expected_source_root: Option<ViewStateRoot>,
    capability: Option<CoverageCapability>,
) -> Result<SnapshotPageClaim, ClientError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?;
    SnapshotPageClaim::decode(&bytes, previous, expected_source_root, capability)
        .map_err(ClientError::Protocol)
}

/// Admits one reset page from its serialized payload.
/// # Errors
///
/// Returns an error when the transport payload or checked state is invalid.
pub fn snapshot_page_from_bytes(
    bytes: &[u8],
    previous: Cursor,
    expected_source_root: Option<ViewStateRoot>,
) -> Result<SnapshotPageClaim, ClientError> {
    SnapshotPageClaim::decode(bytes, previous, expected_source_root, None)
        .map_err(ClientError::Protocol)
}

pub(crate) fn snapshot_page_from_bytes_with_verifier<V: ProducerObservationVerifier>(
    bytes: &[u8],
    previous: Cursor,
    expected_source_root: Option<ViewStateRoot>,
    verifier: &V,
) -> Result<SnapshotPageClaim, ClientError> {
    SnapshotPageClaim::decode_with_verifier(bytes, previous, expected_source_root, verifier)
        .map_err(ClientError::Protocol)
}
