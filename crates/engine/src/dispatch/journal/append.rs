//! Appends typed dispatch records onto the durable journal.

use super::*;

impl DispatchJournal {
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
                super::super::record::DispatchRecordError::UnknownAttempt,
            ));
        };
        if attempt.current_fence != expected_fence {
            return Err(DispatchJournalError::Record(
                super::super::record::DispatchRecordError::PublicationMismatch,
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
                super::super::record::DispatchRecordError::UnknownAttempt,
            ));
        };
        if attempt.current_fence != expected_fence {
            return Err(DispatchJournalError::Record(
                super::super::record::DispatchRecordError::PublicationMismatch,
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
