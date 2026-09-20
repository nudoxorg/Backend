//! Shared, proof-admitting client for every local product surface.
//!
//! CLI, MCP, and desktop hosts differ only in presentation and lifecycle.
//! This crate owns their single command transport, error algebra, framing
//! bounds, and reply admission path.
#![forbid(unsafe_code)]

use backend_library::{
    Command, CommandDto, CommandReply, CoverageCapability, DocumentQuery, GraphQuery, NameQuery,
    OutlineQuery, Query, QueryLimit, ReplyAdmissionError, ReplyDto, RequestAdmissionError,
    ViewProjectionError, ViewRoot, ViewStateRoot, WireCertificate, WireClaim, WireSchema,
    encode_id, package_key, symbol_key,
};
use backend_replication::{
    LocalControlError, LocalControlLimits, ReplicationError, read_frame, write_frame,
};
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
    /// A frame, DTO, or identity proof failed admission.
    Protocol(String),
    /// A bounded transport allocation was rejected.
    Transport(ReplicationError),
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
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(formatter, "local endpoint: {message}"),
            Self::Protocol(message) => write!(formatter, "protocol: {message}"),
            Self::Transport(error) => write!(formatter, "transport: {error}"),
            Self::IncoherentView => formatter.write_str("daemon returned an incoherent view"),
            Self::BasisMismatch { .. } => {
                formatter.write_str("daemon reply is based on a different revision")
            }
            Self::FreshnessMismatch => formatter.write_str("daemon reply did not prove freshness"),
            Self::RequestMismatch { expected, observed } => {
                write!(formatter, "request id {observed} does not match {expected}")
            }
            Self::CursorMismatch => formatter.write_str("daemon returned an invalid cursor"),
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
#[cfg(unix)]
pub struct UnixCommandTransport {
    stream: std::os::unix::net::UnixStream,
    peer: Option<backend_replication::AuthenticatedLocalPeer>,
}

