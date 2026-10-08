//! Portable queries reopen existing command proofs against a fresh owner.
//! Imported certificate bytes establish canonical preimages, never authority.

use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

/// Token bound leaves room for rows in the existing presentation budget.
pub(super) const MAX_PORTABLE_QUERY_TOKEN_BYTES: usize = 32 * 1024;

fn query_parts(
    command: &Command,
) -> Option<(&str, &str, QueryLimit, Option<backend_library::Cursor>)> {
    match command {
        Command::Search(query) => Some(("Search", query.text(), query.limit(), query.cursor())),
        Command::Name(query) => Some(("Name", query.text(), query.limit(), query.cursor())),
        _ => None,
    }
}

// A canonical graph coordinate must survive the hop so the execution owner
// can resolve its exact text-derived address to a compiler-selected row.
// Selected addresses retain only their original opaque selector commitment.
fn graph_address_claim(command: &Command, claim: &WireClaim) -> bool {
    let Command::GraphPage { symbol, .. } = command else {
        return false;
    };
    match claim {
        WireClaim::Key {
            schema: WireSchema::Symbol,
            id,
            ..
        } if !symbol.is_selected() => id == &encode_id(&symbol.claimed_bytes()),
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

fn basis(command: &Command) -> Option<backend_library::ViewRevision> {
    match command {
        Command::Search(query) => Some(query.basis()),
        Command::Name(query) => Some(query.basis()),
        Command::GraphPage { page, .. } => Some(page.basis()),
        _ => None,
    }
}

// Measure the existing DTO serializer without allocating an oversized body.
struct ProofBudget {
    bytes: usize,
    refused: bool,
}
impl std::io::Write for ProofBudget {
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
        let body = backend_library::encode_command_body(request).map_err(ClientError::Protocol)?;
        encode_compact_proof(&body)
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
            CommandReply::ProjectionPage(page) if matches!(&command, Command::GraphPage { .. }) => {
                page.snapshot.next
            }
            _ => None,
        };
        if matches!(
            &reply.reply,
            CommandReply::Search(_) | CommandReply::Names(_) | CommandReply::ProjectionPage(_)
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
            Command::Search(query) => Command::Search(query.with_cursor(cursor)),
            Command::Name(query) => Command::Name(query.with_cursor(cursor)),
            Command::GraphPage { symbol, page } => Command::GraphPage {
                symbol,
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
                // before any RPC. Only a graph continuation needs the fresh
                // owner to reopen its constant-size root commitment. This
                // untrusted tag selects that decoder; it admits no identity.
                if envelope
                    .get("command")
                    .and_then(|command| command.get("kind"))
                    .and_then(serde_json::Value::as_str)
                    != Some("graph_query")
                {
                    return Err(ClientError::Protocol(error));
                }
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
        };
        let cursor = match &imported.command {
            Command::GraphPage { page, .. } => page.continuation().map(PageContinuation::cursor),
            command => query_parts(command).and_then(|(_, _, _, cursor)| cursor),
        }
        .ok_or_else(|| ClientError::Protocol("token is not a continued query".to_owned()))?;
        if imported.request_id != 1 || cursor.query_offset() == 0 {
            return Err(ClientError::Protocol(
                "query token has invalid continuation metadata".to_owned(),
            ));
        }
        let revision = self.revision()?;
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
                backend_library::QueryPageRecipe::search(revision.root, query)
            }
            Command::Name(query) => backend_library::QueryPageRecipe::names(revision.root, query),
            Command::GraphPage { symbol, .. } => {
                backend_library::QueryPageRecipe::graph(revision.root, *symbol)
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
        self.prepared_query = Some(CommandDto::new(1, imported.command).with_certificate(fresh));
        Ok(PageContinuation::from_cursor(cursor))
    }

    pub(super) fn resume_prepared_graph(
        &mut self,
        symbol: SymbolAddress,
        limit: u16,
        continuation: Option<PageContinuation>,
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
        self.send_success(request.command, certificate)
    }

    pub(super) fn resume_prepared_query(
        &mut self,
        family: &str,
        text: &str,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let request = self
            .prepared_query
            .take()
            .ok_or_else(|| ClientError::Protocol("query was not prepared".to_owned()))?;
        let (actual_family, actual_text, actual_limit, actual_cursor) =
            query_parts(&request.command)
                .ok_or_else(|| ClientError::Protocol("prepared query changed family".to_owned()))?;
        let manifest_absent = match &request.command {
            Command::Search(query) => query.read_manifest().is_none(),
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
        self.send_success(request.command, certificate)
    }
}

#[cfg(test)]
#[path = "query_continuation_tests.rs"]
mod tests;

// Compression changes the presentation, not the canonical command or its proofs.
// Both compressed input and expanded command have independent allocation bounds.
fn encode_compact_proof(body: &[u8]) -> Result<String, ClientError> {
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
    Ok(format!("pc3-{}", URL_SAFE_NO_PAD.encode(compressed)))
}

fn decode_portable_body(token: &str) -> Result<Vec<u8>, ClientError> {
    if token.len() > MAX_PORTABLE_QUERY_TOKEN_BYTES {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    if let Some(encoded) = token.strip_prefix("pc2-") {
        return decode_hex(encoded)
            .ok_or_else(|| ClientError::Protocol("malformed query token".to_owned()));
    }
    let encoded = token
        .strip_prefix("pc3-")
        .ok_or_else(|| ClientError::Protocol("unknown query token version".to_owned()))?;
    let compressed = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| ClientError::Protocol("malformed compressed query token".to_owned()))?;
    let mut body = Vec::with_capacity(backend_library::MAX_COMMAND_BODY + 1);
    let mut decoder = flate2::Decompress::new(true);
    let status = decoder
        .decompress_vec(&compressed, &mut body, flate2::FlushDecompress::Finish)
        .map_err(|error| ClientError::Protocol(error.to_string()))?;
    if body.len() > backend_library::MAX_COMMAND_BODY {
        return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
    }
    if status != flate2::Status::StreamEnd || decoder.total_in() != compressed.len() as u64 {
        return Err(ClientError::Protocol(
            "query token is incomplete or contains trailing compressed bytes".to_owned(),
        ));
    }
    Ok(body)
}
