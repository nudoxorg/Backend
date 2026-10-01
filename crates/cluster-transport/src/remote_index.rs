//! Owner-signed, read-only capabilities for private remote index clients.
//!
//! Product-query grants name one immutable view root. Semantic hydration grants
//! name one exact selected semantic target and generation stamp. The QUIC peer
//! identity and this signed capability are both required; worker execution
//! trust is not consulted by this protocol.

use std::time::Duration;

use iroh::{
    EndpointAddr, EndpointId, SecretKey, Signature,
    endpoint::{Connection, RecvStream, SendStream},
};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

use crate::{
    Endpoint, TransportError, frame_error, read_frame_bounded, write_frame_bounded,
    write_frame_bytes_bounded,
};

/// Encrypted Iroh ALPN for a read-only remote index client session.
pub const REMOTE_INDEX_ALPN: &[u8] = b"/backend/remote-index/3";
/// Largest serialized remote-index capability or session hello.
pub const MAX_REMOTE_INDEX_AUTH_BYTES: usize = 16 * 1024;
/// Largest framed product or semantic message on one connection.
pub const MAX_REMOTE_INDEX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Largest request or response body after reserving space for its typed frame envelope.
pub const MAX_REMOTE_INDEX_BODY_BYTES: usize = MAX_REMOTE_INDEX_FRAME_BYTES - 1024;
/// Maximum signed grant lifetime.
pub const MAX_REMOTE_INDEX_GRANT_LIFETIME_MS: u64 = 30 * 24 * 60 * 60 * 1_000;
/// Maximum request count authorized by one capability.
pub const MAX_REMOTE_INDEX_REQUESTS: u32 = 1_000_000;
/// Maximum semantic bytes authorized by one capability.
pub const MAX_REMOTE_INDEX_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum package or coordinate text in one semantic target grant.
pub const MAX_REMOTE_INDEX_TARGET_BYTES: usize = 4 * 1024;

/// One product read operation that may be named by an owner-issued grant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum RemoteIndexQueryOperation {
    /// Search visible declarations and documentation.
    Search,
    /// Resolve visible declaration names.
    Names,
    /// Read one declaration document.
    Document,
    /// Read captured source for one declaration.
    Source,
    /// Read one package outline.
    Outline,
    /// Read graph neighbors.
    Graph,
    /// Read declarations related to one declaration.
    Related,
    /// Search the selected acquired, discovered, and local package catalog.
    IndexSearch,
}

/// A product query capability is fenced to exactly one materialized view root.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexProductScope {
    /// Exact owner-materialized product view root.
    pub view_root: [u8; 32],
    /// Sorted, duplicate-free allowed product query operations.
    pub operations: Vec<RemoteIndexQueryOperation>,
    /// Exact composite snapshot required for cross-plane index-search pages.
    /// Present if and only if `operations` contains `IndexSearch`.
    pub index_search_snapshot: Option<[u8; 32]>,
}

/// Exact selection identity for one semantic target admitted by locald.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexSemanticSelection {
    /// Canonical local package reference used by `SemanticTargetKey`.
    pub package: String,
    /// Canonical compiler package coordinate used by `SemanticTargetKey`.
    pub coordinate: String,
    /// Canonical two-byte language profile code.
    pub profile: [u8; 2],
    /// Turso authority namespace for the exact selected frontier.
    pub namespace: [u8; 16],
    /// Selected compiler coordinate identity.
    pub source_coordinate: [u8; 32],
    /// Monotone local authority selection revision.
    pub selection_revision: u64,
    /// Exact selected logical root.
    pub selected_root: [u8; 32],
    /// Exact selected immutable object closure.
    pub closure_id: [u8; 32],
    /// Exact semantic-plane catalog root.
    pub catalog_root: [u8; 32],
}

/// Closed permission names carried by a signed remote index capability.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum RemoteIndexPermission {
    /// Product read queries against `product.view_root`.
    ProductRead,
    /// Catalog, manifest, image, and segment ranges for `semantic`.
    SemanticHydration,
}

