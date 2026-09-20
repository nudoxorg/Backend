//! Desktop subscription requests and strict wire decoding.

use crate::{ClientError, MAX_EVENTS};
#[cfg(test)]
use backend_library::encode_id;
use backend_library::{
    CoverageCapability, Cursor, CursorRead, CursorSub, SnapshotHydrator, SnapshotPageClaim,
    ViewRoot, ViewStateRoot,
};
#[cfg(test)]
use backend_library::{admit_key_against, admit_root_against, admit_version_against};
use backend_replication::ReplicationError;
use backend_version::ProducerObservationVerifier;
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

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

/// Fallible subscription transport used by desktop composition roots.
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
    /// returned by the trusted producer boundary.
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

/// Compatibility seam for an embedded subscription owner.
pub trait LocalEngine {
    /// Polls a bounded branch/log cursor.
    fn poll(&mut self, sub: &mut CursorSub) -> CursorRead;
}

#[cfg(test)]
pub(crate) fn subscription_request_value(request: SubscriptionRequest) -> Value {
    json!({
        "version": backend_library::protocol_version(),
        "kind": "subscribe",
        "cursor": cursor_value(request.cursor),
        "credit": request.credit,
    })
}

#[cfg(test)]
pub(crate) fn cursor_value(cursor: Cursor) -> Value {
    json!({
        "recipe": encode_id(cursor.recipe().as_bytes()),
        "version": encode_id(cursor.version().as_bytes()),
        "branch": encode_id(cursor.branch().as_bytes()),
        "log": encode_id(cursor.log().as_bytes()),
        "schema": cursor.schema(),
        "root": encode_id(cursor.root().as_bytes()),
        "sequence": cursor.sequence(),
        "query_offset": 0,
    })
}

#[cfg(test)]
fn reject_unknown_fields(value: &Value, allowed: &[&str], what: &str) -> Result<(), ClientError> {
    let object = value
        .as_object()
        .ok_or_else(|| ClientError::Protocol(format!("{what} is not an object")))?;
    if object
        .keys()
        .any(|field| !allowed.iter().any(|allowed| *allowed == field))
    {
        return Err(ClientError::Protocol(format!("unknown {what} field")));
    }
    Ok(())
}

#[cfg(test)]
fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ClientError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ClientError::Protocol(format!("missing {field}")))
}

#[cfg(test)]
pub(crate) fn cursor_from_value(value: &Value, expected: Cursor) -> Result<Cursor, ClientError> {
    reject_unknown_fields(
        value,
        &[
            "recipe",
            "version",
            "branch",
            "log",
            "schema",
            "root",
            "sequence",
            "query_offset",
        ],
        "cursor",
    )?;
    let recipe = admit_key_against::<backend_library::canonical::ViewRecipeSchema>(
        required_string(value, "recipe")?,
        expected.recipe(),
    )
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let version = admit_version_against::<backend_library::canonical::ViewVersionSchema>(
        required_string(value, "version")?,
        expected.version(),
    )
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let branch = admit_key_against::<backend_library::canonical::BranchSchema>(
        required_string(value, "branch")?,
        expected.branch(),
    )
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let log = admit_key_against::<backend_library::canonical::LogSchema>(
        required_string(value, "log")?,
        expected.log(),
    )
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let schema = value
        .get("schema")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .ok_or_else(|| ClientError::Protocol("invalid cursor schema".to_owned()))?;
    if schema != expected.schema() || schema != backend_library::CURSOR_SCHEMA {
        return Err(ClientError::Protocol(
            "unsupported cursor schema".to_owned(),
        ));
    }
    let root = admit_root_against::<backend_library::ViewRelation>(
        required_string(value, "root")?,
        expected.root(),
    )
    .map_err(|error| ClientError::Protocol(error.to_string()))?;
    let sequence = value
        .get("sequence")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol("invalid cursor sequence".to_owned()))?;
    if sequence != expected.sequence() {
        return Err(ClientError::CursorMismatch);
    }
    let query_offset = value
        .get("query_offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| ClientError::Protocol("invalid cursor query offset".to_owned()))?;
    if query_offset != 0 {
        return Err(ClientError::CursorMismatch);
    }
    let cursor = Cursor::for_view(
        recipe,
        version,
        backend_library::Frontier::new(branch, log, schema, root, sequence),
    );
    Ok(cursor)
}

#[cfg(test)]
pub(crate) fn subscription_read_from_value(
    value: &Value,
    previous: Cursor,
) -> Result<CursorRead, ClientError> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?;
    backend_library::SubscriptionDto::decode(&bytes, previous, None).map_err(ClientError::Protocol)
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

pub(crate) fn snapshot_page_from_bytes_with_capability(
    bytes: &[u8],
    previous: Cursor,
    expected_source_root: Option<ViewStateRoot>,
    capability: Option<CoverageCapability>,
) -> Result<SnapshotPageClaim, ClientError> {
    SnapshotPageClaim::decode(bytes, previous, expected_source_root, capability)
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
