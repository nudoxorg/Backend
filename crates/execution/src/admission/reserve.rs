//! Admission reservation and capacity accounting methods.

use super::*;

impl Admission {
    /// Creates an admission controller with interactive capacity. Other lanes
    /// start closed until configured with [`Self::with_envelopes`].
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self::with_envelopes(EnvelopeBudgets {
            interactive: budget,
            background: Budget::zero(),
            transfer: Budget::zero(),
            compaction: Budget::zero(),
        })
    }

    /// Creates an admission controller with independent policy envelopes.
    #[must_use]
    pub fn with_envelopes(budgets: EnvelopeBudgets) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                budgets,
                used: EnvelopeBudgets::default(),
            })),
        }
    }

    /// Returns the configured capacity of every envelope.
    #[must_use]
    pub fn budgets(&self) -> EnvelopeBudgets {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.budgets
    }

    /// Attempts interactive admission.
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when the requested counters do not fit in
    /// the interactive envelope or checked arithmetic overflows.
    pub fn try_admit(&self, request: AdmissionRequest) -> Result<Reservation, AdmissionError> {
        self.try_admit_in(Envelope::Interactive, request)
    }

    /// Attempts admission in one named envelope without waiting. A failed
    /// request changes no counters.
    /// # Errors
    ///
    /// Returns [`AdmissionError`] when the requested counters do not fit in
    /// the selected envelope or checked arithmetic overflows. A failed
    /// request leaves all envelope counters unchanged.
    pub fn try_admit_in(
        &self,
        envelope: Envelope,
        request: AdmissionRequest,
    ) -> Result<Reservation, AdmissionError> {
        let amount = request.amount();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let budget = state.budgets.for_envelope(envelope);
        reject_impossible(amount, budget)?;
        let used = state.used.for_envelope(envelope);
        let next = used.checked_add(amount)?;
        if next.operations > budget.operations {
            return Err(AdmissionError::Operations);
        }
        if next.bytes > budget.bytes {
            return Err(AdmissionError::Bytes);
        }
        if next.hedges > budget.hedges {
            return Err(AdmissionError::Hedges);
        }
        if next.resources.cpu_millis > budget.resources.cpu_millis
            || next.resources.memory_bytes > budget.resources.memory_bytes
            || next.resources.network_bytes > budget.resources.network_bytes
            || next.resources.storage_bytes > budget.resources.storage_bytes
        {
            return Err(AdmissionError::Resources);
        }
        set_envelope(&mut state.used, envelope, next);
        Ok(Reservation {
            state: Arc::clone(&self.state),
            envelope,
            held: amount,
        })
    }

    /// Returns remaining interactive capacity.
    #[must_use]
    pub fn available(&self) -> Budget {
        self.available_in(Envelope::Interactive)
    }

    /// Returns remaining capacity in one envelope.
    #[must_use]
    pub fn available_in(&self, envelope: Envelope) -> Budget {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .budgets
            .for_envelope(envelope)
            .checked_sub(state.used.for_envelope(envelope))
    }

    /// Returns remaining capacity in all envelopes.
    #[must_use]
    pub fn available_envelopes(&self) -> EnvelopeBudgets {
        EnvelopeBudgets {
            interactive: self.available_in(Envelope::Interactive),
            background: self.available_in(Envelope::Background),
            transfer: self.available_in(Envelope::Transfer),
            compaction: self.available_in(Envelope::Compaction),
        }
    }

    /// Returns a deterministic load-shed view. The reservation itself remains
    /// authoritative; this only avoids requests unlikely to fit.
    #[must_use]
    pub fn adaptive_budget(&self, load_per_mille: u16) -> Budget {
        self.adaptive_budget_in(Envelope::Interactive, load_per_mille)
    }

    /// Returns a load-shed view for one envelope using checked scaling.
    #[must_use]
    pub fn adaptive_budget_in(&self, envelope: Envelope, load_per_mille: u16) -> Budget {
        let available = self.available_in(envelope);
        let factor = if load_per_mille >= 900 {
            500
        } else if load_per_mille >= 750 {
            750
        } else {
            1_000
        };
        let scale = |value: u64| value.checked_mul(factor).map_or(0, |scaled| scaled / 1_000);
        Budget {
            operations: scale(available.operations),
            bytes: scale(available.bytes),
            hedges: scale(available.hedges),
            resources: ResourceVector {
                cpu_millis: scale(available.resources.cpu_millis),
                memory_bytes: scale(available.resources.memory_bytes),
                network_bytes: scale(available.resources.network_bytes),
                storage_bytes: scale(available.resources.storage_bytes),
            },
        }
    }
}
