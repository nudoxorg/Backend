//! Shared, proof-admitting client for every local product surface.
//!
//! CLI, MCP, and desktop hosts differ only in presentation and lifecycle.
//! This crate owns their single command transport, error algebra, framing
//! bounds, and reply admission path.
#![forbid(unsafe_code)]

use backend_library::{
    AdmittedGraphQueryInput, Command, CommandDto, CommandFailure, CommandMutation, CommandReply,
    CoverageCapability, DiffRecord, DocumentQuery, GraphNeighborhoodQuery, GraphQueryPage,
    GraphQueryRequest, GraphValue, HealthReport, NameQuery, OutlineQuery, PackageReference,
    PageContinuation, PageRequest, PageTerminal, Query, QueryLimit, ReplyAdmissionError, ReplyDto,
    RequestAdmissionError, SemanticGenerationId, SemanticLanguageProfile, SemanticVersionRecord,
    SurfaceCommand, SurfaceReply, SymbolAddress, SymbolKey, ViewProjectionError, ViewRoot,
    ViewStateRoot, WireCertificate, WireClaim, WireSchema, encode_id, package_key, symbol_key,
};
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

#[cfg(any(unix, windows))]
impl Session {
    /// Connects one revision-aware session.
    ///
    /// # Errors
    /// Returns an error when the local endpoint is unavailable or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let endpoint = path.as_ref().to_path_buf();
        Ok(Self {
            transport: UnixCommandTransport::connect(&endpoint)?,
            endpoint,
            next_request_id: 1,
            continuations: BTreeMap::new(),
        })
    }

    /// Returns the endpoint this session was connected to.
    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
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
        let encoded = token
            .strip_prefix("pc1-")
            .ok_or_else(|| ClientError::Protocol("unknown continuation token schema".to_owned()))?;
        if encoded.len() != backend_library::CURSOR_QUERY_BYTES.saturating_mul(2) {
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
        let owner = self.revision()?.cursor();
        if cursor.query_offset() == 0 || !cursor.matches_owner(owner) {
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
        self.transport = UnixCommandTransport::connect(&self.endpoint)?;
        self.next_request_id = 1;
        Ok(())
    }

    /// Reads the current admitted product revision.
    ///
    /// # Errors
    /// Returns an error when the bounded revision reply fails transport or proof admission.
    pub fn revision(&mut self) -> Result<Revision, ClientError> {
        let reply = self.send_success(Command::Revision, None)?;
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
        Ok(Revision {
            root: receipt.root(),
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

    /// Hydrates a legacy peer's complete health view.
    ///
    /// # Errors
    /// Returns an error when the complete compatibility view fails admission.
    pub fn view(&mut self) -> Result<ViewRoot, ClientError> {
        let reply = self.send_success(Command::Health, None)?;
        let CommandReply::Health(view) = reply.reply else {
            return Err(ClientError::Protocol(
                "legacy health reply changed shape".to_owned(),
            ));
        };
        Ok(view)
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
        let package = package_key(coordinate);
        self.send_success(
            Command::Add { package },
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
        let revision = self.revision()?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        self.send_success(
            Command::Search(Query::new(text, revision.root, limit)),
            Some(revision.certificate),
        )
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
        let revision = self.revision()?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        let query = Query::new(text, revision.root, limit);
        let query = match continuation {
            Some(cursor) => query.with_cursor(cursor.cursor()),
            None => query,
        };
        self.send_success(
            Command::Search(query),
            Some(self.page_certificate(revision.certificate, continuation)),
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
        let revision = self.revision()?;
        let package = package_key(coordinate);
        let certificate = revision.certificate.with_claim_once(WireClaim::Key {
            schema: WireSchema::Package,
            id: encode_id(package.as_bytes()),
            value: coordinate.to_owned(),
        });
        let page = page_request(revision.root, limit, continuation)?;
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
        let revision = self.revision()?;
        let limit = QueryLimit::new(limit)
            .ok_or_else(|| ClientError::Protocol("query limit is outside its bound".to_owned()))?;
        let mut request = GraphQueryRequest::bind(input, revision.root, limit);
        if let Some(continuation) = continuation {
            request = request.with_continuation(continuation);
        }
        if cancel {
            request = request.cancelled();
        }
        let reply = self.send_success(
            Command::GraphQuery(request),
            Some(self.page_certificate(revision.certificate, continuation)),
        )?;
        let CommandReply::GraphQueryPage(page) = reply.reply else {
            return Err(ClientError::Protocol(
                "graph query reply changed shape".to_owned(),
            ));
        };
        Ok(page)
    }

    /// Executes one typed daemon-owned product command.
    ///
    /// # Errors
    /// Returns an error when request admission, transport, or reply-shape admission fails.
    pub fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let expected = command.id();
        let reply = self.send_success(Command::Surface(command), None)?;
        let CommandReply::Surface(reply) = reply.reply else {
            return Err(ClientError::Protocol(
                "surface reply changed shape".to_owned(),
            ));
        };
        reply
            .admit(expected)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(reply)
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
        match self.surface(SurfaceCommand::SemanticVersions { package })? {
            SurfaceReply::SemanticVersions(records) => Ok(records),
            _ => Err(ClientError::Protocol(
                "semantic versions reply changed shape".to_owned(),
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
        self.transport.request(request)
    }

    fn send_success(
        &mut self,
        command: Command,
        certificate: Option<WireCertificate>,
    ) -> Result<ReplyDto, ClientError> {
        let reply = require_command_success(self.send(command, certificate)?)?;
        self.remember_continuation(&reply);
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
                .claims
                .iter()
                .filter(|claim| claim_describes_cursor(claim, continuation.cursor()))
            {
                certificate = certificate.with_claim_once(claim.clone());
            }
        }
        with_page_continuation_claim(certificate, Some(continuation))
    }

    fn remember_continuation(&mut self, reply: &ReplyDto) {
        let terminal = match &reply.reply {
            CommandReply::ProjectionPage(page) => page.terminal,
            CommandReply::GraphQueryPage(page) => page.terminal,
            _ => return,
        };
        let PageTerminal::More(continuation) = terminal else {
            return;
        };
        let Some(certificate) = reply.certificate().cloned() else {
            return;
        };
        self.continuations.clear();
        self.continuations
            .insert(continuation.cursor(), certificate);
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
