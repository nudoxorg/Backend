//! Portable queries reopen existing command proofs against a fresh owner.
//! Imported certificate bytes establish canonical preimages, never authority.

use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::io::Write;

/// Token bound leaves room for rows in the existing presentation budget.
pub(super) const MAX_PORTABLE_QUERY_TOKEN_BYTES: usize = 32 * 1024;

fn query_parts(
    command: &Command,
) -> Option<(&str, &str, QueryLimit, Option<backend_library::Cursor>)> {
    match command {
        Command::Search(query) => Some(("Search", query.text(), query.limit(), query.cursor())),
        Command::SearchPublishedNames(request) => {
            let query = request.query();
            Some(("Search", query.text(), query.limit(), query.cursor()))
        }
        Command::Name(query) => Some(("Name", query.text(), query.limit(), query.cursor())),
        _ => None,
    }
}

// A canonical graph coordinate must survive the hop so the execution owner
// can resolve its exact text-derived address to a compiler-selected row.
// Selected addresses retain their opaque selector commitment and optional
// coordinate assertion; the owner checks that label against current membership.
fn graph_address_claim(command: &Command, claim: &WireClaim) -> bool {
    if let Command::OutlinePage { package, .. } = command {
        return matches!(claim, WireClaim::Key { schema: WireSchema::Package, id, .. }
            if id == &encode_id(package.as_bytes()));
    }
    let Command::GraphPage { symbol, .. } = command else {
        return false;
    };
    match claim {
        WireClaim::Key {
            schema: WireSchema::Symbol,
            id,
            ..
        } => symbol.is_selected() || id == &encode_id(&symbol.claimed_bytes()),
        WireClaim::KeyCommitment {
            schema: WireSchema::Symbol,
            id,
        } if symbol.is_selected() => id == &encode_id(&symbol.claimed_bytes()),
        _ => false,
    }
}

fn scope(certificate: &WireCertificate) -> Result<&WireClaim, ClientError> {
    let mut claims = certificate
        .claims
        .iter()
        .filter(|claim| matches!(claim, WireClaim::Coverage { .. }));
    let claim = claims
        .next()
        .ok_or_else(|| ClientError::Protocol("query proof omitted issuing scope".to_owned()))?;
    if claims.next().is_some() {
        return Err(ClientError::Protocol(
            "query proof duplicated issuing scope".to_owned(),
        ));
    }
    Ok(claim)
}

fn cursor_claim_matches(claim: &WireClaim, cursor: backend_library::Cursor) -> bool {
    let WireClaim::Cursor {
        recipe,
        version,
        branch,
        log,
        schema,
        root,
        sequence,
    } = claim
    else {
        return false;
    };
    recipe == &encode_id(cursor.recipe().as_bytes())
        && version == &encode_id(cursor.version().as_bytes())
        && branch == &encode_id(cursor.branch().as_bytes())
        && log == &encode_id(cursor.log().as_bytes())
        && *schema == cursor.schema()
        && root == &encode_id(cursor.root().as_bytes())
        && *sequence == cursor.sequence()
}

pub(super) fn basis(command: &Command) -> Option<backend_library::ViewRevision> {
    match command {
        Command::Search(query) => Some(query.basis()),
        Command::SearchPublishedNames(query) => Some(query.query().basis()),
        Command::Name(query) => Some(query.basis()),
        Command::GraphQuery(query) => Some(query.page().basis()),
        Command::GraphPage { page, .. }
        | Command::OutlinePage { page, .. }
        | Command::MembersPage { page, .. } => Some(page.basis()),
        _ => None,
    }
}

fn continuation_cursor(command: &Command) -> Option<backend_library::Cursor> {
    query_parts(command)
        .and_then(|(_, _, _, cursor)| cursor)
        .or_else(|| match command {
            Command::GraphQuery(query) => query.page().continuation().map(|page| page.cursor()),
            Command::GraphPage { page, .. } | Command::OutlinePage { page, .. } => {
                page.continuation().map(PageContinuation::cursor)
            }
            _ => None,
        })
}

