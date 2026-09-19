//! Durable dispatch intent, fence, result proof, and publication acknowledgement.

use super::{
    AcceptedResultProof, Daemon, DaemonError, DispatchAttemptKey, DispatchCompletion,
    DispatchError, DispatchPhase, DispatchTicket, JOURNAL_FALLBACK_SELECTED, NotificationCursor,
    OutputVersion, PendingRemoteKey, PublicationAck, QueueSized, Relation, RemoteAttemptIntent,
    TerminalState, TransferCheckpointRef, TransportMessage, WireRecipeRequest, WorkspaceModel,
};

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    pub(crate) fn journal_key_from_request(
        request: &WireRecipeRequest,
    ) -> Result<DispatchAttemptKey, DaemonError> {
        DispatchAttemptKey::new(request.work_key.as_bytes(), request.attempt.get())
            .map_err(|error| DaemonError::dispatch_journal(error.into()))
    }

    pub(crate) fn journal_key_from_pending(
        key: PendingRemoteKey,
    ) -> Result<DispatchAttemptKey, DaemonError> {
        DispatchAttemptKey::new(key.work_key().to_bytes(), key.attempt().get())
            .map_err(|error| DaemonError::dispatch_journal(error.into()))
    }

    pub(crate) fn journal_intent<R: Relation>(
        &self,
        ticket: &DispatchTicket<R>,
        request: &WireRecipeRequest,
    ) -> Result<DispatchAttemptKey, DaemonError> {
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Self::journal_key_from_request(request);
        };
        let key = Self::journal_key_from_request(request)?;
        let contract = ticket.contract().map_err(DaemonError::dispatch)?;
        let schedule = ticket.scheduled().map_err(DaemonError::dispatch)?;
        let encoded = TransportMessage::WireRecipeRequest(request.clone())
            .encode(contract.limits)
            .map_err(DaemonError::Replication)?;
        let deadline = schedule.fallback_deadline().unwrap_or(0);
        let intent = RemoteAttemptIntent::new(
            key,
            encoded.clone().into_boxed_slice(),
            self.owner.head().root().to_bytes(),
            self.owner.lease().fence(),
            self.owner.lease().epoch(),
            schedule.lease().expires_at(),
            deadline,
        )
        .map_err(|error| DaemonError::dispatch_journal(error.into()))?
        .with_revocation_version(request.authority.revocation_version.0);
        journal
            .admit(intent)
            .map_err(DaemonError::dispatch_journal)?;
        let progress = Self::transfer_checkpoint(request, &encoded)?;
        journal
            .record_transfer(key, progress)
            .map_err(DaemonError::dispatch_journal)?;
        Ok(key)
    }

    pub(crate) fn transfer_checkpoint(
        request: &WireRecipeRequest,
        encoded: &[u8],
    ) -> Result<TransferCheckpointRef, DaemonError> {
        let mut transfer = blake3::Hasher::new();
        transfer.update(b"backend.engine.dispatch.transfer.v1\0");
        transfer.update(&request.work_key.as_bytes());
        transfer.update(&request.attempt.get().to_be_bytes());
        transfer.update(encoded);
        let transfer = *transfer.finalize().as_bytes();
        let mut checkpoint = blake3::Hasher::new();
        checkpoint.update(b"backend.engine.dispatch.checkpoint.v1\0");
        checkpoint.update(&request.input_basis.as_bytes());
        checkpoint.update(&transfer);
        let checkpoint = *checkpoint.finalize().as_bytes();
        let bytes = u64::try_from(encoded.len()).map_err(|_| DaemonError::Backpressure)?;
        TransferCheckpointRef::new(transfer, checkpoint, bytes, bytes, DispatchPhase::Executing)
            .map_err(|error| DaemonError::dispatch_journal(error.into()))
    }

    pub(crate) fn journal_cancel(
        &self,
        key: DispatchAttemptKey,
        reason: u16,
    ) -> Result<(), DaemonError> {
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(());
        };
        journal
            .cancel(key, reason, self.library.cursor().sequence())
            .map_err(DaemonError::dispatch_journal)?;
        Ok(())
    }

    pub(crate) fn journal_fence(
        &self,
        key: DispatchAttemptKey,
        reason: u16,
    ) -> Result<(), DaemonError> {
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(());
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.engine.daemon.fallback-fence.v1\0");
        hasher.update(&self.owner.lease().fence());
        hasher.update(&key.work_key);
        hasher.update(&key.attempt.to_be_bytes());
        hasher.update(&reason.to_be_bytes());
        let fence = *hasher.finalize().as_bytes();
        journal
            .fence(key, self.owner.lease().epoch(), fence, reason)
            .map_err(DaemonError::dispatch_journal)?;
        Ok(())
    }

    pub(crate) fn journal_fallback_root(
        &self,
        key: DispatchAttemptKey,
        output_root: [u8; 32],
    ) -> Result<(), DaemonError> {
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(());
        };
        let reason = journal
            .point(key)
            .map_err(DaemonError::dispatch_journal)?
            .and_then(|attempt| attempt.terminal)
            .map_or(JOURNAL_FALLBACK_SELECTED, |terminal| match terminal {
                TerminalState::Cancelled { reason, .. }
                | TerminalState::Fallback { reason, .. } => reason,
            });
        journal
            .fallback(key, output_root, reason, self.library.cursor().sequence())
            .map_err(DaemonError::dispatch_journal)?;
        Ok(())
    }

    /// Records the owner-side fence before a recovered attempt is
    /// re-executed locally. Keeping the attempt nonterminal makes a crash
    /// during local execution recoverable without allowing the stale remote
    /// lease to publish.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn select_recovered_fallback(&self, key: DispatchAttemptKey) -> Result<(), DaemonError> {
        // Keep the attempt nonterminal until local admission has produced an
        // output.  Recording `Fallback` here would prevent the accepted
        // result proof from being appended by the staged completion path and
        // would turn a crash during execution into an output-less terminal
        // record.  The durable fence alone is enough to revoke the worker;
        // restart classifies the fenced attempt back into local fallback.
        self.journal_fence(key, JOURNAL_FALLBACK_SELECTED)
    }

    /// Completes the journal half of a recovered local fallback once its
    /// output has been durably published by the ordinary dispatcher path.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn complete_recovered_fallback(
        &self,
        key: DispatchAttemptKey,
        output_root: [u8; 32],
    ) -> Result<(), DaemonError> {
        self.journal_fallback_root(key, output_root)
    }

    /// Replays an accepted result whose catalog objects were already selected
    /// before the process stopped. The object references are checked against
    /// the current workspace closure before the journal publication
    /// acknowledgement is emitted; otherwise the caller must route the
    /// attempt through fenced local fallback.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acknowledge_recovered_publication(
        &self,
        key: DispatchAttemptKey,
        proof: &AcceptedResultProof,
    ) -> Result<bool, DaemonError> {
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(false);
        };
        let Some(attempt) = journal.point(key).map_err(DaemonError::dispatch_journal)? else {
            return Ok(false);
        };
        if attempt.publication.is_some() {
            return Ok(attempt
                .publication
                .is_some_and(|publication| publication.output_root == proof.output_root));
        }
        if attempt.accepted.as_ref() != Some(proof)
            || !proof.has_staged_objects()
            || OutputVersion::from_value(proof.proof.as_ref()).to_bytes() != proof.output_root
            || !self
                .owner
                .selected_contains_objects(proof.output_object, proof.manifest_object)
        {
            return Ok(false);
        }
        let cursor = self.library.cursor();
        let position = NotificationCursor::new(0, cursor.sequence(), self.cursor_bytes())
            .map_err(|error| DaemonError::dispatch_journal(error.into()))?;
        journal
            .acknowledge_publication_with_cursor_if(
                key,
                attempt.current_fence,
                PublicationAck {
                    output_root: proof.output_root,
                    publication_root: *self.owner.head().root().as_bytes(),
                    owner_epoch: self.owner.lease().epoch(),
                    notification_cursor: cursor.sequence(),
                },
                position,
            )
            .map_err(DaemonError::dispatch_journal)?;
        Ok(true)
    }

    pub(crate) fn journalize_completion(
        &self,
        key: DispatchAttemptKey,
        completion: &DispatchCompletion,
        now: u64,
        staged: Option<&crate::workspace::catalog::StagedDerivedOutput>,
    ) -> Result<(), DaemonError> {
        let DispatchCompletion::Accepted(receipt) = completion else {
            return Ok(());
        };
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(());
        };
        let proof = match staged {
            Some(staged) => staged.accepted_result_proof(now).map_err(|error| {
                DaemonError::dispatch(DispatchError::Workspace(error.to_string()))
            })?,
            None => AcceptedResultProof::new(
                receipt.output().to_bytes(),
                receipt
                    .canonical_bytes_arc()
                    .as_slice()
                    .to_vec()
                    .into_boxed_slice(),
                now,
            )
            .map_err(|error| DaemonError::dispatch_journal(error.into()))?,
        };
        journal
            .accept_result(key, proof)
            .map_err(DaemonError::dispatch_journal)?;
        journal
            .begin_publication(key)
            .map_err(DaemonError::dispatch_journal)?;
        Ok(())
    }

    pub(crate) fn acknowledge_journal_publication(
        &self,
        key: DispatchAttemptKey,
        completion: &DispatchCompletion,
    ) -> Result<(), DaemonError> {
        let DispatchCompletion::Accepted(receipt) = completion else {
            return Ok(());
        };
        let Some(journal) = self.dispatch_journal.as_ref() else {
            return Ok(());
        };
        let cursor = self.library.cursor();
        let ack = PublicationAck {
            output_root: receipt.output().to_bytes(),
            publication_root: *self.owner.head().root().as_bytes(),
            owner_epoch: self.owner.lease().epoch(),
            notification_cursor: cursor.sequence(),
        };
        let position = NotificationCursor::new(0, cursor.sequence(), self.cursor_bytes())
            .map_err(|error| DaemonError::dispatch_journal(error.into()))?;
        let Some(attempt) = journal.point(key).map_err(DaemonError::dispatch_journal)? else {
            return Ok(());
        };
        journal
            .acknowledge_publication_with_cursor_if(key, attempt.current_fence, ack, position)
            .map_err(DaemonError::dispatch_journal)?;
        Ok(())
    }
}
