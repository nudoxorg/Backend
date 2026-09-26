//! Applies admitted dispatch records onto the replay state.

use super::{
    AcceptedResultProof, DispatchAttemptKey, DispatchCursor, DispatchPhase, DispatchRecordError,
    NotificationCursor, PublicationAck, RecoveredAttempt, RemoteAttemptIntent, ReplayState,
    TerminalState, TransferCheckpointRef, accepted_payload_eq, fallback_completion_update,
};

impl ReplayState {
    /// Records one admitted remote attempt, accepting an exact retry.
    pub(super) fn admit(
        &mut self,
        intent: &RemoteAttemptIntent,
    ) -> Result<(), DispatchRecordError> {
        if intent.phase != DispatchPhase::Admitted || intent.fence == [0; 32] {
            return Err(DispatchRecordError::IllegalTransition);
        }
        if let Some(existing) = self.attempts.get(&intent.key) {
            if existing.intent == *intent {
                return Ok(());
            }
            return Err(DispatchRecordError::IdentityCollision);
        }
        if self.attempts.len() >= self.limits.max_attempts {
            return Err(DispatchRecordError::Bounds);
        }
        self.attempts.insert(
            intent.key,
            RecoveredAttempt {
                intent: intent.clone(),
                phase: DispatchPhase::Admitted,
                current_fence: intent.fence,
                transfer: None,
                accepted: None,
                publication: None,
                terminal: None,
                fenced: None,
                cursor: None,
            },
        );
        Ok(())
    }

    fn get_mut(
        &mut self,
        key: DispatchAttemptKey,
    ) -> Result<&mut RecoveredAttempt, DispatchRecordError> {
        self.attempts
            .get_mut(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)
    }

    /// Advances one attempt through a transfer checkpoint.
    pub(super) fn transfer(
        &mut self,
        key: DispatchAttemptKey,
        progress: TransferCheckpointRef,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if attempt.phase == progress.phase && attempt.transfer == Some(progress) {
            return Ok(());
        }
        Self::ensure_nonterminal(attempt)?;
        let valid = matches!(
            (attempt.phase, progress.phase),
            (
                DispatchPhase::Admitted | DispatchPhase::Transferring,
                DispatchPhase::Transferring | DispatchPhase::Executing
            ) | (DispatchPhase::Executing, DispatchPhase::Executing)
        );
        if !valid {
            return Err(DispatchRecordError::IllegalTransition);
        }
        attempt.phase = progress.phase;
        attempt.transfer = Some(progress);
        Ok(())
    }

