//! Portable queries reopen existing command proofs against a fresh owner.
//! Imported certificate bytes establish canonical preimages, never authority.

use super::*;

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
        if bytes.len() > ((MAX_PORTABLE_QUERY_TOKEN_BYTES - 4) / 2).saturating_sub(self.bytes) {
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
    /// Whether this admitted cursor carries a portable Search/Name command.
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
        if body.len() > (MAX_PORTABLE_QUERY_TOKEN_BYTES - 4) / 2 {
            return Err(ClientError::Transport(ReplicationError::MessageTooLarge));
        }
        let mut token = String::with_capacity(4 + body.len() * 2);
        token.push_str("pc2-");
        for byte in body {
            use fmt::Write as _;
            let _ = write!(token, "{byte:02x}");
        }
        Ok(token)
    }

    pub(super) fn retain_portable_query(
        &mut self,
        command: Command,
        owner_certificate: &WireCertificate,
        reply: &ReplyDto,
    ) -> Result<(), ClientError> {
        let next = match &reply.reply {
            CommandReply::Search(page) | CommandReply::Names(page) => page.next,
            _ => None,
        };
        if matches!(
            &reply.reply,
            CommandReply::Search(_) | CommandReply::Names(_)
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
            _ => return Ok(()),
        };
        // Keep only the owner basis commitment, issuing scope, and canonical
        // predecessor claims used by the existing standalone command decoder.
        let mut proof = WireCertificate::new().with_claim(scope(owner_certificate)?.clone());
        let owner_root = basis(&next_command)
            .ok_or_else(|| ClientError::Protocol("query basis omitted".to_owned()))?;
        for claim in &owner_certificate.claims {
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
        let encoded = token
            .strip_prefix("pc2-")
            .ok_or_else(|| ClientError::Protocol("unknown query token version".to_owned()))?;
        let body = decode_hex(encoded)
            .ok_or_else(|| ClientError::Protocol("malformed query token".to_owned()))?;
        // The existing strict command codec rehashes every cursor preimage.
        // Its result is still an imported request, not admitted owner state.
        let imported =
            backend_library::decode_command_body(&body).map_err(ClientError::Protocol)?;
        let (_, _, _, Some(cursor)) = query_parts(&imported.command)
            .ok_or_else(|| ClientError::Protocol("token is not a continued query".to_owned()))?
        else {
            return Err(ClientError::Protocol(
                "query token omitted predecessor".to_owned(),
            ));
        };
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
            if claim_describes_cursor(claim, cursor) {
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