/// Signed authority for one remote index client identity and bounded read scope.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexCapabilityClaims {
    /// Capability schema version. Version 2 binds discovery index pages to their
    /// composite catalog/search snapshot as well as the product view root.
    pub version: u16,
    /// Iroh identity that issued the capability and serves the remote index.
    pub server: EndpointId,
    /// Exact Iroh client identity permitted to use this grant.
    pub client: EndpointId,
    /// Random grant identity used for bounded replay correlation.
    pub grant_id: [u8; 16],
    /// Grant creation time in Unix milliseconds.
    pub issued_at_unix_ms: u64,
    /// Absolute Unix-millisecond expiry.
    pub expires_at_unix_ms: u64,
    /// Upper bound on authenticated requests across both capability scopes.
    pub request_budget: u32,
    /// Upper bound on returned application bytes across both capability scopes.
    pub byte_budget: u64,
    /// Closed read-only permissions in canonical sorted order.
    pub permissions: Vec<RemoteIndexPermission>,
    /// Optional whole-view product query scope.
    pub product: Option<RemoteIndexProductScope>,
    /// Optional exact-generation semantic hydration scope.
    pub semantic: Option<RemoteIndexSemanticSelection>,
}

/// Owner signature over a bounded remote index capability.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexCapability {
    /// Complete signed claims.
    pub claims: RemoteIndexCapabilityClaims,
    /// Ed25519 signature made by the owner Iroh identity.
    pub signature: Signature,
}

/// Signs read-only remote index grants with the existing owner Iroh identity.
#[derive(Clone)]
pub struct RemoteIndexCapabilityIssuer(SecretKey);

impl RemoteIndexCapabilityIssuer {
    /// Uses the persisted owner identity without exporting its private key.
    #[must_use]
    pub fn new(secret: SecretKey) -> Self {
        Self(secret)
    }

    /// Public issuer identity.
    #[must_use]
    pub fn server(&self) -> EndpointId {
        self.0.public()
    }

    /// Signs one exact read grant after validating its scope and budgets.
    pub fn issue(
        &self,
        claims: RemoteIndexCapabilityClaims,
        now_ms: u64,
    ) -> Result<RemoteIndexCapability, RemoteIndexCapabilityError> {
        validate_claims(&claims, self.server(), now_ms)?;
        let bytes = postcard::to_allocvec(&claims)
            .map_err(|_| RemoteIndexCapabilityError::Invalid("claims encoding"))?;
        if bytes.len() > MAX_REMOTE_INDEX_AUTH_BYTES {
            return Err(RemoteIndexCapabilityError::Invalid("claims size"));
        }
        let capability = RemoteIndexCapability {
            claims,
            signature: self.0.sign(&bytes),
        };
        capability.encode()?;
        Ok(capability)
    }
}

impl RemoteIndexCapability {
    /// Canonically encodes the capability for a mode-0600 grant file.
    pub fn encode(&self) -> Result<Vec<u8>, RemoteIndexCapabilityError> {
        let bytes = postcard::to_allocvec(self)
            .map_err(|_| RemoteIndexCapabilityError::Invalid("capability encoding"))?;
        if bytes.len() > MAX_REMOTE_INDEX_AUTH_BYTES {
            return Err(RemoteIndexCapabilityError::Invalid("capability size"));
        }
        Ok(bytes)
    }

    /// Decodes a bounded canonical grant file.
    pub fn decode(bytes: &[u8]) -> Result<Self, RemoteIndexCapabilityError> {
        if bytes.is_empty() || bytes.len() > MAX_REMOTE_INDEX_AUTH_BYTES {
            return Err(RemoteIndexCapabilityError::Invalid("capability size"));
        }
        let capability: Self = postcard::from_bytes(bytes)
            .map_err(|_| RemoteIndexCapabilityError::Invalid("capability encoding"))?;
        if capability.encode()?.as_slice() != bytes {
            return Err(RemoteIndexCapabilityError::Invalid(
                "noncanonical capability",
            ));
        }
        Ok(capability)
    }

    /// Verifies the signature, Iroh peer binding, lifetime, and closed scope.
    pub fn verify(
        &self,
        server: EndpointId,
        client: EndpointId,
        now_ms: u64,
    ) -> Result<(), RemoteIndexCapabilityError> {
        validate_claims(&self.claims, server, now_ms)?;
        if self.claims.client != client {
            return Err(RemoteIndexCapabilityError::PeerMismatch);
        }
        let bytes = postcard::to_allocvec(&self.claims)
            .map_err(|_| RemoteIndexCapabilityError::Invalid("claims encoding"))?;
        server
            .verify(&bytes, &self.signature)
            .map_err(|_| RemoteIndexCapabilityError::Signature)?;
        Ok(())
    }

    /// Returns the signed grant identity used to scope request replay state.
    #[must_use]
    pub const fn grant_id(&self) -> [u8; 16] {
        self.claims.grant_id
    }
}

/// Operation carried on one authenticated remote-index QUIC connection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RemoteIndexChannel {
    /// Bounded, read-only product `CommandDto` calls.
    ProductQuery,
    /// Existing typed local-control semantic catalog/range calls.
    SemanticHydration,
}