    /// Records the accepted result proof for one attempt.
    pub(super) fn accept(
        &mut self,
        key: DispatchAttemptKey,
        proof: &AcceptedResultProof,
    ) -> Result<(), DispatchRecordError> {
        if (proof.output_object == [0; 32]) != (proof.manifest_object == [0; 32])
            || (proof.output_object != [0; 32]) == proof.provenance.is_empty()
        {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        let attempt = self.get_mut(key)?;
        if let Some(old) = attempt.accepted.as_ref()
            && accepted_payload_eq(old, proof)
        {
            if matches!(
                attempt.phase,
                DispatchPhase::ResultAccepted | DispatchPhase::PublicationPending
            ) {
                return Ok(());
            }
            if attempt.phase == DispatchPhase::Fenced
                || (attempt.phase == DispatchPhase::Fallback
                    && matches!(
                        attempt.terminal,
                        Some(TerminalState::Fallback { output_root, .. })
                            if output_root == [0; 32]
                    ))
            {
                // The owner fence has already revoked the remote producer;
                // re-enter the result phase so the ordinary publication
                // transaction can publish the deterministic local result.
                attempt.phase = DispatchPhase::ResultAccepted;
                return Ok(());
            }
        }
        if attempt.accepted.as_ref().is_some_and(|old| old != proof) {
            return Err(DispatchRecordError::IdentityCollision);
        }
        let pending_fallback = matches!(
            attempt.terminal,
            Some(TerminalState::Fallback { output_root, .. })
                if output_root == [0; 32]
        );
        if matches!(
            attempt.phase,
            DispatchPhase::Published | DispatchPhase::Cancelled
        ) || (attempt.phase == DispatchPhase::Fallback && !pending_fallback)
        {
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        if !matches!(
            attempt.phase,
            DispatchPhase::Admitted
                | DispatchPhase::Transferring
                | DispatchPhase::Executing
                | DispatchPhase::Fenced
                | DispatchPhase::Fallback
                | DispatchPhase::ResultAccepted
        ) {
            return Err(DispatchRecordError::IllegalTransition);
        }
        // A fenced attempt may accept exactly one locally produced result.
        // The fence revokes the remote lease; it does not discard the
        // staged-output transaction needed to make fallback crash-safe.
        attempt.phase = DispatchPhase::ResultAccepted;
        attempt.accepted = Some(proof.clone());
        Ok(())
    }

    /// Records a publication acknowledgement.
    pub(super) fn publish(
        &mut self,
        key: DispatchAttemptKey,
        ack: PublicationAck,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if attempt.phase == DispatchPhase::Published && attempt.publication == Some(ack) {
            return Ok(());
        }
        let Some(proof) = attempt.accepted.as_ref() else {
            return Err(DispatchRecordError::PublicationMismatch);
        };
        if proof.output_root != ack.output_root {
            return Err(DispatchRecordError::PublicationMismatch);
        }
        if attempt.phase.is_terminal() && attempt.phase != DispatchPhase::Published {
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        if !matches!(
            attempt.phase,
            DispatchPhase::ResultAccepted
                | DispatchPhase::PublicationPending
                | DispatchPhase::Published
        ) {
            return Err(DispatchRecordError::IllegalTransition);
        }
        attempt.phase = DispatchPhase::Published;
        attempt.publication = Some(ack);
        Ok(())
    }

    /// Records a publication acknowledgement and its notification cursor.
    pub(super) fn publish_with_cursor(
        &mut self,
        key: DispatchAttemptKey,
        ack: PublicationAck,
        cursor: &NotificationCursor,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if attempt.phase == DispatchPhase::Published
            && attempt.publication == Some(ack)
            && attempt.cursor.as_ref() == Some(cursor)
        {
            return Ok(());
        }
        let Some(proof) = attempt.accepted.as_ref() else {
            return Err(DispatchRecordError::PublicationMismatch);
        };
        if proof.output_root != ack.output_root {
            return Err(DispatchRecordError::PublicationMismatch);
        }
        if attempt.phase.is_terminal() && attempt.phase != DispatchPhase::Published {
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        if let Some(old) = &attempt.cursor
            && (cursor.waiter < old.waiter || cursor.subscription < old.subscription)
        {
            return Err(DispatchRecordError::IllegalTransition);
        }
        if !matches!(
            attempt.phase,
            DispatchPhase::ResultAccepted
                | DispatchPhase::PublicationPending
                | DispatchPhase::Published
        ) {
            return Err(DispatchRecordError::IllegalTransition);
        }
        attempt.phase = DispatchPhase::Published;
        attempt.publication = Some(ack);
        attempt.cursor = Some(cursor.clone());
        Ok(())
    }

    /// Marks an accepted attempt as waiting for publication.
    pub(super) fn publication_pending(
        &mut self,
        key: DispatchAttemptKey,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if attempt.phase == DispatchPhase::PublicationPending {
            return Ok(());
        }
        if attempt.phase != DispatchPhase::ResultAccepted || attempt.accepted.is_none() {
            return Err(DispatchRecordError::IllegalTransition);
        }
        attempt.phase = DispatchPhase::PublicationPending;
        Ok(())
    }

    /// Records a terminal state for one attempt.
    pub(super) fn terminal(
        &mut self,
        key: DispatchAttemptKey,
        terminal: TerminalState,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if let TerminalState::Fallback { output_root, .. } = terminal
            && output_root != [0; 32]
            && attempt
                .accepted
                .as_ref()
                .is_some_and(|proof| proof.output_root != output_root)
        {
            return Err(DispatchRecordError::PublicationMismatch);
        }
        if attempt.terminal == Some(terminal) {
            return Ok(());
        }
        if attempt.terminal.is_some() {
            if fallback_completion_update(attempt.terminal, terminal) {
                attempt.phase = terminal.phase();
                attempt.terminal = Some(terminal);
                return Ok(());
            }
            return Err(DispatchRecordError::TerminalMismatch);
        }
        if attempt.phase == DispatchPhase::Published {
            let valid_published_fallback = matches!(
                terminal,
                TerminalState::Fallback { output_root, .. }
                    if output_root != [0; 32]
                        && attempt
                            .publication
                            .is_some_and(|ack| ack.output_root == output_root)
            );
            if valid_published_fallback {
                attempt.phase = DispatchPhase::Fallback;
                attempt.terminal = Some(terminal);
                return Ok(());
            }
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        if matches!(terminal, TerminalState::Cancelled { .. })
            && attempt.accepted.is_some()
            && attempt.publication.is_none()
        {
            // A cancellation cannot discard a proof that the owner may still
            // need to publish. Fallback is the only safe terminal transition.
            return Err(DispatchRecordError::TerminalMismatch);
        }
        attempt.phase = terminal.phase();
        attempt.terminal = Some(terminal);
        Ok(())
    }

    /// Records a notification cursor for one attempt.
    pub(super) fn cursor(&mut self, cursor: &DispatchCursor) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(cursor.key)?;
        if let Some(old) = &attempt.cursor {
            if cursor.position.waiter < old.waiter
                || cursor.position.subscription < old.subscription
            {
                return Err(DispatchRecordError::IllegalTransition);
            }
            if old == &cursor.position {
                return Ok(());
            }
        }
        attempt.cursor = Some(cursor.position.clone());
        Ok(())
    }

    /// Records an owner fence that revokes the remote producer.
    pub(super) fn fence(
        &mut self,
        key: DispatchAttemptKey,
        owner_epoch: u64,
        fence: [u8; 32],
        reason: u16,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self.get_mut(key)?;
        if attempt.fenced == Some((owner_epoch, reason)) && attempt.current_fence == fence {
            return Ok(());
        }
        if attempt.phase.is_terminal() {
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        if fence == [0; 32] || owner_epoch < attempt.intent.owner_epoch {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        if attempt.fenced.is_some() {
            return Err(DispatchRecordError::TerminalMismatch);
        }
        attempt.phase = DispatchPhase::Fenced;
        attempt.current_fence = fence;
        attempt.fenced = Some((owner_epoch, reason));
        Ok(())
    }
}
