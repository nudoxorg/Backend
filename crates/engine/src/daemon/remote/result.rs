//! Result admission, completion publication, cancellation, and fallback.

use super::{
    Arc, Daemon, DaemonError, DispatchAttemptKey, DispatchCompletion, DispatchError,
    DispatchTicket, JOURNAL_CANCEL_REQUESTED, JOURNAL_FALLBACK_SELECTED, PendingRemoteEnvelope,
    PendingRemoteKey, QueueSized, Relation, RemoteCorrelationKey, TransportMessage,
    WireRecipeResult, WorkspaceModel,
};

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    fn dispatch_completion(
        &mut self,
        key: DispatchAttemptKey,
        completion: &DispatchCompletion,
        now: u64,
    ) -> Result<(), DaemonError> {
        // Stage immutable output and dependency objects first.  The accepted
        // journal record then becomes the durable hand-off between CAS and
        // workspace selection; a crash in that interval leaves either an
        // orphaned immutable object (safe for GC) or a fully reconstructible
        // accepted proof.
        let staged = self.stage_dispatch_completion(completion)?;
        self.journalize_completion(key, completion, now, staged.as_ref())?;
        if let Some(staged) = staged {
            let output_key = match completion {
                DispatchCompletion::Accepted(receipt) => receipt.key(),
                DispatchCompletion::Reused(_) | DispatchCompletion::Waiting(_) => {
                    return Err(DaemonError::dispatch(DispatchError::Workspace(
                        "staged output has no accepted receipt".to_owned(),
                    )));
                }
            };
            let mut staged = Some(staged);
            self.dispatcher
                .publish_derived_output_retryable(output_key, |_proof| {
                    let staged = staged.take().ok_or(DispatchError::Workspace(
                        "staged output already consumed".to_owned(),
                    ))?;
                    self.owner
                        .publish_staged_derived_output(staged)
                        .map(|_| ())
                        .map_err(|error| DispatchError::Workspace(error.to_string()))
                })
                .map_err(DaemonError::dispatch)?;
        }
        self.acknowledge_journal_publication(key, completion)
    }

    /// Publishes a scheduler-terminal completion while its owner envelope
    /// remains indexed. Any failure restores the completion to that envelope,
    /// allowing an exact retry without re-running or losing the affine ticket.
    fn publish_pending_completion(
        &mut self,
        key: PendingRemoteKey,
        journal_key: DispatchAttemptKey,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let (completion, fallback) = self
            .pending_remote
            .get_mut(&key)
            .and_then(PendingRemoteEnvelope::take_prepared)
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        let published = self
            .dispatch_completion(journal_key, &completion, now)
            .and_then(|()| {
                if fallback {
                    self.finish_fallback_journal(journal_key, &completion)
                } else {
                    Ok(())
                }
            });
        if let Err(error) = published {
            if let Some(envelope) = self.pending_remote.get_mut(&key) {
                envelope.restore_prepared(completion, fallback);
            }
            return Err(error);
        }
        match self.remove_pending_remote(key) {
            Ok(envelope) => {
                drop(envelope);
                Ok(completion)
            }
            Err(error) => {
                if let Some(envelope) = self.pending_remote.get_mut(&key) {
                    envelope.restore_prepared(completion, fallback);
                }
                Err(error)
            }
        }
    }

    /// Marks a fenced remote attempt with the exact output selected by local
    /// fallback.  The result transaction above owns staging, accepted-proof
    /// journaling, workspace publication, and the fused publication/cursor
    /// acknowledgement; this final terminal record only closes the already
    /// published attempt.  A crash before it is durable therefore recovers
    /// the visible output as `Published`, while a crash after it recovers the
    /// same output as a terminal fallback.
    fn finish_fallback_journal(
        &self,
        key: DispatchAttemptKey,
        completion: &DispatchCompletion,
    ) -> Result<(), DaemonError> {
        let output_root = match completion {
            DispatchCompletion::Accepted(receipt) => receipt.output().to_bytes(),
            DispatchCompletion::Reused(output) => output.output().to_bytes(),
            DispatchCompletion::Waiting(_) => {
                return Err(DaemonError::dispatch(DispatchError::Workspace(
                    "local fallback remained a follower".to_owned(),
                )));
            }
        };
        self.journal_fallback_root(key, output_root)
    }

    fn complete_pending_remote(
        &mut self,
        key: PendingRemoteKey,
        result: Box<WireRecipeResult>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        // Keep the envelope in the owner map while admission runs. A frame
        // with valid correlation but bad authority/coverage/attestation must
        // leave the affine ticket available for explicit cancellation or
        // fallback; only a successful publication removes it.
        let result_bytes = Self::wire_result_size(&result)?;
        self.pending_remote
            .release_result_bytes(key, result_bytes)?;
        let completion = {
            let envelope = self
                .pending_remote
                .get_mut(&key)
                .ok_or(DaemonError::RemoteResultUnmatched)?;
            envelope.complete(&self.dispatcher, *result, now)
        };
        match completion {
            Ok(_) => {}
            Err(error) => {
                let terminal = self
                    .pending_remote
                    .get(&key)
                    .is_some_and(PendingRemoteEnvelope::is_terminal);
                if terminal {
                    let envelope = self.remove_pending_remote(key)?;
                    drop(envelope);
                }
                return Err(DaemonError::dispatch(error));
            }
        }
        let journal_key = Self::journal_key_from_pending(key)?;
        self.publish_pending_completion(key, journal_key, now)
    }

    /// Receives one remote result and routes it through exact dispatcher
    /// admission and scheduler completion. The ticket owns the exact
    /// contract, so a result cannot be paired with another plan's request
    /// material.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn receive_remote_result<R: Relation>(
        &mut self,
        ticket: DispatchTicket<R>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let Some(remote) = self.remote.as_mut() else {
            return Err(DaemonError::RemoteUnavailable);
        };
        let message = remote
            .recv()
            .map_err(DaemonError::Replication)?
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        let TransportMessage::WireRecipeResult(result) = message else {
            self.enqueue_replication(message)?;
            return Err(DaemonError::RemoteResultUnmatched);
        };
        let journal_key =
            Self::journal_key_from_request(&ticket.wire_request().map_err(DaemonError::dispatch)?)?;
        let completion = self
            .dispatcher
            .complete_remote_ticket(ticket, *result, now)
            .map_err(DaemonError::dispatch)?;
        self.dispatch_completion(journal_key, &completion, now)?;
        Ok(completion)
    }

    /// Receives and completes one daemon-owned pending remote envelope.
    /// Correlation is checked before removing the envelope from the map; a
    /// stale or cross-attempt frame therefore leaves the original ticket
    /// available for a later valid result.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn receive_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let Some(envelope) = self.pending_remote.get(&key) else {
            return Err(DaemonError::RemoteResultUnmatched);
        };
        let correlation = envelope.correlation();
        if let Some(result) = self.take_result(correlation) {
            return self.complete_pending_remote(key, result, now);
        }

        loop {
            let Some(message) = self.recv_remote_message()? else {
                return Err(DaemonError::RemoteResultUnmatched);
            };
            match message {
                TransportMessage::WireRecipeResult(result) => {
                    let received = RemoteCorrelationKey::new(result.work_key, result.attempt);
                    self.route_remote_message(TransportMessage::WireRecipeResult(result))?;
                    if received == correlation
                        && let Some(result) = self.take_result(correlation)
                    {
                        return self.complete_pending_remote(key, result, now);
                    }
                }
                control => {
                    self.enqueue_replication(control)?;
                }
            }
        }
    }

    /// Receives the next remote frame and completes whichever pending ticket
    /// it matches. Capability/control frames are retained in the replication
    /// lane so they cannot be mistaken for a result or silently discarded.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn receive_remote(&mut self, now: u64) -> Result<DispatchCompletion, DaemonError> {
        if let Some((key, result)) = self.take_any_result() {
            return self.complete_pending_remote(key, result, now);
        }
        loop {
            let Some(message) = self.recv_remote_message()? else {
                return Err(DaemonError::RemoteResultUnmatched);
            };
            match message {
                TransportMessage::WireRecipeResult(result) => {
                    self.route_remote_message(TransportMessage::WireRecipeResult(result))?;
                    if let Some((key, result)) = self.take_any_result() {
                        return self.complete_pending_remote(key, result, now);
                    }
                }
                control => {
                    self.enqueue_replication(control)?;
                }
            }
        }
    }

    /// Cancels the exact remote request and consumes its ticket.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancel_remote<R: Relation>(
        &mut self,
        ticket: DispatchTicket<R>,
    ) -> Result<(), DaemonError> {
        let cancellation = ticket.cancel_command().map_err(DaemonError::dispatch)?;
        let request = ticket.wire_request().map_err(DaemonError::dispatch)?;
        let journal_key = Self::journal_key_from_request(&request)?;
        self.journal_cancel(journal_key, JOURNAL_CANCEL_REQUESTED)?;
        let transport = match self.remote.as_mut() {
            Some(remote) => remote
                .cancel(cancellation)
                .map_err(DaemonError::Replication),
            None => Err(DaemonError::RemoteUnavailable),
        };
        self.dispatcher
            .cancel_ticket(ticket)
            .map_err(DaemonError::dispatch)?;
        transport
    }

    /// Cancels a daemon-owned pending ticket and sends its full attempt/fence
    /// command to the peer. The local ticket is consumed even when transport
    /// delivery fails, so a late result cannot retain a scheduler reservation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancel_remote_pending(&mut self, key: PendingRemoteKey) -> Result<(), DaemonError> {
        let journal_key = Self::journal_key_from_pending(key)?;
        self.journal_cancel(journal_key, JOURNAL_CANCEL_REQUESTED)?;
        let envelope = self.remove_pending_remote(key)?;
        let command = envelope.cancel_command();
        let transport = self
            .remote
            .as_mut()
            .ok_or(DaemonError::RemoteUnavailable)
            .and_then(|remote| remote.cancel(command).map_err(DaemonError::Replication));
        envelope.cancel();
        transport
    }

    /// Cancels the peer best-effort, consumes the daemon-owned pending ticket,
    /// and activates its exact local fallback. The fallback remains usable
    /// during an outage because peer cancellation is advisory once the owner
    /// has fenced the remote attempt locally.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fallback_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.complete_pending_fallback(key, bytes, now, false)
    }

    /// Activates an owner-held fallback immediately after a terminal remote
    /// transport or admission failure. This bypasses only the latency timer;
    /// the local reserve, replacement fence, semantic admission, journal, and
    /// publication transaction remain mandatory.
    /// # Errors
    ///
    /// Returns an error when the pending key is unknown or fallback output
    /// fails exact local admission.
    pub fn fail_remote_pending(
        &mut self,
        key: PendingRemoteKey,
        bytes: Arc<Vec<u8>>,
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        self.complete_pending_fallback(key, bytes, now, true)
    }

    fn complete_pending_fallback(
        &mut self,
        key: PendingRemoteKey,
        bytes: Arc<Vec<u8>>,
        now: u64,
        remote_failed: bool,
    ) -> Result<DispatchCompletion, DaemonError> {
        let journal_key = Self::journal_key_from_pending(key)?;
        let prepared_kind = self
            .pending_remote
            .get(&key)
            .ok_or(DaemonError::RemoteResultUnmatched)?
            .prepared_kind();
        if prepared_kind.is_none() {
            let pending = self
                .pending_remote
                .get_mut(&key)
                .ok_or(DaemonError::RemoteResultUnmatched)?;
            if remote_failed {
                pending.fail_remote(&self.dispatcher, bytes, now)
            } else {
                pending.fallback(&self.dispatcher, bytes, now)
            }
            .map_err(DaemonError::dispatch)?;
        }
        let fallback_prepared = self
            .pending_remote
            .get(&key)
            .ok_or(DaemonError::RemoteResultUnmatched)?
            .prepared_kind()
            == Some(true);
        if fallback_prepared {
            // Scheduler/output admission prepares in memory first. The
            // durable fence is the commit point before peer cancellation or
            // workspace publication, and is idempotent across retries.
            self.journal_fence(journal_key, JOURNAL_FALLBACK_SELECTED)?;
            let cancel = {
                let pending = self
                    .pending_remote
                    .get_mut(&key)
                    .ok_or(DaemonError::RemoteResultUnmatched)?;
                if pending.fallback_cancel_attempted() {
                    None
                } else {
                    pending.mark_fallback_cancel_attempted();
                    Some(pending.cancel_command())
                }
            };
            if let (Some(remote), Some(cancel)) = (self.remote.as_mut(), cancel) {
                let _ = remote.cancel(cancel);
            }
        }
        self.publish_pending_completion(key, journal_key, now)
    }

    /// Cancels the remote side, activates the ticket's local fallback, and
    /// publishes the fallback output through the same semantic authority
    /// checks as ordinary local completion.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fallback_remote<R: Relation>(
        &mut self,
        ticket: DispatchTicket<R>,
        bytes: &[u8],
        now: u64,
    ) -> Result<DispatchCompletion, DaemonError> {
        let cancellation = ticket.cancel_command().map_err(DaemonError::dispatch)?;
        let request = ticket.wire_request().map_err(DaemonError::dispatch)?;
        let journal_key = Self::journal_key_from_request(&request)?;
        self.journal_fence(journal_key, JOURNAL_FALLBACK_SELECTED)?;
        if let Some(remote) = self.remote.as_mut() {
            // Local fallback owns the scheduler fence. Peer cancellation is
            // advisory once that fence is transferred, so a transport outage
            // must not prevent the owner from completing locally.
            let _ = remote.cancel(cancellation);
        }
        let completion = self
            .dispatcher
            .fallback_remote_ticket(ticket, bytes, now)
            .map_err(DaemonError::dispatch)?;
        self.dispatch_completion(journal_key, &completion, now)?;
        self.finish_fallback_journal(journal_key, &completion)?;
        Ok(completion)
    }
}
