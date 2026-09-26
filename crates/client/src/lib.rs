//! Shared, proof-admitting client for every local product surface.
//!
//! CLI, MCP, and desktop hosts differ only in presentation and lifecycle.
//! This crate owns their single command transport, error algebra, framing
//! bounds, and reply admission path.
#![forbid(unsafe_code)]

mod subscription;
#[cfg(any(unix, windows))]
mod subscription_local;
#[cfg(any(unix, windows))]
mod session;

pub use subscription::{
    CertifiedSubscriptionTransport, SubscriptionRequest, SubscriptionTransport,
    snapshot_page_from_bytes, snapshot_page_from_value,
};
#[cfg(any(unix, windows))]
pub use subscription_local::LocalSubscriptionTransport;

use backend_library::{
    CommandDto, CommandFailure, CommandMutation, CommandReply, CoverageCapability, HealthReport,
    PageContinuation, PageRequest, QueryLimit, ReplyAdmissionError, ReplyDto, RequestAdmissionError,
    SymbolKey, ViewProjectionError, ViewStateRoot, WireCertificate, WireClaim, WireSchema,
    encode_id,
};
#[cfg(test)]
use backend_library::{Command, package_key};
use backend_replication::{
    LocalControlError, LocalControlLimits, ReplicationError, read_frame, write_frame,
};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Maximum admitted local command/reply body.
pub const MAX_FRAME: usize = backend_replication::LOCAL_CONTROL_MAX_FRAME;
/// Maximum events admitted in one interactive subscription batch.
pub const MAX_EVENTS: usize = backend_library::MAX_SUBSCRIPTION_EVENTS;

/// One closed failure type shared by all command clients.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientError {
    /// The endpoint or stream could not be used.
    Io(String),
    /// The connection is gone: the peer closed it, reset it, or stopped
    /// answering on it.
    ///
    /// This is separate from [`ClientError::Io`] because it is not a fault of
    /// the request. The daemon closes a connection that has sent nothing for
    /// its read timeout, so a long-lived client sees this on its first call
    /// after an idle gap and can recover by connecting again.
    Disconnected(std::io::ErrorKind),
    /// A frame, DTO, or identity proof failed admission.
    Protocol(String),
    /// A bounded transport allocation was rejected.
    Transport(ReplicationError),
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
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "local endpoint: {message}"),
            Self::Disconnected(kind) => {
                write!(formatter, "local endpoint disconnected: {kind}")
            }
            Self::Protocol(message) => write!(formatter, "protocol: {message}"),
            Self::Transport(error) => write!(formatter, "transport: {error}"),
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
        }
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
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
}

#[cfg(any(unix, windows))]
impl UnixCommandTransport {
    /// Connects and authenticates the local endpoint owner.
    ///
    /// # Errors
    /// Returns an error when the endpoint is invalid, unavailable, or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let endpoint = backend_replication::UnixEndpointRef::new(path.as_ref())
            .map_err(|_| ClientError::Transport(ReplicationError::MessageTooLarge))?;
        let path = endpoint.as_path();
        let stream =
            backend_replication::LocalStream::connect(path).map_err(map_endpoint_connect_error)?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
            .map_err(map_peer_authentication_error)?;
        configure(&stream)?;
        Ok(Self {
            stream,
            peer: Some(peer),
        })
    }

    /// Wraps a connected stream for tests and embedded transports.
    #[must_use]
    pub fn from_stream(stream: backend_replication::LocalStream) -> Self {
        let _ = configure(&stream);
        Self { stream, peer: None }
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
        configure_request(&self.stream, request)?;
        let body = encode_request(request)?;
        write_body(&mut self.stream, &body)?;
        let body = read_body(&mut self.stream)?;
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
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        configure_request(&self.stream, &request)?;
        let body = encode_request(&request)?;
        write_body(&mut self.stream, &body)?;
        let body = read_body(&mut self.stream)?;
        let reply = self.decode(&body)?;
        admit_reply(&request, reply)
    }
}

#[cfg(any(unix, windows))]
impl CertifiedCommandTransport for UnixCommandTransport {
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError> {
        configure_request(&self.stream, &request)?;
        let body = encode_request(&request)?;
        write_body(&mut self.stream, &body)?;
        let body = read_body(&mut self.stream)?;
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
    transport: UnixCommandTransport,
    next_request_id: u64,
    continuations: BTreeMap<backend_library::Cursor, WireCertificate>,
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
    continuations: BTreeMap<backend_library::Cursor, WireCertificate>,
}

/// One admitted health revision retained long enough to build a dependent
/// query request without copying the view.
#[cfg(any(unix, windows))]
pub struct Revision {
    /// Current immutable product view root.
    pub root: ViewStateRoot,
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

#[cfg(any(unix, windows))]
fn configure_request(
    stream: &backend_replication::LocalStream,
    request: &CommandDto,
) -> Result<(), ClientError> {
    let timeout = match backend_library::command_spec(request.command.id()).mutation {
        CommandMutation::Write => CLIENT_MUTATION_TIMEOUT,
        CommandMutation::Read => CLIENT_REQUEST_TIMEOUT,
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
#[cfg(any(unix, windows))]
fn map_endpoint_connect_error(error: std::io::Error) -> ClientError {
    if is_disconnect(error.kind()) || error.kind() == std::io::ErrorKind::NotFound {
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
            if is_disconnect(kind) || kind == std::io::ErrorKind::NotFound =>
        {
            disconnected(kind)
        }
        AuthenticationError::PeerAddress => disconnected(std::io::ErrorKind::ConnectionAborted),
        AuthenticationError::PeerCredentials(PeerCredentialError::Io(kind))
            if is_disconnect(kind) || kind == std::io::ErrorKind::NotFound =>
        {
            disconnected(kind)
        }
        other => ClientError::Io(format!("local peer authentication failed: {other}")),
    }
}

/// Bounded deadline for one read-only command, including health and discovery.
///
/// Every request re-arms this value, so a long mutation cannot make a later
/// health or query call wait on the mutation lease.
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

fn encode_request(request: &CommandDto) -> Result<Vec<u8>, ClientError> {
    admit_request(request)?;
    backend_library::encode_command_body(request).map_err(ClientError::Protocol)
}

fn write_body(writer: &mut impl Write, body: &[u8]) -> Result<(), ClientError> {
    write_frame(writer, body, limits()).map_err(map_frame)
}

fn read_body(reader: &mut impl Read) -> Result<Vec<u8>, ClientError> {
    read_frame(reader, limits()).map_err(map_frame)
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
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    let encoded =
        serde_json::to_vec(&reply).map_err(|error| ClientError::Protocol(error.to_string()))?;
    if encoded.len() > MAX_FRAME {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
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
        let _ = std::fs::remove_file(path);
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