#[cfg(unix)]
impl UnixCommandTransport {
    /// Connects and authenticates the local endpoint owner.
    ///
    /// # Errors
    /// Returns an error when the endpoint is invalid, unavailable, or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        let endpoint = backend_replication::UnixEndpointRef::new(path.as_ref())
            .map_err(|_| ClientError::Transport(ReplicationError::MessageTooLarge))?;
        let path = endpoint.as_path();
        let stream = std::os::unix::net::UnixStream::connect(path)
            .map_err(|error| ClientError::Io(error.to_string()))?;
        let peer = backend_replication::AuthenticatedLocalPeer::authenticate(&stream, path)
            .map_err(|error| {
                ClientError::Io(format!("local peer authentication failed: {error}"))
            })?;
        configure(&stream)?;
        Ok(Self {
            stream,
            peer: Some(peer),
        })
    }

    /// Wraps a connected stream for tests and embedded transports.
    #[must_use]
    pub fn from_stream(stream: std::os::unix::net::UnixStream) -> Self {
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

#[cfg(unix)]
impl CommandTransport for UnixCommandTransport {
    fn request(&mut self, request: CommandDto) -> Result<ReplyDto, ClientError> {
        let body = encode_request(&request)?;
        write_body(&mut self.stream, &body)?;
        let body = read_body(&mut self.stream)?;
        let reply = self.decode(&body)?;
        admit_reply(&request, reply)
    }
}

#[cfg(unix)]
impl CertifiedCommandTransport for UnixCommandTransport {
    fn request_with_certificate(
        &mut self,
        request: CommandDto,
        capability: Option<CoverageCapability>,
    ) -> Result<ReplyDto, ClientError> {
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
#[cfg(unix)]
pub struct Session {
    transport: UnixCommandTransport,
    next_request_id: u64,
}

/// One admitted health revision retained long enough to build a dependent
/// query request without copying the view.
#[cfg(unix)]
pub struct Revision {
    /// Current immutable product view root.
    pub root: ViewStateRoot,
    certificate: WireCertificate,
    cursor: backend_library::Cursor,
}

#[cfg(unix)]
impl Revision {
    /// Returns the exact owner cursor paired with this immutable root.
    #[must_use]
    pub const fn cursor(&self) -> backend_library::Cursor {
        self.cursor
    }
}

#[cfg(unix)]
impl Session {
    /// Connects one revision-aware session.
    ///
    /// # Errors
    /// Returns an error when the local endpoint is unavailable or cannot authenticate.
    pub fn connect(path: impl AsRef<Path>) -> Result<Self, ClientError> {
        Ok(Self {
            transport: UnixCommandTransport::connect(path)?,
            next_request_id: 1,
        })
    }

    /// Reads the current admitted product revision.
    ///
    /// # Errors
    /// Returns an error when the bounded revision reply fails transport or proof admission.
    pub fn revision(&mut self) -> Result<Revision, ClientError> {
        let reply = self.send(Command::Revision, None)?;
        let certificate = reply.certificate().cloned().ok_or_else(|| {
            ClientError::Protocol("revision reply omitted its certificate".to_owned())
        })?;
        let CommandReply::Revision(receipt) = reply.reply else {
            return Err(ClientError::Protocol(
                "revision reply changed shape".to_owned(),
            ));
        };
        Ok(Revision {
            root: receipt.root(),
            certificate,
            cursor: receipt.cursor(),
        })
    }

    /// Hydrates the complete current view for callers that explicitly need
    /// every row, such as an in-process graph query engine.
    ///
    /// # Errors
    /// Returns an error when the complete health snapshot cannot be admitted.
    pub fn view(&mut self) -> Result<ViewRoot, ClientError> {
        let reply = self.send(Command::Health, None)?;
        let CommandReply::Health(view) = reply.reply else {
            return Err(ClientError::Protocol(
                "health reply changed shape".to_owned(),
            ));
        };
        Ok(view)
    }

    /// Lists indexed projects.
    ///
    /// # Errors
    /// Returns an error when the request or reply fails admission.
    pub fn packages(&mut self) -> Result<ReplyDto, ClientError> {
        self.send(Command::Packages, None)
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
        self.send(
            Command::Search(Query::new(text, revision.root, limit)),
            Some(revision.certificate),
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
        self.send(
            Command::Name(NameQuery::new(text, revision.root, limit)),
            Some(revision.certificate),
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
        self.send(
            Command::Document(DocumentQuery::new(symbol, revision.root)),
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
        self.send(
            Command::Outline(OutlineQuery::new(package, revision.root)),
            Some(certificate),
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
        self.send(
            Command::Graph(GraphQuery::new(symbol, revision.root)),
            Some(certificate),
        )
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
        let reply = self.send(command, certificate)?;
        if let CommandReply::Error(message) = &reply.reply {
            return Err(ClientError::Protocol(message.clone()));
        }
        Ok(reply)
    }
}

fn key_certificate(schema: WireSchema, id: &[u8; 32], value: &str) -> WireCertificate {
    WireCertificate::new().with_claim(WireClaim::Key {
        schema,
        id: encode_id(id),
        value: value.to_owned(),
    })
}

#[cfg(unix)]
fn configure(stream: &std::os::unix::net::UnixStream) -> Result<(), ClientError> {
    let timeout = Some(Duration::from_secs(30));
    stream
        .set_read_timeout(timeout)
        .and_then(|()| stream.set_write_timeout(timeout))
        .map_err(|error| ClientError::Io(error.to_string()))
}

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
        LocalControlError::Io(kind) => {
            ClientError::Io(format!("local control I/O failed: {kind:?}"))
        }
        LocalControlError::Closed => ClientError::Io("local endpoint is closed".to_owned()),
        other => ClientError::Protocol(other.to_string()),
    }
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
        RequestAdmissionError::EmptyText => ClientError::Protocol(error.to_string()),
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
