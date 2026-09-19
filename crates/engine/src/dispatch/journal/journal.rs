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

    /// Persists an admitted remote intent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn admit(
        &self,
        intent: RemoteAttemptIntent,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(&DispatchRecord::Admitted { intent }, Boundary::Prepare)
    }

    /// Persists transfer/checkpoint progress.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn record_transfer(
        &self,
        key: DispatchAttemptKey,
        progress: TransferCheckpointRef,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Transfer { key, progress },
            Boundary::Transfer,
        )
    }

    /// Persists an accepted result proof before owner publication begins.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn accept_result(
        &self,
        key: DispatchAttemptKey,
        proof: AcceptedResultProof,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Accepted { key, proof },
            Boundary::OutputAdmission,
        )
    }

    /// Persists the owner publication acknowledgement.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acknowledge_publication(
        &self,
        key: DispatchAttemptKey,
        ack: PublicationAck,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Published { key, ack },
            Boundary::JournalPublished,
        )
    }

    /// Atomically acknowledges publication only when the retained attempt
    /// still carries the caller's exact generation fence.
    ///
    /// A point lookup followed by [`Self::acknowledge_publication`] would let
    /// a takeover race acknowledge a newer owner generation.  This operation
    /// performs the fence comparison, replay validation, journal append, and
    /// in-memory fold while holding one state owner lock.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acknowledge_publication_if(
        &self,
        key: DispatchAttemptKey,
        expected_fence: [u8; 32],
        ack: PublicationAck,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        let record = DispatchRecord::Published { key, ack };
        self.limits.accepts(&record)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        let Some(attempt) = state.attempts.get(&key) else {
            return Err(DispatchJournalError::Record(
                super::record::DispatchRecordError::UnknownAttempt,
            ));
        };
        if attempt.current_fence != expected_fence {
            return Err(DispatchJournalError::Record(
                super::record::DispatchRecordError::PublicationMismatch,
            ));
        }
        state.validate(&record)?;
        self.faults.trip(Boundary::JournalPublished)?;
        let receipt = self.journal.append(&record)?;
        state.apply(&record)?;
        Ok(receipt)
    }

    /// Atomically compares the current attempt fence and commits the owner
    /// publication acknowledgement together with its exact notification
    /// position.
    ///
    /// Recovered publication has two coupled durable facts: the selected
    /// owner root and the cursor from which waiters/subscribers resume. A
    /// separate `Published` followed by `Cursor` append leaves a crash window
    /// in which the owner is published but the notification position is lost.
    /// The fused record keeps those facts in one frame, while the state lock
    /// makes a takeover unable to interleave between the fence comparison and
    /// append.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn acknowledge_publication_with_cursor_if(
        &self,
        key: DispatchAttemptKey,
        expected_fence: [u8; 32],
        ack: PublicationAck,
        position: NotificationCursor,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        let record = DispatchRecord::PublishedWithCursor {
            key,
            ack,
            cursor: position,
        };
        self.limits.accepts(&record)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        let Some(attempt) = state.attempts.get(&key) else {
            return Err(DispatchJournalError::Record(
                super::record::DispatchRecordError::UnknownAttempt,
            ));
        };
        if attempt.current_fence != expected_fence {
            return Err(DispatchJournalError::Record(
                super::record::DispatchRecordError::PublicationMismatch,
            ));
        }
        state.validate(&record)?;
        self.faults.trip(Boundary::JournalPublished)?;
        let receipt = self.journal.append(&record)?;
        state.apply(&record)?;
        // Keep the existing notification fault seam observable for crash
        // tests even though the cursor now shares the publication frame.
        self.faults.trip(Boundary::Notification)?;
        Ok(receipt)
    }

    /// Records that owner publication began after the accepted proof was
    /// durably appended.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn begin_publication(
        &self,
        key: DispatchAttemptKey,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::PublicationPending { key },
            Boundary::JournalPrepared,
        )
    }

    /// Persists a cancellation terminal state.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn cancel(
        &self,
        key: DispatchAttemptKey,
        reason: u16,
        notification_cursor: u64,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Terminal {
                key,
                terminal: TerminalState::Cancelled {
                    reason,
                    notification_cursor,
                },
            },
            Boundary::OwnerFence,
        )
    }

    /// Persists local fallback terminal state.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fallback(
        &self,
        key: DispatchAttemptKey,
        output_root: [u8; 32],
        reason: u16,
        notification_cursor: u64,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Terminal {
                key,
                terminal: TerminalState::Fallback {
                    output_root,
                    reason,
                    notification_cursor,
                },
            },
            Boundary::OwnerFence,
        )
    }

    /// Persists a waiter/subscription notification cursor.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn update_cursor(
        &self,
        key: DispatchAttemptKey,
        position: NotificationCursor,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Cursor {
                cursor: DispatchCursor { key, position },
            },
            Boundary::Notification,
        )
    }

    /// Persists a replacement fence during takeover.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn fence(
        &self,
        key: DispatchAttemptKey,
        owner_epoch: u64,
        fence: [u8; 32],
        reason: u16,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.append(
            &DispatchRecord::Fenced {
                key,
                owner_epoch,
                fence,
                reason,
            },
            Boundary::OwnerFence,
        )
    }

    /// Appends a prevalidated transition using the boundary implied by its
    /// record tag. This is the low-level typed façade used by integration
    /// owners that build records from their own protocol adapters.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn append_record(
        &self,
        record: &DispatchRecord,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        let boundary = boundary_for(record);
        self.append(record, boundary)
    }

    /// Retries an ambiguous append with the exact canonical record.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn retry(
        &self,
        receipt: JournalReceipt<DispatchLog>,
        record: &DispatchRecord,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.limits.accepts(record)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        state.validate(record)?;
        let journal_receipt = self.journal.retry(receipt, record)?;
        state.apply(record)?;
        self.faults.trip(Boundary::JournalFlush)?;
        Ok(journal_receipt)
    }

    fn append(
        &self,
        record: &DispatchRecord,
        boundary: Boundary,
    ) -> Result<JournalReceipt<DispatchLog>, DispatchJournalError> {
        self.limits.accepts(record)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| DispatchJournalError::Journal(JournalError::Corrupt("poisoned replay")))?;
        state.validate(record)?;

        // The lifecycle boundary models the point immediately before the
        // frame is written. JournalFlush is always tripped after append and
        // replay application, simulating a crash after fsync so recovery can
        // observe the durable transition even when the caller supplied a
        // more specific pre-append boundary.
        if boundary != Boundary::JournalFlush {
            self.faults.trip(boundary)?;
        }
        let receipt = self.journal.append(record)?;
        state.apply(record)?;
        self.faults.trip(Boundary::JournalFlush)?;
        Ok(receipt)
    }
}

fn boundary_for(record: &DispatchRecord) -> Boundary {
    match record {
        DispatchRecord::Admitted { .. } => Boundary::Prepare,
        DispatchRecord::Transfer { .. } => Boundary::Transfer,
        DispatchRecord::Accepted { .. } => Boundary::OutputAdmission,
        DispatchRecord::PublicationPending { .. } => Boundary::JournalPrepared,
        DispatchRecord::Published { .. } | DispatchRecord::PublishedWithCursor { .. } => {
            Boundary::JournalPublished
        }
        DispatchRecord::Terminal { .. } | DispatchRecord::Fenced { .. } => Boundary::OwnerFence,
        DispatchRecord::Cursor { .. } => Boundary::Notification,
    }
}