// Retain the closed candidate-shape check as an eligibility guard, but export
// the full root preimage until the library can carry a deferred root without
// promoting its untrusted digest to `StateRoot`.
fn full_root_catalog_continuation_shape(request: &CommandDto) -> bool {
    let Some(certificate) = request.certificate() else {
        return false;
    };
    if matches!(&request.command, Command::GraphQuery(_)) {
        // GraphQuery has a separately owner-bound decoder below. Keep that
        // established path until its complete claim grammar is handled here.
        return false;
    }
    let Some(cursor) = continuation_cursor(&request.command) else {
        return false;
    };
    let Some(owner_root) = basis(&request.command).map(|basis| encode_id(basis.as_bytes())) else {
        return false;
    };
    let page_root = encode_id(cursor.root().as_bytes());
    if owner_root == page_root {
        return false;
    }
    if cursor.query_offset() == 0 {
        return false;
    }
    let mut owner_root_commitments = 0;
    let mut page_root_commitments = 0;
    let mut page_roots = 0;
    let mut coverages = 0;
    for claim in &certificate.claims {
        match claim {
            WireClaim::RootCommitment {
                schema: WireSchema::ViewRelation,
                id,
            } => {
                if id == &owner_root {
                    owner_root_commitments += 1;
                }
                if id == &page_root {
                    page_root_commitments += 1;
                }
                if id != &owner_root && id != &page_root {
                    return false;
                }
            }
            WireClaim::Coverage { .. } => coverages += 1,
            WireClaim::Root {
                schema: WireSchema::ViewRelation,
                id,
                ..
            } if id == &page_root => page_roots += 1,
            WireClaim::Root { .. } => return false,
            claim
                if claim_describes_cursor(claim, cursor)
                    || cursor_claim_matches(claim, cursor)
                    || graph_address_claim(&request.command, claim) => {}
            _ => return false,
        }
    }
    owner_root_commitments == 1
        && coverages == 1
        && page_roots == 1
        && page_root_commitments <= 1
        && scope(certificate).is_ok()
}

// Recognize only a closed rootless catalog envelope so it can be rejected
// explicitly; it never enters an owner decoder that would promote its digest.
fn raw_rootless_catalog_candidate(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let Some(envelope) = value.as_object() else {
        return false;
    };
    if envelope.len() != 4
        || !envelope.contains_key("version")
        || !envelope.contains_key("request_id")
        || !envelope.contains_key("command")
        || !envelope.contains_key("certificate")
        || envelope
            .get("request_id")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        return false;
    }
    let Some(command) = envelope
        .get("command")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if command.len() != 2 || !command.contains_key("kind") || !command.contains_key("data") {
        return false;
    }
    let Some(kind) = command.get("kind").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !matches!(
        kind,
        "search" | "search_published_names" | "name" | "graph_page" | "outline_page"
    ) {
        return false;
    }
    let Some(data) = command.get("data").and_then(serde_json::Value::as_object) else {
        return false;
    };
    let continued = match kind {
        "graph_page" | "outline_page" => data
            .get("page")
            .and_then(serde_json::Value::as_object)
            .and_then(|page| page.get("continuation"))
            .is_some_and(|value| !value.is_null()),
        _ => data.get("cursor").is_some_and(|value| !value.is_null()),
    };
    let Some(claims) = envelope
        .get("certificate")
        .and_then(|certificate| certificate.get("claims"))
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    continued
        && claims.iter().any(|claim| {
            claim.get("kind").and_then(serde_json::Value::as_str) == Some("root_commitment")
                && claim
                    .get("data")
                    .and_then(|data| data.get("schema"))
                    .and_then(serde_json::Value::as_str)
                    == Some("view_relation")
        })
        && claims
            .iter()
            .all(|claim| claim.get("kind").and_then(serde_json::Value::as_str) != Some("root"))
}

fn full_root_catalog_continuation(request: &CommandDto) -> Option<CommandDto> {
    if !full_root_catalog_continuation_shape(request) {
        return None;
    }
    Some(request.clone())
}