/// A closed status code returned while admitting a remote-index request.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RemoteIndexReject {
    /// The remote capability is expired or its exact selection has changed.
    StaleCapability,
    /// The owner revoked this exact grant after issuing it.
    CapabilityRevoked,
    /// The request is outside the channel or operation scope.
    ScopeDenied,
    /// A request ID was reused for different bytes or exceeded the budget.
    ReplayOrBudget,
    /// A nested typed request failed canonical admission.
    InvalidRequest,
    /// The owner could not serve this request.
    OwnerUnavailable,
}

/// Exact remote-index request sent over a signed session channel.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexRequest {
    /// Monotone per-session correlation identity.
    pub request_id: u64,
    /// Existing canonical product-command or local semantic-control payload.
    pub body: Box<[u8]>,
}

/// Result of one correlated remote-index operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub enum RemoteIndexOutcome {
    /// Canonical response body from the existing product or semantic API.
    Payload(Box<[u8]>),
    /// The product grant's view root is no longer the owner's selected root.
    StaleProductRoot {
        expected: [u8; 32],
        observed: [u8; 32],
    },
    /// Producer coverage changed while the owner executed one query.
    StaleProductSource {
        expected: [u8; 32],
        observed: [u8; 32],
    },
    /// The exact acquired/discovered/local search snapshot authorized by the
    /// grant is no longer the current composite index snapshot.
    StaleProductSnapshot {
        expected: [u8; 32],
        observed: [u8; 32],
    },
    /// The semantic target now selects another exact generation/catalog root.
    StaleSemanticSelection,
    /// Closed refusal; implementation detail strings never cross the network.
    Rejected(RemoteIndexReject),
}

/// Correlated owner response on an authenticated remote-index session.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexResponse {
    /// Exact request identity being answered.
    pub request_id: u64,
    /// Bounded query/hydration result or typed stale/refusal result.
    pub outcome: RemoteIndexOutcome,
}

/// Canonically serialized response frame prepared for exact budget admission.
///
/// The bytes are opaque so callers cannot account one representation and send
/// another. `wire_bytes` includes the four-byte length prefix written on QUIC.
pub struct RemoteIndexPreparedResponse {
    request_id: u64,
    encoded: Box<[u8]>,
}

impl RemoteIndexPreparedResponse {
    /// Number of bytes written on the wire, including the length prefix.
    #[must_use]
    pub fn wire_bytes(&self) -> usize {
        4 + self.encoded.len()
    }
}

/// Encodes one typed response exactly as the remote-index session sends it.
pub fn prepare_remote_index_response(
    response: RemoteIndexResponse,
) -> Result<RemoteIndexPreparedResponse, TransportError> {
    if response.request_id == 0
        || matches!(
            &response.outcome,
            RemoteIndexOutcome::Payload(body)
                if body.is_empty() || body.len() > MAX_REMOTE_INDEX_BODY_BYTES
        )
    {
        return Err(frame_error("remote-index response exceeds its bound"));
    }
    let request_id = response.request_id;
    let encoded =
        postcard::to_allocvec(&RemoteIndexMessage::Response(response)).map_err(frame_error)?;
    if encoded.is_empty() || encoded.len() > MAX_REMOTE_INDEX_FRAME_BYTES {
        return Err(frame_error("remote-index response exceeds its bound"));
    }
    Ok(RemoteIndexPreparedResponse {
        request_id,
        encoded: encoded.into_boxed_slice(),
    })
}

/// First signed-session exchange and subsequent query/hydration messages.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
enum RemoteIndexMessage {
    Hello(RemoteIndexSessionHello),
    Accepted {
        grant_id: [u8; 16],
        session_id: [u8; 16],
        channel: RemoteIndexChannel,
    },
    Rejected(RemoteIndexReject),
    Request(RemoteIndexRequest),
    Response(RemoteIndexResponse),
}

/// First framed message on each remote-index connection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteIndexSessionHello {
    /// Owner-signed read-only capability.
    pub capability: RemoteIndexCapability,
    /// Authorized application channel for this connection.
    pub channel: RemoteIndexChannel,
    /// Per-connection nonce. Replays are idempotent only inside this session.
    pub session_id: [u8; 16],
}

impl RemoteIndexSessionHello {
    /// Builds an authenticated channel hello with a fresh session identity.
    pub fn new(
        capability: RemoteIndexCapability,
        channel: RemoteIndexChannel,
    ) -> Result<Self, RemoteIndexCapabilityError> {
        let random = SecretKey::generate().to_bytes();
        let mut session_id = [0; 16];
        session_id.copy_from_slice(&random[..16]);
        if session_id == [0; 16] {
            return Err(RemoteIndexCapabilityError::Invalid("session identity"));
        }
        Ok(Self {
            capability,
            channel,
            session_id,
        })
    }

