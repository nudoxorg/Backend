//! Restart classification, fencing, and fallback decisions.

use super::error::{
    DispatchJournalError, RESTART_ALREADY_FENCED, RESTART_AUTHORITY_REVOKED, RESTART_LEASE_EXPIRED,
};
use super::journal::DispatchJournal;
use super::model::{
    AuthoritySnapshot, DispatchRecoveryAction, DispatchRestartAuthority, DispatchRestartDecision,
    RecoveredAttempt,
};
use super::record::{
    DispatchAttemptKey, DispatchPhase, DispatchRecordError, NotificationCursor, TerminalState,
};
use crate::fault::Boundary;
use crate::journal::JournalError;
use std::ops::Bound;

impl DispatchJournal {
    /// Selects safe restart actions and persists fencing/fallback transitions
    /// for attempts whose lease or authority is no longer valid.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn restart<A: DispatchRestartAuthority>(
        &self,
        now: u64,
        authority: &A,
    ) -> Result<Vec<DispatchRecoveryAction>, DispatchJournalError> {
        let mut actions = Vec::new();
        self.visit_restart(now, authority, |action| actions.push(action))?;
        Ok(actions)
    }

    /// Visits restart actions one at a time.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn visit_restart<A, F>(
        &self,
        now: u64,
        authority: &A,
        mut visit: F,
    ) -> Result<(), DispatchJournalError>
    where
        A: DispatchRestartAuthority,
        F: FnMut(DispatchRecoveryAction),
    {
        self.faults.trip(Boundary::Recovery)?;
        // Clone one bounded attempt at a time. `restart_attempt` appends a
        // fence/fallback transition and therefore cannot hold the replay map
        // lock while it mutates the journal. Advancing by key keeps recovery
        // streaming and avoids a second full map allocation at takeover.
        let mut after = None;
        loop {
            let next = {
                let state = self.state.lock().map_err(|_| {
                    DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay"))
                })?;
                match after {
                    None => state.attempts.first_key_value(),
                    Some(key) => state
                        .attempts
                        .range((Bound::Excluded(key), Bound::Unbounded))
                        .next(),
                }
                .map(|(key, attempt)| (*key, attempt.clone()))
            };
            let Some((key, attempt)) = next else {
                break;
            };
            after = Some(key);
            if let Some(action) = self.restart_attempt(key, attempt, now, authority)? {
                visit(action);
            }
        }
        Ok(())
    }

    fn restart_attempt<A: DispatchRestartAuthority>(
        &self,
        key: DispatchAttemptKey,
        attempt: RecoveredAttempt,
        now: u64,
        authority: &A,
    ) -> Result<Option<DispatchRecoveryAction>, DispatchJournalError> {
        if attempt.phase.is_terminal() {
            return Ok(None);
        }
        let decision = authority.decide(&attempt, now);
        let lease_ok = attempt.intent.lease_until > now;
        let current = match decision {
            DispatchRestartDecision::Permit(current) => current,
            DispatchRestartDecision::Fence { current, reason } => {
                self.fence_and_fallback(key, &attempt, current, reason)?;
                return self.fallback_action(key, reason);
            }
            DispatchRestartDecision::Defer => return Ok(None),
        };
        if matches!(
            attempt.phase,
            DispatchPhase::ResultAccepted | DispatchPhase::PublicationPending
        ) && attempt.publication_pending()
        {
            let Some(proof) = attempt.accepted.clone() else {
                return Ok(None);
            };
            return Ok(Some(DispatchRecoveryAction::PublishAccepted {
                attempt,
                proof,
            }));
        }
        if attempt.phase.may_resend() && lease_ok {
            return Ok(Some(DispatchRecoveryAction::Resend { attempt }));
        }
        let reason = if attempt.phase == DispatchPhase::Fenced {
            RESTART_ALREADY_FENCED
        } else if !lease_ok {
            RESTART_LEASE_EXPIRED
        } else {
            RESTART_AUTHORITY_REVOKED
        };
        self.fence_and_fallback(key, &attempt, current, reason)?;
        self.fallback_action(key, reason)
    }

    fn fence_and_fallback(
        &self,
        key: DispatchAttemptKey,
        attempt: &RecoveredAttempt,
        current: AuthoritySnapshot,
        reason: u16,
    ) -> Result<(), DispatchJournalError> {
        if current.owner_epoch() < attempt.intent.owner_epoch {
            return Err(DispatchJournalError::Record(
                DispatchRecordError::InvalidIdentifier,
            ));
        }
        if attempt.phase != DispatchPhase::Fenced {
            let next_fence = Self::derive_restart_fence(attempt, current, reason);
            self.fence(key, current.owner_epoch(), next_fence, reason)?;
        }
        let cursor = attempt
            .cursor
            .as_ref()
            .map_or(0, NotificationCursor::watermark)
            .max(current.notification_cursor());
        self.fallback(key, [0; 32], reason, cursor)?;
        Ok(())
    }

    fn fallback_action(
        &self,
        key: DispatchAttemptKey,
        reason: u16,
    ) -> Result<Option<DispatchRecoveryAction>, DispatchJournalError> {
        let proof = {
            let state = self.state.lock().map_err(|_| {
                DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay"))
            })?;
            state
                .attempts
                .get(&key)
                .and_then(|attempt| attempt.accepted.clone())
        };
        let recovered = {
            let state = self.state.lock().map_err(|_| {
                DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay"))
            })?;
            state
                .attempts
                .get(&key)
                .cloned()
                .ok_or(DispatchRecordError::UnknownAttempt)?
        };
        Ok(Some(DispatchRecoveryAction::Fallback {
            attempt: recovered,
            proof,
            reason,
        }))
    }

    fn derive_restart_fence(
        attempt: &RecoveredAttempt,
        current: AuthoritySnapshot,
        reason: u16,
    ) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.engine.dispatch.restart-fence.v1\0");
        hasher.update(&attempt.intent.key.work_key);
        hasher.update(&attempt.intent.key.attempt.to_be_bytes());
        hasher.update(&attempt.current_fence);
        hasher.update(&current.owner_epoch().to_be_bytes());
        hasher.update(&current.revocation_version().to_be_bytes());
        hasher.update(&current.notification_cursor().to_be_bytes());
        hasher.update(&reason.to_be_bytes());
        *hasher.finalize().as_bytes()
    }
}

pub(super) fn fallback_completion_update(old: Option<TerminalState>, new: TerminalState) -> bool {
    let Some(TerminalState::Fallback {
        output_root: old_root,
        reason: old_reason,
        notification_cursor: old_cursor,
    }) = old
    else {
        return false;
    };
    let TerminalState::Fallback {
        output_root: new_root,
        reason: new_reason,
        notification_cursor: new_cursor,
    } = new
    else {
        return false;
    };
    old_root == [0; 32]
        && new_root != [0; 32]
        && old_reason == new_reason
        && new_cursor >= old_cursor
}
