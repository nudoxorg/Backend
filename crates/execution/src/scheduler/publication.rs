//! Fenced result publication and reusable output installation.

use super::engine::Scheduler;
use super::guard::Scheduled;
use super::outcome::{ScheduleError, ScheduleReceipt};
use crate::{
    AdmissionError, AttemptError, HedgeError, HedgeRace, HedgeSide, ResultReceipt, ReuseContext,
};
use backend_version::Relation;
use std::num::NonZeroU64;

impl Scheduler {
    /// Completes a scheduled route and accepts one typed result.  The generic
    /// lower API publishes an ephemeral receipt; a reusable entry requires an
    /// explicit semantic generation through
    /// [`Self::complete_with_reuse_policy`].
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when the receipt does not match the actual
    /// scheduled fence, fails output/attempt validation, or loses a hedge
    /// race. The schedule guard is consumed only after acceptance.
    pub fn complete<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        // Without an engine-issued semantic generation this generic lower
        // API can only publish a one-shot result. Reuse is selected through
        // `complete_with_reuse_policy` after semantic admission. Delegate to
        // the single validation/publication path so fence, attempt, and hedge
        // checks are not repeated.
        self.complete_ephemeral(scheduled, receipt, now)
    }

    /// Completes a validated route without installing it in the reusable
    /// output lookup. This is used when the upper semantic authority admitted
    /// only a one-shot coverage claim and did not retain an exact dependency
    /// manifest. The receipt still owns the canonical bytes for its caller,
    /// while a later request must execute again and revalidate its semantics.
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::complete`].
    pub fn complete_ephemeral<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        Self::check_scheduled_receipt(&scheduled, receipt)?;
        self.attempts
            .validate_for_scheduler(receipt, now)
            .map_err(ScheduleError::Attempt)?;
        if let Some(race) = &scheduled.race {
            Self::claim_race(race, HedgeSide::Local, receipt)?;
        }
        self.complete_accepted(scheduled, receipt, now, false)
    }

    /// Selects reusable or one-shot publication after all receipt checks. The
    /// policy is supplied by the upper semantic authority, so the lower
    /// scheduler never has to infer reuse from a digest-only claim.
    /// # Errors
    ///
    /// Returns the validation error selected by the chosen publication path.
    pub fn complete_with_reuse_policy<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
        retain_lookup: bool,
        dependency_generation: Option<NonZeroU64>,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        if retain_lookup {
            let dependency_generation =
                dependency_generation.ok_or(ScheduleError::ObservationMismatch)?;
            Self::check_scheduled_receipt(&scheduled, receipt)?;
            self.attempts
                .validate_for_scheduler(receipt, now)
                .map_err(ScheduleError::Attempt)?;
            if let Some(race) = &scheduled.race {
                Self::claim_race(race, HedgeSide::Local, receipt)?;
            }
            self.complete_accepted_with_generation(
                scheduled,
                receipt,
                now,
                true,
                Some(dependency_generation),
            )
        } else {
            self.complete_ephemeral(scheduled, receipt, now)
        }
    }

    /// Completes one side of a hedge after the caller has validated its worker
    /// receipt. The first valid side cancels the other side before publication.
    /// # Errors
    ///
    /// Returns [`ScheduleError`] when the receipt does not match the scheduled
    /// fence, fails output/attempt validation, or loses the first-valid hedge
    /// race. The schedule guard is consumed only after acceptance.
    pub fn complete_side<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        side: HedgeSide,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        // `complete_side_ephemeral` owns the side-specific validation and
        // publication sequence; keeping one path avoids doing the same
        // attempt/fence/hedge work twice on every completion.
        self.complete_side_ephemeral(scheduled, side, receipt, now)
    }

    /// Completes one side of a hedge without installing a reusable lookup
    /// entry. This preserves the affine hedge winner semantics for a
    /// manifest-less, one-shot semantic claim.
    /// # Errors
    ///
    /// Returns the same validation errors as [`Self::complete_side`].
    pub fn complete_side_ephemeral<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        side: HedgeSide,
        receipt: &ResultReceipt<R>,
        now: u64,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        Self::check_scheduled_receipt(&scheduled, receipt)?;
        self.attempts
            .validate_for_scheduler(receipt, now)
            .map_err(ScheduleError::Attempt)?;
        if let Some(race) = &scheduled.race {
            Self::claim_race(race, side, receipt)?;
        } else if side != HedgeSide::Local {
            return Err(ScheduleError::Hedge(HedgeError::AlreadyWon));
        }
        self.complete_accepted(scheduled, receipt, now, false)
    }

    /// Selects reusable or one-shot publication for a hedge winner.
    /// # Errors
    ///
    /// Returns the validation error selected by the chosen publication path.
    pub fn complete_side_with_reuse_policy<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        side: HedgeSide,
        receipt: &ResultReceipt<R>,
        now: u64,
        retain_lookup: bool,
        dependency_generation: Option<NonZeroU64>,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        if retain_lookup {
            let dependency_generation =
                dependency_generation.ok_or(ScheduleError::ObservationMismatch)?;
            Self::check_scheduled_receipt(&scheduled, receipt)?;
            self.attempts
                .validate_for_scheduler(receipt, now)
                .map_err(ScheduleError::Attempt)?;
            if let Some(race) = &scheduled.race {
                Self::claim_race(race, side, receipt)?;
            } else if side != HedgeSide::Local {
                return Err(ScheduleError::Hedge(HedgeError::AlreadyWon));
            }
            self.complete_accepted_with_generation(
                scheduled,
                receipt,
                now,
                true,
                Some(dependency_generation),
            )
        } else {
            self.complete_side_ephemeral(scheduled, side, receipt, now)
        }
    }

    pub(super) fn claim_race<R: Relation>(
        race: &HedgeRace,
        side: HedgeSide,
        receipt: &ResultReceipt<R>,
    ) -> Result<(), ScheduleError> {
        let output = receipt.result();
        let admission = receipt.output_admission();
        if race.confirm_winner(side, output) {
            drop(admission);
            return Ok(());
        }
        race.try_win(side, admission)
            .map(|_| ())
            .map_err(ScheduleError::Hedge)
    }

    pub(super) fn check_scheduled_receipt<R: Relation>(
        scheduled: &Scheduled<R>,
        receipt: &ResultReceipt<R>,
    ) -> Result<(), ScheduleError> {
        if receipt.key() != scheduled.key() {
            return Err(ScheduleError::KeyMismatch);
        }
        if receipt.ordinal() != scheduled.lease.ordinal()
            || receipt.fence() != scheduled.lease.fence()
        {
            return Err(ScheduleError::Attempt(AttemptError::Stale));
        }
        Ok(())
    }

    fn complete_accepted<R: Relation>(
        &self,
        scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
        retain_lookup: bool,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        self.complete_accepted_with_generation(scheduled, receipt, now, retain_lookup, None)
    }

    fn complete_accepted_with_generation<R: Relation>(
        &self,
        mut scheduled: Scheduled<R>,
        receipt: &ResultReceipt<R>,
        now: u64,
        retain_lookup: bool,
        dependency_generation: Option<NonZeroU64>,
    ) -> Result<ScheduleReceipt, ScheduleError> {
        // The interner leader is the only operation below that could report a
        // coalescing failure after attempt acceptance. Preflight it while the
        // attempt is still live; its affine ownership makes the subsequent
        // terminal transition infallible.
        if retain_lookup
            && !self
                .lookup
                .can_retain_capacity(receipt.canonical_bytes_capacity())
        {
            return Err(ScheduleError::Admission(AdmissionError::Bytes));
        }
        if let Some(handle) = scheduled.interned.as_ref() {
            handle
                .prepare_complete(receipt.result())
                .map_err(|_| ScheduleError::Coalesced)?;
        }
        let output = self
            .attempts
            .accept(receipt, now)
            .map_err(ScheduleError::Attempt)?;
        scheduled.lease.set_state(crate::AttemptState::Accepted);
        let key = scheduled.key();
        let fence = scheduled.lease.fence();
        let ordinal = scheduled.lease.ordinal();
        let decision = scheduled.decision;
        self.supervisor.completed();
        self.telemetry.record_with(|| crate::Observation {
            family: crate::MetricFamily::Lifecycle,
            outcome: crate::MetricOutcome::Completed,
            latency: std::time::Duration::ZERO,
            units: u64::try_from(receipt.canonical_bytes_capacity()).unwrap_or(u64::MAX),
        });
        let reusable = dependency_generation.map(|generation| {
            let reuse_context = ReuseContext::from_receipt(receipt, generation);
            crate::ReusableOutput::from_parts(reuse_context, output, receipt.canonical_bytes_arc())
        });
        if let Some(handle) = scheduled.interned.take() {
            // `prepare_complete` above validated the only fallible state
            // transition. Do not turn an invariant violation into an
            // ordinary error after the attempt has become terminal. The
            // retained owner is installed before followers can observe the
            // completed interner entry, preserving pointer identity with the
            // scheduler lookup and returned receipt.
            let _ = handle
                .complete_prepared_with_reusable(receipt.output_admission(), reusable.clone());
        }
        let evicted_keys = if retain_lookup {
            reusable
                .as_ref()
                .map_or_else(Vec::new, |reusable| self.lookup.insert(reusable.clone()))
        } else {
            Vec::new()
        };
        drop(scheduled);
        Ok(ScheduleReceipt {
            key,
            output,
            reusable,
            canonical_bytes: receipt.canonical_bytes_arc(),
            decision,
            fence,
            ordinal,
            evicted_keys,
        })
    }
}