    /// Validates the signed capability and requires the permission for this channel.
    pub fn verify(
        &self,
        server: EndpointId,
        client: EndpointId,
        now_ms: u64,
    ) -> Result<(), RemoteIndexCapabilityError> {
        self.capability.verify(server, client, now_ms)?;
        let permission = match self.channel {
            RemoteIndexChannel::ProductQuery => RemoteIndexPermission::ProductRead,
            RemoteIndexChannel::SemanticHydration => RemoteIndexPermission::SemanticHydration,
        };
        if !self.capability.claims.permissions.contains(&permission) || self.session_id == [0; 16] {
            return Err(RemoteIndexCapabilityError::ChannelDenied);
        }
        Ok(())
    }
}

/// Authenticated application session on one direct Iroh connection.
pub struct RemoteIndexSession {
    connection: Connection,
    send: SendStream,
    receive: RecvStream,
    hello: RemoteIndexSessionHello,
    local_identity: EndpointId,
    last_request_id: u64,
    last_responded_request_id: u64,
    last_sent_request_id: u64,
}

impl std::fmt::Debug for RemoteIndexSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteIndexSession")
            .field("peer", &self.connection.remote_id())
            .field("channel", &self.hello.channel)
            .field("grant_id", &self.hello.capability.grant_id())
            .finish_non_exhaustive()
    }
}

impl RemoteIndexSession {
    /// Authenticated peer identity.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.connection.remote_id()
    }

    /// Signed channel selected by the client.
    #[must_use]
    pub const fn channel(&self) -> RemoteIndexChannel {
        self.hello.channel
    }

    /// Authenticated signed capability claims.
    #[must_use]
    pub const fn capability(&self) -> &RemoteIndexCapability {
        &self.hello.capability
    }

    /// Returns an opaque owner-grant proof for a client-side product session.
    ///
    /// Both values are minted only after the accepted Iroh session is bound to
    /// the exact owner-signed capability. The server side cannot obtain this
    /// pair because its authenticated peer is the client, not the capability
    /// issuer.
    #[must_use]
    pub fn product_coverage_authentication(
        &self,
    ) -> Option<(
        RemoteIndexAuthenticatedPeer,
        RemoteIndexProductCapabilityReceipt,
    )> {
        authenticated_product_coverage(
            self.peer(),
            self.local_identity,
            self.channel(),
            &self.hello.capability,
            remote_index_now().ok()?,
        )
    }

    /// Receives one bounded request whose ID increases within this session.
    pub async fn receive_request(&mut self) -> Result<RemoteIndexRequest, TransportError> {
        if self.last_request_id != self.last_responded_request_id {
            self.connection
                .close(1_u32.into(), b"previous request is still in flight");
            return Err(frame_error(
                "previous remote-index request has no correlated response",
            ));
        }
        let RemoteIndexMessage::Request(request) =
            read_frame_bounded(&mut self.receive, MAX_REMOTE_INDEX_FRAME_BYTES).await?
        else {
            return Err(frame_error("unexpected remote-index request frame"));
        };
        if request.request_id <= self.last_request_id
            || request.body.is_empty()
            || request.body.len() > MAX_REMOTE_INDEX_BODY_BYTES
        {
            self.connection
                .close(1_u32.into(), b"invalid request sequence");
            return Err(frame_error("remote-index request ID or size is invalid"));
        }
        self.last_request_id = request.request_id;
        Ok(request)
    }

    /// Sends one bounded response with exact request correlation.
    pub async fn send_response(
        &mut self,
        response: &RemoteIndexResponse,
    ) -> Result<(), TransportError> {
        let prepared = prepare_remote_index_response(response.clone())?;
        self.send_prepared_response(prepared).await
    }

    /// Sends a response previously prepared for exact wire-byte admission.
    pub async fn send_prepared_response(
        &mut self,
        response: RemoteIndexPreparedResponse,
    ) -> Result<(), TransportError> {
        if !remote_response_can_be_sent(
            response.request_id,
            self.last_request_id,
            self.last_responded_request_id,
        ) {
            self.connection
                .close(1_u32.into(), b"request correlation mismatch");
            return Err(frame_error(
                "remote-index response does not match the outstanding request",
            ));
        }
        write_frame_bytes_bounded(
            &mut self.send,
            &response.encoded,
            MAX_REMOTE_INDEX_FRAME_BYTES,
        )
        .await?;
        self.last_responded_request_id = response.request_id;
        Ok(())
    }

    /// Sends one bounded query/hydration request with a strictly increasing ID.
    pub async fn send_request(
        &mut self,
        request: &RemoteIndexRequest,
    ) -> Result<(), TransportError> {
        if request.request_id <= self.last_sent_request_id
            || request.body.is_empty()
            || request.body.len() > MAX_REMOTE_INDEX_BODY_BYTES
        {
            return Err(frame_error("remote-index request ID or size is invalid"));
        }
        write_frame_bounded(
            &mut self.send,
            &RemoteIndexMessage::Request(request.clone()),
            MAX_REMOTE_INDEX_FRAME_BYTES,
        )
        .await?;
        self.last_sent_request_id = request.request_id;
        Ok(())
    }

    /// Receives one bounded response and checks exact request correlation.
    pub async fn receive_response(
        &mut self,
        request_id: u64,
    ) -> Result<RemoteIndexResponse, TransportError> {
        let RemoteIndexMessage::Response(response) =
            read_frame_bounded(&mut self.receive, MAX_REMOTE_INDEX_FRAME_BYTES).await?
        else {
            return Err(frame_error("unexpected remote-index response frame"));
        };
        if response.request_id != request_id {
            self.connection
                .close(1_u32.into(), b"request correlation mismatch");
            return Err(frame_error("remote-index request correlation mismatch"));
        }
        if matches!(
            &response.outcome,
            RemoteIndexOutcome::Payload(body)
                if body.is_empty() || body.len() > MAX_REMOTE_INDEX_BODY_BYTES
        ) {
            return Err(frame_error("remote-index response exceeds its bound"));
        }
        Ok(response)
    }
}

