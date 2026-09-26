//! Revision-aware local product session.
//!
//! Search, graph, outline, and package pages all admit against one owner
//! revision. Continuations stay owner-issued certificates, not socket state.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use backend_library::{
    AdmittedGraphQueryInput, Command, CommandDto, CommandReply, DiffRecord, DocumentQuery,
    GraphNeighborhoodQuery, GraphQueryPage, GraphQueryRequest, GraphValue, HealthReport, NameQuery,
    OutlineQuery, PackageReference, PageContinuation, PageTerminal, Query, QueryLimit, ReplyDto,
    SemanticGenerationId, SemanticLanguageProfile, SemanticVersionRecord, SurfaceCommand,
    SurfaceReply, SymbolAddress, SymbolKey, WireCertificate, WireClaim, WireSchema, encode_id,
    package_key, symbol_key,
};

use super::{
    ClientError, CommandTransport, Revision, Session, SessionContinuationState,
    UnixCommandTransport, claim_describes_cursor, decode_hex, health_from_reply, key_certificate,
    page_request, require_command_success, selected_symbol_certificate,
    with_page_continuation_claim,
};

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
