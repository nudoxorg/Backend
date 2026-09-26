//! Durable dispatch journal storage and append façade.

use super::error::DispatchJournalError;
use super::fold::{ReplayState, fold_frame};
use super::model::{DispatchRecovery, recovery_from_state};
use super::record::{
    AcceptedResultProof, DispatchAttemptKey, DispatchCursor, DispatchJournalLimits, DispatchLog,
    DispatchRecord, NotificationCursor, PublicationAck, RemoteAttemptIntent, TerminalState,
    TransferCheckpointRef,
};
use crate::fault::{Boundary, Faults};
use crate::journal::{HashChainJournal, JournalError, JournalReceipt, JournalScan};
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

mod append;

/// Durable dispatch journal and typed replay state.
pub struct DispatchJournal {
    pub(super) journal: HashChainJournal<DispatchLog>,
    pub(super) state: Mutex<ReplayState>,
    pub(super) limits: DispatchJournalLimits,
    pub(super) faults: Arc<Faults>,
}

impl fmt::Debug for DispatchJournal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DispatchJournal")
            .field("path", &self.journal.path())
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl DispatchJournal {
    /// Opens and streams a bounded dispatch journal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open(
        path: impl AsRef<Path>,
        limits: DispatchJournalLimits,
    ) -> Result<(Self, DispatchRecovery), DispatchJournalError> {
        Self::open_with_faults(path, limits, Arc::new(Faults::default()))
    }

    /// Opens a journal with an externally controlled fault injector.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_with_faults(
        path: impl AsRef<Path>,
        limits: DispatchJournalLimits,
        faults: Arc<Faults>,
    ) -> Result<(Self, DispatchRecovery), DispatchJournalError> {
        limits.validate()?;
        faults.trip(Boundary::Recovery)?;
        let mut replay = ReplayState::new(limits);
        let (journal, scan) = HashChainJournal::<DispatchLog>::open_streaming_with(
            path,
            limits.journal_limits(),
            |frame| fold_frame(&mut replay, &frame),
        )?;
        let recovery = recovery_from_state(replay.attempts.clone(), &scan);
        Ok((
            Self {
                journal,
                state: Mutex::new(replay),
                limits,
                faults,
            },
            recovery,
        ))
    }

    /// Returns the fault controller used by transition boundaries.
    #[must_use]
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }

    /// Returns the backing journal path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.journal.path()
    }

    /// Replays the current file into a bounded state fold.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn recover(&self) -> Result<DispatchRecovery, DispatchJournalError> {
        self.faults.trip(Boundary::Recovery)?;
        let mut replay = ReplayState::new(self.limits);
        let scan = self
            .journal
            .scan_stream(self.limits.journal_limits(), |frame| {
                fold_frame(&mut replay, &frame)
            })?;
        let recovery = recovery_from_state(replay.attempts.clone(), &scan);
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        *state = replay;
        Ok(recovery)
    }

    /// Returns a bounded snapshot of the in-memory replay state.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn snapshot(&self) -> Result<DispatchRecovery, DispatchJournalError> {
        let state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        let scan = JournalScan {
            frames_scanned: 0,
            bytes_scanned: 0,
            peak_payload_bytes: 0,
            valid_offset: 0,
            last_sequence: None,
            chain: crate::journal::ChainHash::genesis(),
            truncated_tail: false,
        };
        Ok(recovery_from_state(state.attempts.clone(), &scan))
    }

    /// Looks up one recovered attempt without cloning the bounded replay map.
    ///
    /// Point recovery paths use this operation when they already know the
    /// exact work/attempt key.  The returned attempt is the only retained
    /// value cloned, so an unrelated attempt flood cannot turn one fallback
    /// lookup into an O(number-of-attempts) allocation.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn point(
        &self,
        key: DispatchAttemptKey,
    ) -> Result<Option<super::model::RecoveredAttempt>, DispatchJournalError> {
        let state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        Ok(state.attempts.get(&key).cloned())
    }

    /// Looks up one recovered attempt only when its generation fence still
    /// matches. This keeps point recovery bound to the same owner generation
    /// that selected the result; a takeover cannot turn a stale observation
    /// into a publication attempt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn point_if(
        &self,
        key: DispatchAttemptKey,
        expected_fence: [u8; 32],
    ) -> Result<Option<super::model::RecoveredAttempt>, DispatchJournalError> {
        let state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        let Some(attempt) = state.attempts.get(&key) else {
            return Ok(None);
        };
        if attempt.current_fence != expected_fence {
            return Err(DispatchJournalError::Record(
                super::record::DispatchRecordError::PublicationMismatch,
            ));
        }
        Ok(Some(attempt.clone()))
    }

    /// Returns the latest durable revocation and notification observations.
    ///
    /// This is deliberately a crate-private observation tuple. It cannot be
    /// passed to restart directly: the workspace owner must combine it with
    /// its checked root and live lease fence through `restart_authority`.
    pub(crate) fn authority_observation(&self) -> (u64, u64) {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let revocation_version = state
            .attempts
            .values()
            .map(|attempt| attempt.intent.revocation_version)
            .max()
            .unwrap_or(0);
        let notification_cursor = state
            .attempts
            .values()
            .flat_map(|attempt| {
                let cursor = attempt
                    .cursor
                    .as_ref()
                    .map_or(0, NotificationCursor::watermark);
                let terminal = attempt.terminal.map_or(0, |terminal| match terminal {
                    TerminalState::Cancelled {
                        notification_cursor,
                        ..
                    }
                    | TerminalState::Fallback {
                        notification_cursor,
                        ..
                    } => notification_cursor,
                });
                [cursor, terminal]
            })
            .max()
            .unwrap_or(0);
        (revocation_version, notification_cursor)
    }
}