/// Returns a connected, owner-authenticated remote-index client session.
pub async fn connect_remote_index(
    endpoint: &Endpoint,
    address: EndpointAddr,
    hello: RemoteIndexSessionHello,
) -> Result<RemoteIndexSession, TransportError> {
    let connection = endpoint
        .connect(address, REMOTE_INDEX_ALPN)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let (mut send, mut receive) = connection
        .open_bi()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    hello.capability.encode().map_err(frame_error)?;
    write_frame_bounded(
        &mut send,
        &RemoteIndexMessage::Hello(hello.clone()),
        MAX_REMOTE_INDEX_AUTH_BYTES,
    )
    .await?;
    match read_frame_bounded(&mut receive, MAX_REMOTE_INDEX_AUTH_BYTES).await? {
        RemoteIndexMessage::Accepted {
            grant_id,
            session_id,
            channel,
        } if grant_id == hello.capability.grant_id()
            && session_id == hello.session_id
            && channel == hello.channel =>
        {
            Ok(RemoteIndexSession {
                connection,
                send,
                receive,
                hello,
                local_identity: endpoint.id(),
                last_request_id: 0,
                last_responded_request_id: 0,
                last_sent_request_id: 0,
            })
        }
        RemoteIndexMessage::Rejected(reason) => {
            let deadline = tokio::time::Instant::now() + REMOTE_INDEX_SESSION_TIMEOUT;
            let _ = tokio::time::timeout_at(
                deadline,
                finish_rejected_exchange(&mut send, &mut receive),
            )
            .await;
            connection.close(1_u32.into(), b"remote-index admission denied");
            Err(frame_error(format!(
                "remote-index admission denied: {reason:?}"
            )))
        }
        _ => {
            connection.close(1_u32.into(), b"remote-index handshake mismatch");
            Err(frame_error("remote-index handshake mismatch"))
        }
    }
}

