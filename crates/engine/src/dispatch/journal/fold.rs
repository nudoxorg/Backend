//! Bounded one-frame-at-a-time dispatch replay fold.

use super::model::RecoveredAttempt;
use super::record::{
    AcceptedResultProof, DispatchAttemptKey, DispatchCursor, DispatchJournalLimits, DispatchPhase,
    DispatchRecord, DispatchRecordError, NotificationCursor, PublicationAck, RemoteAttemptIntent,
    TerminalState, TransferCheckpointRef,
};
use super::restart::fallback_completion_update;
use crate::journal::{JournalCodec, JournalError, JournalFrameRef};
use std::collections::BTreeMap;

pub(super) struct ReplayState {
    pub(super) attempts: BTreeMap<DispatchAttemptKey, RecoveredAttempt>,
    limits: DispatchJournalLimits,
}

impl ReplayState {
    pub(super) fn new(limits: DispatchJournalLimits) -> Self {
        Self {
            attempts: BTreeMap::new(),
            limits,
        }
    }

    pub(super) fn apply(&mut self, record: &DispatchRecord) -> Result<(), DispatchRecordError> {
        self.limits.accepts(record)?;
        match record {
            DispatchRecord::Admitted { intent } => self.admit(intent),
            DispatchRecord::Transfer { key, progress } => self.transfer(*key, *progress),
            DispatchRecord::Accepted { key, proof } => self.accept(*key, proof),
            DispatchRecord::Published { key, ack } => self.publish(*key, *ack),
            DispatchRecord::PublishedWithCursor { key, ack, cursor } => {
                self.publish_with_cursor(*key, *ack, cursor)
            }
            DispatchRecord::PublicationPending { key } => self.publication_pending(*key),
            DispatchRecord::Terminal { key, terminal } => self.terminal(*key, *terminal),
            DispatchRecord::Cursor { cursor } => self.cursor(cursor),
            DispatchRecord::Fenced {
                key,
                owner_epoch,
                fence,
                reason,
            } => self.fence(*key, *owner_epoch, *fence, *reason),
        }
    }

    pub(super) fn validate(&self, record: &DispatchRecord) -> Result<(), DispatchRecordError> {
        self.limits.accepts(record)?;
        match record {
            DispatchRecord::Admitted { intent } => self.validate_admitted(intent),
            DispatchRecord::Transfer { key, progress } => self.validate_transfer(*key, *progress),
            DispatchRecord::Accepted { key, proof } => self.validate_accepted(*key, proof),
            DispatchRecord::Published { key, ack } => self.validate_published(*key, *ack),
            DispatchRecord::PublishedWithCursor { key, ack, cursor } => {
                self.validate_published_with_cursor(*key, *ack, cursor)
            }
            DispatchRecord::PublicationPending { key } => self.validate_publication_pending(*key),
            DispatchRecord::Terminal { key, terminal } => self.validate_terminal(*key, *terminal),
            DispatchRecord::Cursor { cursor } => self.validate_cursor(cursor),
            DispatchRecord::Fenced {
                key,
                owner_epoch,
                fence,
                reason,
            } => self.validate_fenced(*key, *owner_epoch, *fence, *reason),
        }
    }

    fn validate_admitted(&self, intent: &RemoteAttemptIntent) -> Result<(), DispatchRecordError> {
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
        Ok(())
    }

    fn validate_transfer(
        &self,
        key: DispatchAttemptKey,
        progress: TransferCheckpointRef,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
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
        if valid {
            Ok(())
        } else {
            Err(DispatchRecordError::IllegalTransition)
        }
    }