// Measure the existing DTO serializer without allocating an oversized body.
struct ProofBudget {
    bytes: usize,
    refused: bool,
}
impl Write for ProofBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > backend_library::MAX_COMMAND_BODY.saturating_sub(self.bytes) {
            self.refused = true;
            return Err(std::io::Error::other("query proof exceeds export budget"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Session {
    /// Whether this admitted cursor carries a portable query command.
    #[must_use]
    pub fn has_portable_query_continuation(&self, continuation: PageContinuation) -> bool {
        self.continuations
            .get(&continuation.cursor())
            .is_some_and(|retained| retained.next_request.is_some())
    }
    /// Exports the exact next command admitted from a bounded query page.
    /// No session map or file must survive in the receiving process.
    ///
    /// # Errors
    /// Refuses unknown continuations or proof exports exceeding the token bound.
    pub fn encode_query_continuation(
        &self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        let request = self
            .continuations
            .get(&continuation.cursor())
            .and_then(|retained| retained.next_request.as_ref())
            .ok_or_else(|| {
                ClientError::Protocol(
                    "query continuation was not issued by this session".to_owned(),
                )
            })?;
        let mut budget = ProofBudget {
            bytes: 0,
            refused: false,
        };
        if let Err(error) = serde_json::to_writer(&mut budget, request) {
            return Err(if budget.refused {
                ClientError::Transport(ReplicationError::MessageTooLarge)
            } else {
                ClientError::Protocol(error.to_string())
            });
        }
        // Keep the existing strict encoder admission before changing transport
        // spelling. Binary packing never supplies authority of its own.
        backend_library::encode_command_body(request).map_err(ClientError::Protocol)?;
        let full_root = full_root_catalog_continuation(request).unwrap_or_else(|| request.clone());
        let body = backend_library::encode_binary_certificate_command(&full_root)
            .map_err(ClientError::Protocol)?;
        encode_portable_proof(&body, "pc4-")
    }

    pub(super) fn retain_portable_query(
        &mut self,
        command: Command,
        owner_certificate: &WireCertificate,
        reply: &ReplyDto,
    ) -> Result<(), ClientError> {
        if let (Command::GraphQuery(query), CommandReply::GraphQueryPage(page)) =
            (&command, &reply.reply)
        {
            let observed = reply
                .certificate()
                .ok_or_else(|| ClientError::Protocol("query page omitted proof".to_owned()))?;
            if scope(owner_certificate)? != scope(observed)? {
                return Err(ClientError::StaleCursor);
            }
            let PageTerminal::More(next) = page.terminal else {
                return Ok(());
            };
            // A portable graph request carries the owner claim unchanged;
            // its projection is reconstructed from the canonical arguments.
            let proof = owner_certificate.clone();
            let request = CommandDto::new(
                1,
                Command::GraphQuery(query.clone().with_continuation(next)),
            )
            .with_certificate(proof);
            if let Some(retained) = self.continuations.get_mut(&next.cursor()) {
                retained.next_request = Some(request);
            }
            return Ok(());
        }
        let next = match &reply.reply {
            CommandReply::Search(page) | CommandReply::Names(page) => page.next,
            CommandReply::PublishedNameSearch(page) => page.snapshot.next,
            CommandReply::ProjectionPage(page)
                if matches!(
                    &command,
                    Command::GraphPage { .. } | Command::OutlinePage { .. }
                ) =>
            {
                page.snapshot.next
            }
            _ => None,
        };
        if matches!(
            &reply.reply,
            CommandReply::Search(_)
                | CommandReply::PublishedNameSearch(_)
                | CommandReply::Names(_)
                | CommandReply::ProjectionPage(_)
        ) {
            let observed = reply
                .certificate()
                .ok_or_else(|| ClientError::Protocol("query page omitted proof".to_owned()))?;
            if scope(owner_certificate)? != scope(observed)? {
                return Err(ClientError::StaleCursor);
            }
        }
        let Some(cursor) = next else {
            return Ok(());
        };
        let certificate = reply
            .certificate()
            .ok_or_else(|| ClientError::Protocol("query page omitted proof".to_owned()))?;
        let next_command = match command {
            Command::Search(query)
                if matches!(&reply.reply, CommandReply::PublishedNameSearch(_)) =>
            {
                Command::SearchPublishedNames(
                    backend_library::PublishedNameSearchQuery::admit(query)
                        .map_err(|_| {
                            ClientError::Protocol(
                                "published-name reply changed query grammar".to_owned(),
                            )
                        })?
                        .with_cursor(cursor),
                )
            }
            Command::Search(query) => Command::Search(query.with_cursor(cursor)),
            Command::SearchPublishedNames(query) => {
                Command::SearchPublishedNames(query.with_cursor(cursor))
            }
            Command::Name(query) => Command::Name(query.with_cursor(cursor)),
            Command::GraphPage { symbol, page } => Command::GraphPage {
                symbol,
                page: page.with_continuation(PageContinuation::from_cursor(cursor)),
            },
            Command::OutlinePage { package, page } => Command::OutlinePage {
                package,
                page: page.with_continuation(PageContinuation::from_cursor(cursor)),
            },
            _ => return Ok(()),
        };
        // Keep only the owner basis commitment, issuing scope, and canonical
        // predecessor claims used by the existing standalone command decoder.
        let mut proof = WireCertificate::new().with_claim(scope(owner_certificate)?.clone());
        let owner_root = basis(&next_command)
            .ok_or_else(|| ClientError::Protocol("query basis omitted".to_owned()))?;
        for claim in &owner_certificate.claims {
            if graph_address_claim(&next_command, claim) {
                proof = proof.with_claim_once(claim.clone());
            }
            if matches!(claim, WireClaim::RootCommitment { schema: WireSchema::ViewRelation, id } if id == &encode_id(owner_root.as_bytes()))
            {
                proof = proof.with_claim_once(claim.clone());
            }
        }
        for claim in &certificate.claims {
            if claim_describes_cursor(claim, cursor) {
                proof = proof.with_claim_once(claim.clone());
            }
        }
        let request = CommandDto::new(1, next_command).with_certificate(proof);
        if let Some(retained) = self.continuations.get_mut(&cursor) {
            retained.next_request = Some(request);
        }
        Ok(())
    }

    pub(super) fn decode_portable_query(
        &mut self,
        token: &str,
    ) -> Result<PageContinuation, ClientError> {
        self.prepared_query = None;
        if token.len() > MAX_PORTABLE_QUERY_TOKEN_BYTES {
            return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
        }
        let body = decode_portable_body(token)?;
        // The existing strict command codec rehashes every cursor preimage.
        // Its result is still an imported request, not admitted owner state.
        let imported = match backend_library::decode_command_body(&body) {
            Ok(imported) => imported,
            Err(error) => {
                // Inspect only a bounded sequence; the strict owner-aware codec
                // then compares every identity against fresh, typed authority.
                let envelope: serde_json::Value = serde_json::from_slice(&body)
                    .map_err(|error| ClientError::Protocol(error.to_string()))?;
                // Ordinary proofs must pass their standalone strict decoder
                // before any RPC. The graph-query continuation path uses an
                // already admitted owner cursor and retains its typed claim
                // checks below.
                if envelope
                    .get("command")
                    .and_then(|command| command.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    != Some("graph_query")
                {
                    if raw_rootless_catalog_candidate(&body) {
                        return Err(ClientError::Protocol(
                            "catalog continuation requires the canonical page-root preimage"
                                .to_owned(),
                        ));
                    }
                    return Err(ClientError::Protocol(error));
                } else {
                    let certificate: WireCertificate =
                        serde_json::from_value(envelope.get("certificate").cloned().ok_or_else(
                            || ClientError::Protocol("query token omitted certificate".to_owned()),
                        )?)
                        .map_err(|error| ClientError::Protocol(error.to_string()))?;
                    let mut sequences = certificate.claims.iter().filter_map(|claim| {
                        if let WireClaim::Cursor { sequence, .. } = claim {
                            Some(*sequence)
                        } else {
                            None
                        }
                    });
                    let sequence = sequences.next().ok_or_else(|| {
                        ClientError::Protocol("query token omitted owner cursor".to_owned())
                    })?;
                    let revision = self.revision()?;
                    let owner = revision.cursor();
                    if sequences.next().is_some() || sequence > owner.sequence() {
                        return Err(ClientError::StaleCursor);
                    }
                    if scope(&certificate)? != scope(&revision.certificate)? {
                        return Err(ClientError::StaleCursor);
                    }
                    let retained_owner = backend_library::Cursor::for_view(
                        owner.recipe(),
                        owner.version(),
                        backend_library::Frontier::new(
                            owner.branch(),
                            owner.log(),
                            owner.schema(),
                            owner.root(),
                            sequence,
                        ),
                    );
                    let imported =
                        backend_library::decode_command_body_for_owner(&body, retained_owner)
                            .map_err(ClientError::Protocol)?;
                    let Command::GraphQuery(query) = &imported.command else {
                        return Err(ClientError::Protocol(
                            "token is not a graph query".to_owned(),
                        ));
                    };
                    let continuation = query.page().continuation().ok_or_else(|| {
                        ClientError::Protocol("query token omitted predecessor".to_owned())
                    })?;
                    if imported.request_id != 1
                        || query.control() != backend_library::GraphQueryControl::Continue
                    {
                        return Err(ClientError::Protocol(
                            "query token has invalid continuation metadata".to_owned(),
                        ));
                    }
                    query
                        .start_offset(owner)
                        .map_err(|error| ClientError::Protocol(error.to_string()))?;
                    let fresh = revision.certificate.with_claim_once(WireClaim::KeyBytes {
                        schema: WireSchema::ViewRecipe,
                        id: encode_id(query.recipe().as_bytes()),
                        value: query.recipe_preimage(),
                    });
                    self.prepared_query =
                        Some(CommandDto::new(1, imported.command).with_certificate(fresh));
                    return Ok(continuation);
                }
            }
        };
        self.admit_query_continuation(imported, None)
    }

    /// Re-admits the exact next command, whether it arrived in a portable
    /// token or was retained from an admitted page on an earlier connection.
    fn admit_query_continuation(
        &mut self,
        imported: CommandDto,
        remaining: Option<Duration>,
    ) -> Result<PageContinuation, ClientError> {
        let started = std::time::Instant::now();
        self.check_request_context()?;
        let cursor = match &imported.command {
            Command::GraphPage { page, .. } | Command::OutlinePage { page, .. } => {
                page.continuation().map(PageContinuation::cursor)
            }
            command => query_parts(command).and_then(|(_, _, _, cursor)| cursor),
        }
        .ok_or_else(|| ClientError::Protocol("token is not a continued query".to_owned()))?;
        if imported.request_id != 1 || cursor.query_offset() == 0 {
            return Err(ClientError::Protocol(
                "query token has invalid continuation metadata".to_owned(),
            ));
        }
        let revision = match remaining {
            Some(remaining) => {
                let remaining = remaining.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(ClientError::RemoteDeadlineExceeded);
                }
                self.revision_with_budget(remaining)?
            }
            None => self.revision()?,
        };
        let owner = revision.cursor();
        let certificate = imported
            .certificate()
            .ok_or_else(|| ClientError::Protocol("query token omitted certificate".to_owned()))?;
        if !basis(&imported.command).is_some_and(|basis| basis.matches(revision.root))
            || cursor.branch() != owner.branch()
            || cursor.log() != owner.log()
            || cursor.schema() != owner.schema()
            || cursor.sequence() > owner.sequence()
            || scope(certificate)? != scope(&revision.certificate)?
        {
            return Err(ClientError::StaleCursor);
        }
        let recipe = match &imported.command {
            Command::Search(query) => {
                backend_library::QueryPageRecipe::search_predecessor(revision.root, query)
                    .ok_or_else(|| {
                        ClientError::Protocol("query predecessor changed window credit".to_owned())
                    })?
            }
            Command::SearchPublishedNames(query) => {
                backend_library::QueryPageRecipe::published_names_predecessor(revision.root, query)
                    .ok_or_else(|| {
                        ClientError::Protocol(
                            "published-name predecessor changed window credit".to_owned(),
                        )
                    })?
            }
            Command::Name(query) => {
                backend_library::QueryPageRecipe::names_predecessor(revision.root, query)
                    .ok_or_else(|| {
                        ClientError::Protocol("query predecessor changed window credit".to_owned())
                    })?
            }
            Command::GraphPage { symbol, page } => {
                backend_library::QueryPageRecipe::graph_predecessor(revision.root, *symbol, *page)
                    .ok_or_else(|| {
                    ClientError::Protocol("graph predecessor changed window credit".to_owned())
                })?
            }
            Command::OutlinePage { package, page } => {
                let encoded = encode_id(cursor.recipe().as_bytes());
                let mut preimages = certificate.claims.iter().filter_map(|claim| match claim {
                    WireClaim::KeyBytes {
                        schema: WireSchema::ViewRecipe,
                        id,
                        value,
                    } if id == &encoded => Some(value.as_ref()),
                    _ => None,
                });
                let preimage = preimages.next().ok_or_else(|| {
                    ClientError::Protocol("outline token omitted its order-bound recipe".to_owned())
                })?;
                if preimages.next().is_some() {
                    return Err(ClientError::Protocol(
                        "outline token duplicated its recipe".to_owned(),
                    ));
                }
                backend_library::QueryPageRecipe::outline_window_predecessor(
                    revision.root,
                    *package,
                    page.window(),
                    cursor,
                    preimage,
                )
                .ok_or_else(|| {
                    ClientError::Protocol(
                        "outline token changed its order-bound predecessor context".to_owned(),
                    )
                })?
            }
            _ => {
                return Err(ClientError::Protocol(
                    "query token changed family".to_owned(),
                ));
            }
        };
        if recipe.identity() != cursor.recipe() {
            return Err(ClientError::Protocol(
                "query token does not identify its query recipe".to_owned(),
            ));
        }
        // Fresh authority supplies the basis and scope. Imported claims supply
        // only already-rehashed canonical predecessor values. The producer
        // reconstructs that exact predecessor before returning a successor.
        let mut fresh = revision.certificate;
        for claim in &certificate.claims {
            if claim_describes_cursor(claim, cursor)
                || cursor_claim_matches(claim, cursor)
                || matches!(claim, WireClaim::RootCommitment { schema: WireSchema::ViewRelation, id } if id == &encode_id(cursor.root().as_bytes()))
                || graph_address_claim(&imported.command, claim)
            {
                fresh = fresh.with_claim_once(claim.clone());
            } else if !matches!(claim, WireClaim::Coverage { .. })
                && !matches!(claim, WireClaim::RootCommitment { schema: WireSchema::ViewRelation, id } if id == &encode_id(revision.root.as_bytes()))
            {
                return Err(ClientError::Protocol(
                    "query token contains unrelated proof claims".to_owned(),
                ));
            }
        }
        // A prepared command belongs to one connection. Keep the validated
        // input proof in the existing bounded owner state too, so replacing
        // that connection does not discard a cold imported continuation.
        let selected_coordinate =
            selected_coordinate_assertion(&imported.command, Some(certificate))?.and_then(
                |assertion| {
                    let source = backend_library::Basis {
                        root: revision.root,
                        object: revision.source,
                        branch: owner.branch(),
                        log: owner.log(),
                        schema: owner.schema(),
                    };
                    self.continuations
                        .get(&cursor)
                        .and_then(|retained| retained.selected_coordinate.as_ref())
                        .filter(|center| center.matches(&assertion, source))
                        .cloned()
                },
            );
        let retained = RetainedContinuation {
            certificate: certificate.clone(),
            owner,
            next_request: Some(imported.clone()),
            selected_coordinate,
        };
        self.prepared_query = Some(CommandDto::new(1, imported.command).with_certificate(fresh));
        self.continuations.clear();
        self.continuations.insert(cursor, retained);
        Ok(PageContinuation::from_cursor(cursor))
    }

    /// Only an exact retained next command can promote a cursor to adaptive
    /// delivery. A cursor alone is never authority for a new page recipe.
    pub(super) fn prepare_query_window(
        &mut self,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
        remaining: Option<Duration>,
    ) -> Result<(), ClientError> {
        let started = std::time::Instant::now();
        self.check_request_context()?;
        if remaining.is_some_and(|budget| budget.is_zero()) {
            return Err(ClientError::RemoteDeadlineExceeded);
        }
        if self.prepared_query.is_some() {
            return Ok(());
        }
        let Some(continuation) = continuation else {
            return Ok(());
        };
        let request = self
            .continuations
            .get(&continuation.cursor())
            .and_then(|retained| retained.next_request.clone());
        let Some(request) = request else {
            return if window.is_windowed() {
                Err(ClientError::Protocol(
                    "adaptive catalog continuation omitted its retained query proof".to_owned(),
                ))
            } else {
                // Keep the fixed-width API for old peers without an exported
                // next command. Its existing owner/cursor admission still runs.
                Ok(())
            };
        };
        let admitted = self.admit_query_continuation(
            request,
            remaining.map(|budget| budget.saturating_sub(started.elapsed())),
        )?;
        if admitted != continuation {
            self.prepared_query = None;
            return Err(ClientError::Protocol(
                "retained query proof changed its continuation".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) fn resume_prepared_outline(
        &mut self,
        package: backend_library::PackageKey,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let request = self
            .prepared_query
            .take()
            .ok_or_else(|| ClientError::Protocol("outline page was not prepared".to_owned()))?;
        let Command::OutlinePage {
            package: actual,
            page,
        } = &request.command
        else {
            return Err(ClientError::Protocol(
                "continuation changed query family".to_owned(),
            ));
        };
        if *actual != package
            || page.limit() != window.issued()
            || page.continuation() != continuation
        {
            return Err(ClientError::Protocol(
                "continuation changed outline package/credit/predecessor".to_owned(),
            ));
        }
        let certificate = request.certificate().cloned();
        let Command::OutlinePage { package, page } = request.command else {
            return Err(ClientError::Protocol(
                "outline delivery changed family".to_owned(),
            ));
        };
        let page = page.with_window(window).ok_or_else(|| {
            ClientError::Protocol("outline delivery changed issued ceiling".to_owned())
        })?;
        self.send_success(Command::OutlinePage { package, page }, certificate)
    }

    pub(super) fn resume_prepared_graph(
        &mut self,
        symbol: SymbolAddress,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.resume_prepared_graph_window(symbol, limit, continuation, None)
    }

    pub(super) fn resume_prepared_graph_window(
        &mut self,
        symbol: SymbolAddress,
        limit: u16,
        continuation: Option<PageContinuation>,
        window: Option<backend_library::QueryWindow>,
    ) -> Result<ReplyDto, ClientError> {
        let request = self
            .prepared_query
            .take()
            .ok_or_else(|| ClientError::Protocol("graph page was not prepared".to_owned()))?;
        let Command::GraphPage {
            symbol: actual,
            page,
        } = &request.command
        else {
            return Err(ClientError::Protocol(
                "continuation changed query family".to_owned(),
            ));
        };
        if *actual != symbol || page.limit().get() != limit || page.continuation() != continuation {
            return Err(ClientError::Protocol(
                "continuation changed graph address/credit/predecessor".to_owned(),
            ));
        }
        let certificate = request.certificate().cloned();
        let command = match (request.command, window) {
            (Command::GraphPage { symbol, page }, Some(window)) => Command::GraphPage {
                symbol,
                page: page.with_window(window).ok_or_else(|| {
                    ClientError::Protocol("graph delivery changed issued ceiling".to_owned())
                })?,
            },
            (command, None) => command,
            _ => {
                return Err(ClientError::Protocol(
                    "graph delivery request changed family".to_owned(),
                ));
            }
        };
        self.send_success(command, certificate)
    }

    pub(super) fn resume_prepared_query(
        &mut self,
        family: &str,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.resume_prepared_query_with_budget(family, text, limit, continuation, None)
    }

    pub(super) fn resume_prepared_query_with_budget(
        &mut self,
        family: &str,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
        remaining: Option<Duration>,
    ) -> Result<ReplyDto, ClientError> {
        self.resume_prepared_query_window(family, text, limit, continuation, remaining, None)
    }

    pub(super) fn resume_prepared_query_window(
        &mut self,
        family: &str,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
        remaining: Option<Duration>,
        window: Option<backend_library::QueryWindow>,
    ) -> Result<ReplyDto, ClientError> {
        let started = std::time::Instant::now();
        let request = self
            .prepared_query
            .take()
            .ok_or_else(|| ClientError::Protocol("query was not prepared".to_owned()))?;
        let (actual_family, actual_text, actual_limit, actual_cursor) =
            query_parts(&request.command)
                .ok_or_else(|| ClientError::Protocol("prepared query changed family".to_owned()))?;
        let manifest_absent = match &request.command {
            Command::Search(query) => query.read_manifest().is_none(),
            Command::SearchPublishedNames(query) => query.query().read_manifest().is_none(),
            Command::Name(query) => query.read_manifest().is_none(),
            _ => false,
        };
        if family != actual_family
            || text != actual_text
            || limit != actual_limit.get()
            || actual_cursor != continuation.map(PageContinuation::cursor)
            || !manifest_absent
        {
            return Err(ClientError::Protocol(
                "continuation does not match query family/text/credit/read manifest".to_owned(),
            ));
        }
        let certificate = request.certificate().cloned();
        let command = match (request.command, window) {
            (Command::Search(query), Some(window)) => {
                Command::Search(query.with_window(window).ok_or_else(|| {
                    ClientError::Protocol("search delivery changed issued ceiling".to_owned())
                })?)
            }
            (Command::SearchPublishedNames(query), Some(window)) => {
                Command::SearchPublishedNames(query.with_window(window).ok_or_else(|| {
                    ClientError::Protocol(
                        "published-name delivery changed issued ceiling".to_owned(),
                    )
                })?)
            }
            (Command::Name(query), Some(window)) => {
                Command::Name(query.with_window(window).ok_or_else(|| {
                    ClientError::Protocol("name delivery changed issued ceiling".to_owned())
                })?)
            }
            (command, None) => command,
            _ => {
                return Err(ClientError::Protocol(
                    "delivery request changed family".to_owned(),
                ));
            }
        };
        self.send_success_with_budget(
            command,
            certificate,
            remaining.map(|budget| budget.saturating_sub(started.elapsed())),
        )
    }
}

#[cfg(test)]
#[path = "query_continuation_tests.rs"]
mod tests;

// Compression changes the presentation, not the canonical command or its proofs.
// Both compressed input and expanded command have independent allocation bounds.
fn encode_portable_proof(body: &[u8], prefix: &str) -> Result<String, ClientError> {
    if body.len() > backend_library::MAX_COMMAND_BODY {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    let mut compressed = Vec::with_capacity((MAX_PORTABLE_QUERY_TOKEN_BYTES - 4) * 3 / 4 + 1);
    let mut encoder = flate2::Compress::new(flate2::Compression::default(), true);
    let status = encoder
        .compress_vec(body, &mut compressed, flate2::FlushCompress::Finish)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    if status != flate2::Status::StreamEnd {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    let encoded_len = compressed.len().saturating_mul(4).div_ceil(3);
    if encoded_len > MAX_PORTABLE_QUERY_TOKEN_BYTES - 4 {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(compressed)))
}

// Legacy vectors and adversarial JSON envelopes continue to exercise pc3.
#[cfg(test)]
fn encode_compact_proof(body: &[u8]) -> Result<String, ClientError> {
    encode_portable_proof(body, "pc3-")
}

fn decode_portable_body(token: &str) -> Result<Vec<u8>, ClientError> {
    if token.len() > MAX_PORTABLE_QUERY_TOKEN_BYTES {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    if let Some(encoded) = token.strip_prefix("pc2-") {
        return decode_hex(encoded)
            .ok_or_else(|| ClientError::Protocol("malformed query token".to_owned()));
    }
    let binary = token.starts_with("pc4-");
    let encoded = token
        .strip_prefix("pc4-")
        .or_else(|| token.strip_prefix("pc3-"))
        .ok_or_else(|| ClientError::Protocol("unknown query token version".to_owned()))?;
    let compressed = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ClientError::Protocol("malformed compressed query token".to_owned()))?;
    if compressed.is_empty() || URL_SAFE_NO_PAD.encode(&compressed) != encoded {
        return Err(ClientError::Protocol(
            "noncanonical compressed query token".to_owned(),
        ));
    }
    let mut body = Vec::new();
    let mut decoder = flate2::Decompress::new(true);
    let mut chunk = [0; 8192];
    loop {
        let input = decoder.total_in() as usize;
        let output = decoder.total_out();
        let status = decoder
            // Finish on the first call requires one output buffer large
            // enough for the entire stream; None preserves streaming state.
            .decompress(
                &compressed[input..],
                &mut chunk,
                flate2::FlushDecompress::None,
            )
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        let written = (decoder.total_out() - output) as usize;
        if written > backend_library::MAX_COMMAND_BODY.saturating_sub(body.len()) {
            return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
        }
        body.extend_from_slice(&chunk[..written]);
        if status == flate2::Status::StreamEnd {
            if decoder.total_in() != compressed.len() as u64 {
                return Err(ClientError::Protocol(
                    "query token contains trailing compressed bytes".to_owned(),
                ));
            }
            break;
        }
        if decoder.total_in() as usize == input && written == 0 {
            return Err(ClientError::Protocol(
                "query token is incomplete".to_owned(),
            ));
        }
    }
    if binary {
        backend_library::expand_binary_certificate_command(&body).map_err(ClientError::Protocol)
    } else {
        Ok(body)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod selected_operand_tests {
    use super::*;
    #[test]
    fn prepared_selected_graph_refuses_changed_or_omitted_coordinate_assertion() {
        let symbol = backend_library::SelectedSymbolId::new([0x51; 32]).expect("selected ID");
        let exact = DeclarationSelector::SelectedAt {
            symbol,
            coordinate: "/project::file.ts:1::item".to_owned(),
        };
        let certificate = declaration_certificate(WireCertificate::new(), &exact);
        assert!(declaration_coordinate_claims_match(
            Some(&certificate),
            &exact
        ));
        assert!(!declaration_coordinate_claims_match(
            Some(&certificate),
            &DeclarationSelector::Selected(symbol)
        ));
        assert!(!declaration_coordinate_claims_match(
            Some(&certificate),
            &DeclarationSelector::SelectedAt {
                symbol,
                coordinate: "/project::file.ts:2::item".to_owned()
            }
        ));
        assert!(!declaration_coordinate_claims_match(None, &exact));
        let plain = declaration_certificate(
            WireCertificate::new(),
            &DeclarationSelector::Selected(symbol),
        );
        assert!(declaration_coordinate_claims_match(
            Some(&plain),
            &DeclarationSelector::Selected(symbol)
        ));
        assert!(!declaration_coordinate_claims_match(Some(&plain), &exact));
    }

    #[test]
    fn graph_portable_claims_retain_selected_locator_and_exact_coordinate_assertion() {
        let symbol = backend_library::SelectedSymbolId::new([0x76; 32]).expect("selected ID");
        let selected = backend_library::DeclarationSelector::SelectedAt {
            symbol,
            coordinate: "/project::box.ts:1::item".to_owned(),
        };
        let command = Command::GraphPage {
            symbol: selected.address(),
            page: PageRequest::new(
                backend_library::view_state_root(&[]),
                QueryLimit::new(1).expect("credit"),
            ),
        };
        let certificate = declaration_certificate(WireCertificate::new(), &selected);
        assert_eq!(certificate.claims.len(), 2);
        assert!(
            certificate
                .claims
                .iter()
                .all(|claim| graph_address_claim(&command, claim))
        );
        let retained = certificate
            .claims
            .iter()
            .filter(|claim| graph_address_claim(&command, claim))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(retained.as_slice(), certificate.claims.as_ref());
        assert!(!graph_address_claim(
            &command,
            &WireClaim::KeyCommitment {
                schema: WireSchema::Symbol,
                id: encode_id(&[0x77; 32])
            }
        ));
        assert!(!graph_address_claim(
            &command,
            &WireClaim::Key {
                schema: WireSchema::Package,
                id: encode_id(package_key("/project").as_bytes()),
                value: "/project".to_owned()
            }
        ));
    }
}