/// Admits the first framed hello on an already selected remote-index ALPN.
pub async fn accept_remote_index(
    connection: Connection,
    local: EndpointId,
) -> Result<RemoteIndexSession, TransportError> {
    if connection.alpn() != REMOTE_INDEX_ALPN {
        connection.close(1_u32.into(), b"remote-index ALPN mismatch");
        return Err(frame_error("remote-index ALPN mismatch"));
    }
    let (mut send, mut receive) =
        tokio::time::timeout(REMOTE_INDEX_SESSION_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| frame_error("remote-index stream acceptance timed out"))?
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let hello = match tokio::time::timeout(
        REMOTE_INDEX_SESSION_TIMEOUT,
        read_frame_bounded(&mut receive, MAX_REMOTE_INDEX_AUTH_BYTES),
    )
    .await
    {
        Ok(Ok(RemoteIndexMessage::Hello(hello))) => hello,
        Ok(Ok(_)) => {
            connection.close(1_u32.into(), b"remote-index hello required");
            return Err(frame_error("remote-index hello required"));
        }
        Ok(Err(error)) => {
            connection.close(1_u32.into(), b"invalid remote-index hello");
            return Err(error);
        }
        Err(_) => {
            connection.close(1_u32.into(), b"remote-index hello timed out");
            return Err(frame_error("remote-index hello timed out"));
        }
    };
    hello.capability.encode().map_err(frame_error)?;
    let peer = connection.remote_id();
    if hello.verify(local, peer, system_now_unix_ms()?).is_err() {
        let deadline = tokio::time::Instant::now() + REMOTE_INDEX_SESSION_TIMEOUT;
        let _ = tokio::time::timeout_at(deadline, async {
            write_frame_bounded(
                &mut send,
                &RemoteIndexMessage::Rejected(RemoteIndexReject::StaleCapability),
                MAX_REMOTE_INDEX_AUTH_BYTES,
            )
            .await?;
            finish_rejected_exchange(&mut send, &mut receive).await
        })
        .await;
        connection.close(1_u32.into(), b"remote-index grant rejected");
        return Err(frame_error("remote-index capability rejected"));
    }
    write_frame_bounded(
        &mut send,
        &RemoteIndexMessage::Accepted {
            grant_id: hello.capability.grant_id(),
            session_id: hello.session_id,
            channel: hello.channel,
        },
        MAX_REMOTE_INDEX_AUTH_BYTES,
    )
    .await?;
    Ok(RemoteIndexSession {
        connection,
        send,
        receive,
        hello,
        local_identity: local,
        last_request_id: 0,
        last_responded_request_id: 0,
        last_sent_request_id: 0,
    })
}

/// Half-closes both sides of a rejected handshake so the rejection frame is
/// delivered before the connection closes and the receiver can acknowledge
/// that it consumed the final frame.
async fn finish_rejected_exchange(
    send: &mut SendStream,
    receive: &mut RecvStream,
) -> Result<(), TransportError> {
    send.finish()
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let mut trailing = [0_u8; 1];
    match receive
        .read(&mut trailing)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?
    {
        None => Ok(()),
        Some(_) => Err(frame_error(
            "remote-index rejection acknowledgement had trailing data",
        )),
    }?;
    match send.stopped().await {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(frame_error(
            "remote-index rejection was not fully delivered",
        )),
        Err(error) => Err(TransportError::Iroh(error.to_string())),
    }
}

/// Opaque authenticated peer identity for one accepted remote-index session.
///
/// Its fields are private so callers cannot turn a claimed endpoint ID into
/// producer authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteIndexAuthenticatedPeer {
    peer: EndpointId,
    client: EndpointId,
    channel: RemoteIndexChannel,
    grant_id: [u8; 16],
}

impl RemoteIndexAuthenticatedPeer {
    /// Iroh identity authenticated by the accepted session.
    #[must_use]
    pub const fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Local Iroh identity used by the authenticated client session.
    #[must_use]
    pub const fn client(&self) -> EndpointId {
        self.client
    }

    /// Session channel authenticated by the accepted handshake.
    #[must_use]
    pub const fn channel(&self) -> RemoteIndexChannel {
        self.channel
    }

    /// Grant identity accepted by the session handshake.
    #[must_use]
    pub const fn grant_id(&self) -> [u8; 16] {
        self.grant_id
    }
}

/// Opaque copy of the owner-signed ProductQuery grant accepted by a session.
#[derive(Clone, Debug)]
pub struct RemoteIndexProductCapabilityReceipt {
    capability: RemoteIndexCapability,
    view_root: [u8; 32],
}

impl RemoteIndexProductCapabilityReceipt {
    /// Iroh identity whose signature appears on the accepted grant.
    #[must_use]
    pub const fn server(&self) -> EndpointId {
        self.capability.claims.server
    }

    /// Iroh client identity named by the accepted grant.
    #[must_use]
    pub const fn client(&self) -> EndpointId {
        self.capability.claims.client
    }

    /// Grant identity signed by the owner.
    #[must_use]
    pub const fn grant_id(&self) -> [u8; 16] {
        self.capability.claims.grant_id
    }

    /// Exact product view root signed into the accepted grant.
    #[must_use]
    pub const fn view_root(&self) -> [u8; 32] {
        self.view_root
    }
}

/// Errors while loading or admitting remote index capabilities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RemoteIndexCapabilityError {
    /// Capability shape, budget, expiry, or canonical encoding was rejected.
    Invalid(&'static str),
    /// Signature did not match the configured owner endpoint identity.
    Signature,
    /// Authenticated Iroh peer differs from the named client.
    PeerMismatch,
    /// Capability does not include the requested read channel.
    ChannelDenied,
}