    fn validate_accepted(
        &self,
        key: DispatchAttemptKey,
        proof: &AcceptedResultProof,
    ) -> Result<(), DispatchRecordError> {
        if (proof.output_object == [0; 32]) != (proof.manifest_object == [0; 32])
            || (proof.output_object != [0; 32]) == proof.provenance.is_empty()
        {
            return Err(DispatchRecordError::InvalidIdentifier);
        }
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
        if let Some(old) = attempt.accepted.as_ref()
            && accepted_payload_eq(old, proof)
        {
            // `accepted_at` belongs to the admission observation, not to the
            // immutable result identity. A crash after an accepted proof but
            // before catalog publication may cause deterministic local
            // fallback to admit the same staged objects again later.
            if matches!(
                attempt.phase,
                DispatchPhase::ResultAccepted
                    | DispatchPhase::PublicationPending
                    | DispatchPhase::Fenced
            ) || (attempt.phase == DispatchPhase::Fallback
                && matches!(
                    attempt.terminal,
                    Some(TerminalState::Fallback { output_root, .. })
                        if output_root == [0; 32]
                ))
            {
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
        if matches!(
            attempt.phase,
            DispatchPhase::Admitted
                | DispatchPhase::Transferring
                | DispatchPhase::Executing
                | DispatchPhase::Fenced
                | DispatchPhase::Fallback
                | DispatchPhase::ResultAccepted
        ) {
            Ok(())
        } else {
            Err(DispatchRecordError::IllegalTransition)
        }
    }

    fn validate_published(
        &self,
        key: DispatchAttemptKey,
        ack: PublicationAck,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
        if attempt.phase == DispatchPhase::Published && attempt.publication == Some(ack) {
            return Ok(());
        }
        Self::validate_publication(attempt, ack.output_root)?;
        if matches!(
            attempt.phase,
            DispatchPhase::ResultAccepted
                | DispatchPhase::PublicationPending
                | DispatchPhase::Published
        ) {
            Ok(())
        } else {
            Err(DispatchRecordError::IllegalTransition)
        }
    }

    fn validate_published_with_cursor(
        &self,
        key: DispatchAttemptKey,
        ack: PublicationAck,
        cursor: &NotificationCursor,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
        if attempt.phase == DispatchPhase::Published
            && attempt.publication == Some(ack)
            && attempt.cursor.as_ref() == Some(cursor)
        {
            return Ok(());
        }
        Self::validate_publication(attempt, ack.output_root)?;
        if let Some(old) = &attempt.cursor
            && (cursor.waiter < old.waiter || cursor.subscription < old.subscription)
        {
            return Err(DispatchRecordError::IllegalTransition);
        }
        if matches!(
            attempt.phase,
            DispatchPhase::ResultAccepted
                | DispatchPhase::PublicationPending
                | DispatchPhase::Published
        ) {
            Ok(())
        } else {
            Err(DispatchRecordError::IllegalTransition)
        }
    }

    fn validate_publication(
        attempt: &RecoveredAttempt,
        output_root: [u8; 32],
    ) -> Result<(), DispatchRecordError> {
        let Some(proof) = attempt.accepted.as_ref() else {
            return Err(DispatchRecordError::PublicationMismatch);
        };
        if proof.output_root != output_root {
            return Err(DispatchRecordError::PublicationMismatch);
        }
        if attempt.phase.is_terminal() && attempt.phase != DispatchPhase::Published {
            return Err(DispatchRecordError::AlreadyTerminal);
        }
        Ok(())
    }

    fn validate_publication_pending(
        &self,
        key: DispatchAttemptKey,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
        if matches!(
            attempt.phase,
            DispatchPhase::PublicationPending | DispatchPhase::ResultAccepted
        ) && attempt.accepted.is_some()
        {
            Ok(())
        } else {
            Err(DispatchRecordError::IllegalTransition)
        }
    }

    fn validate_terminal(
        &self,
        key: DispatchAttemptKey,
        terminal: TerminalState,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
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
            return if fallback_completion_update(attempt.terminal, terminal) {
                Ok(())
            } else {
                Err(DispatchRecordError::TerminalMismatch)
            };
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
            return if valid_published_fallback {
                Ok(())
            } else {
                Err(DispatchRecordError::AlreadyTerminal)
            };
        }
        if matches!(terminal, TerminalState::Cancelled { .. })
            && attempt.accepted.is_some()
            && attempt.publication.is_none()
        {
            return Err(DispatchRecordError::TerminalMismatch);
        }
        Ok(())
    }

    fn validate_cursor(&self, cursor: &DispatchCursor) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&cursor.key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
        if let Some(old) = &attempt.cursor
            && (cursor.position.waiter < old.waiter
                || cursor.position.subscription < old.subscription)
        {
            return Err(DispatchRecordError::IllegalTransition);
        }
        Ok(())
    }

    fn validate_fenced(
        &self,
        key: DispatchAttemptKey,
        owner_epoch: u64,
        fence: [u8; 32],
        reason: u16,
    ) -> Result<(), DispatchRecordError> {
        let attempt = self
            .attempts
            .get(&key)
            .ok_or(DispatchRecordError::UnknownAttempt)?;
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
        Ok(())
    }

    fn admit(&mut self, intent: &RemoteAttemptIntent) -> Result<(), DispatchRecordError> {
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

    fn ensure_nonterminal(attempt: &RecoveredAttempt) -> Result<(), DispatchRecordError> {
        if attempt.phase.is_terminal() {
            Err(DispatchRecordError::AlreadyTerminal)
        } else {
            Ok(())
        }
    }

    fn transfer(
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

    fn accept(
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

    fn publish(
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

    fn publish_with_cursor(
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

    fn publication_pending(&mut self, key: DispatchAttemptKey) -> Result<(), DispatchRecordError> {
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

    fn terminal(
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

    fn cursor(&mut self, cursor: &DispatchCursor) -> Result<(), DispatchRecordError> {
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

    fn fence(
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

/// Compares the immutable accepted-result identity while ignoring the local
/// admission timestamp.  The timestamp is useful for diagnostics, but it is
/// not allowed to turn an exact retry after an ambiguous/crashed append into
/// a conflicting result.
fn accepted_payload_eq(left: &AcceptedResultProof, right: &AcceptedResultProof) -> bool {
    left.output_root == right.output_root
        && left.output_object == right.output_object
        && left.manifest_object == right.manifest_object
        && left.proof == right.proof
        && left.provenance == right.provenance
}

/// Folds one borrowed authenticated frame into the bounded replay state.
pub(super) fn fold_frame(
    replay: &mut ReplayState,
    frame: &JournalFrameRef<'_, super::record::DispatchLog>,
) -> Result<(), JournalError> {
    let record = super::record::DispatchLog::decode(frame.payload)?;
    replay.apply(&record).map_err(Into::into)
}