impl std::fmt::Display for RemoteIndexCapabilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(field) => write!(formatter, "invalid remote index capability: {field}"),
            Self::Signature => formatter.write_str("remote index capability signature failed"),
            Self::PeerMismatch => formatter.write_str("remote index client identity mismatch"),
            Self::ChannelDenied => formatter.write_str("remote index channel is not authorized"),
        }
    }
}

impl std::error::Error for RemoteIndexCapabilityError {}

fn validate_claims(
    claims: &RemoteIndexCapabilityClaims,
    server: EndpointId,
    now_ms: u64,
) -> Result<(), RemoteIndexCapabilityError> {
    if claims.version != 2
        || claims.server != server
        || claims.client == server
        || claims.grant_id == [0; 16]
        || claims.issued_at_unix_ms > now_ms
        || claims.expires_at_unix_ms <= now_ms
        || claims.expires_at_unix_ms <= claims.issued_at_unix_ms
        || claims
            .expires_at_unix_ms
            .saturating_sub(claims.issued_at_unix_ms)
            > MAX_REMOTE_INDEX_GRANT_LIFETIME_MS
        || claims.request_budget == 0
        || claims.request_budget > MAX_REMOTE_INDEX_REQUESTS
        || claims.byte_budget == 0
        || claims.byte_budget > MAX_REMOTE_INDEX_BYTES
        || claims.permissions.is_empty()
        || claims.permissions.len() > 2
        || claims.permissions.windows(2).any(|pair| pair[0] >= pair[1])
        || claims.product.is_some()
            != claims
                .permissions
                .contains(&RemoteIndexPermission::ProductRead)
        || claims.semantic.is_some()
            != claims
                .permissions
                .contains(&RemoteIndexPermission::SemanticHydration)
        || (claims.product.is_none() && claims.semantic.is_none())
    {
        return Err(RemoteIndexCapabilityError::Invalid("claims"));
    }
    if let Some(product) = &claims.product
        && (product.view_root == [0; 32]
            || product.operations.is_empty()
            || product.operations.len() > 8
            || product.operations.windows(2).any(|pair| pair[0] >= pair[1])
            || product
                .operations
                .contains(&RemoteIndexQueryOperation::IndexSearch)
                != product.index_search_snapshot.is_some()
            || product.index_search_snapshot == Some([0; 32]))
    {
        return Err(RemoteIndexCapabilityError::Invalid("product scope"));
    }
    if let Some(semantic) = &claims.semantic
        && (semantic.package.is_empty()
            || semantic.package.len() > MAX_REMOTE_INDEX_TARGET_BYTES
            || semantic.coordinate.is_empty()
            || semantic.coordinate.len() > MAX_REMOTE_INDEX_TARGET_BYTES
            || semantic.profile == [0; 2]
            || semantic.namespace == [0; 16]
            || semantic.source_coordinate == [0; 32]
            || semantic.selection_revision == 0
            || semantic.selected_root == [0; 32]
            || semantic.closure_id == [0; 32]
            || semantic.catalog_root == [0; 32])
    {
        return Err(RemoteIndexCapabilityError::Invalid("semantic scope"));
    }
    Ok(())
}

/// Makes a bounded direct address for one private owner endpoint.
pub fn remote_index_owner_address(peer: EndpointId, address: std::net::SocketAddr) -> EndpointAddr {
    EndpointAddr::new(peer).with_ip_addr(address)
}

/// Current owner wall clock used to issue a bounded read grant.
pub fn remote_index_now() -> Result<u64, TransportError> {
    system_now_unix_ms()
}

fn system_now_unix_ms() -> Result<u64, TransportError> {
    crate::now_unix_ms().map_err(TransportError::Io)
}

fn authenticated_product_coverage(
    peer: EndpointId,
    client: EndpointId,
    channel: RemoteIndexChannel,
    capability: &RemoteIndexCapability,
    now_ms: u64,
) -> Option<(
    RemoteIndexAuthenticatedPeer,
    RemoteIndexProductCapabilityReceipt,
)> {
    let product = capability.claims.product.as_ref()?;
    if channel != RemoteIndexChannel::ProductQuery
        || peer != capability.claims.server
        || client != capability.claims.client
        || capability.verify(peer, client, now_ms).is_err()
    {
        return None;
    }
    Some((
        RemoteIndexAuthenticatedPeer {
            peer,
            client,
            channel,
            grant_id: capability.grant_id(),
        },
        RemoteIndexProductCapabilityReceipt {
            capability: capability.clone(),
            view_root: product.view_root,
        },
    ))
}

/// Maximum duration a single remote-index connection may remain active.
pub const REMOTE_INDEX_SESSION_TIMEOUT: Duration = Duration::from_secs(30);

fn remote_response_can_be_sent(
    response_id: u64,
    last_request_id: u64,
    last_responded_request_id: u64,
) -> bool {
    response_id != 0
        && response_id == last_request_id
        && last_request_id != last_responded_request_id
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn product_capability(
        owner: &SecretKey,
        client: EndpointId,
        now_ms: u64,
    ) -> RemoteIndexCapability {
        RemoteIndexCapabilityIssuer::new(owner.clone())
            .issue(
                RemoteIndexCapabilityClaims {
                    version: 2,
                    server: owner.public(),
                    client,
                    grant_id: [9; 16],
                    issued_at_unix_ms: now_ms,
                    expires_at_unix_ms: now_ms + 60_000,
                    request_budget: 10,
                    byte_budget: 10_000,
                    permissions: vec![RemoteIndexPermission::ProductRead],
                    product: Some(RemoteIndexProductScope {
                        view_root: [7; 32],
                        operations: vec![RemoteIndexQueryOperation::Search],
                        index_search_snapshot: None,
                    }),
                    semantic: None,
                },
                now_ms,
            )
            .expect("signed product capability")
    }

    #[test]
    fn prepared_response_reports_the_exact_canonical_wire_frame_size() {
        let response = RemoteIndexResponse {
            request_id: 17,
            outcome: RemoteIndexOutcome::StaleProductRoot {
                expected: [3; 32],
                observed: [4; 32],
            },
        };
        let expected = postcard::to_allocvec(&RemoteIndexMessage::Response(response.clone()))
            .expect("canonical response encoding");
        let prepared = prepare_remote_index_response(response).expect("prepared response");
        assert_eq!(prepared.encoded.as_ref(), expected.as_slice());
        assert_eq!(prepared.wire_bytes(), expected.len() + 4);
        assert!(prepared.wire_bytes() > 64);
    }

    #[test]
    fn stale_product_source_is_a_bounded_typed_response() {
        let response = RemoteIndexResponse {
            request_id: 18,
            outcome: RemoteIndexOutcome::StaleProductSource {
                expected: [5; 32],
                observed: [6; 32],
            },
        };
        let canonical = postcard::to_allocvec(&RemoteIndexMessage::Response(response.clone()))
            .expect("canonical stale-source frame");
        let prepared = prepare_remote_index_response(response).expect("prepared response");
        assert_eq!(prepared.encoded.as_ref(), canonical.as_slice());
        assert_eq!(prepared.wire_bytes(), canonical.len() + 4);
    }

    #[test]
    fn product_coverage_proof_binds_the_authenticated_owner_and_exact_grant() {
        let now_ms = 10_000;
        let owner = SecretKey::generate();
        let client = SecretKey::generate();
        let capability = product_capability(&owner, client.public(), now_ms);
        let (peer, receipt) = authenticated_product_coverage(
            owner.public(),
            client.public(),
            RemoteIndexChannel::ProductQuery,
            &capability,
            now_ms + 1,
        )
        .expect("authenticated owner session and signed grant");

        assert_eq!(peer.peer(), owner.public());
        assert_eq!(peer.client(), client.public());
        assert_eq!(peer.grant_id(), receipt.grant_id());
        assert_eq!(receipt.server(), owner.public());
        assert_eq!(receipt.view_root(), [7; 32]);
        assert!(authenticated_product_coverage(
            SecretKey::generate().public(),
            client.public(),
            RemoteIndexChannel::ProductQuery,
            &capability,
            now_ms + 1,
        )
        .is_none());
        assert!(authenticated_product_coverage(
            owner.public(),
            SecretKey::generate().public(),
            RemoteIndexChannel::ProductQuery,
            &capability,
            now_ms + 1,
        )
        .is_none());
        assert!(authenticated_product_coverage(
            owner.public(),
            client.public(),
            RemoteIndexChannel::SemanticHydration,
            &capability,
            now_ms + 1,
        )
        .is_none());
    }

    #[test]
    fn response_id_must_match_one_outstanding_request() {
        assert!(!remote_response_can_be_sent(1, 0, 0));
        assert!(!remote_response_can_be_sent(2, 1, 0));
        assert!(remote_response_can_be_sent(1, 1, 0));
        assert!(!remote_response_can_be_sent(1, 1, 1));
        assert!(!remote_response_can_be_sent(2, 1, 1));
    }
}
