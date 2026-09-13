use std::sync::{Arc, Mutex};

/// One independently accounted resource envelope.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Envelope {
    /// User visible work and the local fallback reservation.
    Interactive,
    /// Predictive and freshness work that cannot delay interactive work.
    Background,
    /// Input/output transfer and decode admission.
    Transfer,
    /// Physical arrangement/pack maintenance.
    Compaction,
}

/// Typed CPU, memory, network, and temporary-storage credits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceVector {
    /// CPU time credits in implementation-defined milliseconds.
    pub cpu_millis: u64,
    /// Mutable scratch/memory credits in bytes.
    pub memory_bytes: u64,
    /// Network transfer credits in bytes.
    pub network_bytes: u64,
    /// Durable temporary-storage credits in bytes.
    pub storage_bytes: u64,
}

impl ResourceVector {
    /// Empty multidimensional resource demand.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            cpu_millis: 0,
            memory_bytes: 0,
            network_bytes: 0,
            storage_bytes: 0,
        }
    }

    fn checked_add(self, other: Self) -> Result<Self, AdmissionError> {
        Ok(Self {
            cpu_millis: self
                .cpu_millis
                .checked_add(other.cpu_millis)
                .ok_or(AdmissionError::Overflow)?,
            memory_bytes: self
                .memory_bytes
                .checked_add(other.memory_bytes)
                .ok_or(AdmissionError::Overflow)?,
            network_bytes: self
                .network_bytes
                .checked_add(other.network_bytes)
                .ok_or(AdmissionError::Overflow)?,
            storage_bytes: self
                .storage_bytes
                .checked_add(other.storage_bytes)
                .ok_or(AdmissionError::Overflow)?,
        })
    }

    fn checked_sub(self, other: Self) -> Self {
        // Admission invariants guarantee each used dimension is no greater
        // than its budget.  The explicit checked transition keeps a
        // corrupted state fail closed without silently masking ownership
        // underflow with saturation.
        Self {
            cpu_millis: checked_remaining(self.cpu_millis, other.cpu_millis),
            memory_bytes: checked_remaining(self.memory_bytes, other.memory_bytes),
            network_bytes: checked_remaining(self.network_bytes, other.network_bytes),
            storage_bytes: checked_remaining(self.storage_bytes, other.storage_bytes),
        }
    }
}

/// A checked capacity for one envelope.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Budget {
    /// Number of admitted operation slots.
    pub operations: u64,
    /// Bytes reserved for scratch, transfer, or materialized output.
    pub bytes: u64,
    /// Number of duplicate pure attempts allowed.
    pub hedges: u64,
    /// Additional typed CPU, memory, network, and storage credits.
    pub resources: ResourceVector,
}

impl Budget {
    /// Empty capacity.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            operations: 0,
            bytes: 0,
            hedges: 0,
            resources: ResourceVector::zero(),
        }
    }

    fn checked_add(self, other: Self) -> Result<Self, AdmissionError> {
        Ok(Self {
            operations: self
                .operations
                .checked_add(other.operations)
                .ok_or(AdmissionError::Overflow)?,
            bytes: self
                .bytes
                .checked_add(other.bytes)
                .ok_or(AdmissionError::Overflow)?,
            hedges: self
                .hedges
                .checked_add(other.hedges)
                .ok_or(AdmissionError::Overflow)?,
            resources: self.resources.checked_add(other.resources)?,
        })
    }

    fn checked_sub(self, other: Self) -> Self {
        Self {
            operations: checked_remaining(self.operations, other.operations),
            bytes: checked_remaining(self.bytes, other.bytes),
            hedges: checked_remaining(self.hedges, other.hedges),
            resources: self.resources.checked_sub(other.resources),
        }
    }
}

/// Capacity for all execution envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeBudgets {
    /// Interactive capacity.
    pub interactive: Budget,
    /// Background capacity.
    pub background: Budget,
    /// Transfer capacity.
    pub transfer: Budget,
    /// Compaction capacity.
    pub compaction: Budget,
}

impl EnvelopeBudgets {
    /// Creates a set of capacities with one shared default for all envelopes.
    #[must_use]
    pub const fn uniform(budget: Budget) -> Self {
        Self {
            interactive: budget,
            background: budget,
            transfer: budget,
            compaction: budget,
        }
    }

    /// Returns the capacity selected by an envelope.
    #[must_use]
    pub const fn for_envelope(self, envelope: Envelope) -> Budget {
        match envelope {
            Envelope::Interactive => self.interactive,
            Envelope::Background => self.background,
            Envelope::Transfer => self.transfer,
            Envelope::Compaction => self.compaction,
        }
    }
}

impl Default for EnvelopeBudgets {
    fn default() -> Self {
        Self::uniform(Budget::zero())
    }
}

/// One admission request. `try_admit` places it in the interactive envelope;
/// use `try_admit_in` when the caller owns a different policy lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionRequest {
    /// Operation slots required by this phase.
    pub operations: u64,
    /// Bytes retained or transferred by this phase.
    pub bytes: u64,
    /// Whether this reservation is a duplicate pure attempt.
    pub hedge: bool,
    /// Additional typed resource demand.
    pub resources: ResourceVector,
}

impl AdmissionRequest {
    /// Constructs a non-hedged request.
    #[must_use]
    pub const fn new(operations: u64, bytes: u64) -> Self {
        Self {
            operations,
            bytes,
            hedge: false,
            resources: ResourceVector::zero(),
        }
    }

    /// Marks this request as a pure-work hedge.
    #[must_use]
    pub const fn hedged(mut self) -> Self {
        self.hedge = true;
        self
    }

    /// Sets the hedge bit from a policy decision.
    #[must_use]
    pub const fn hedged_if(mut self, hedge: bool) -> Self {
        self.hedge = hedge;
        self
    }

    /// Adds multidimensional resource credits to this request.
    #[must_use]
    pub const fn with_resources(mut self, resources: ResourceVector) -> Self {
        self.resources = resources;
        self
    }

    /// Builds a reservation request for one immutable store pack.
    ///
    /// The pack's encoded length is charged both to the route byte envelope
    /// and to temporary storage credits. Callers can add CPU or memory demand
    /// with [`Self::with_resources`] before admitting it.
    ///
    /// # Errors
    ///
    /// Returns [`AdmissionError::Overflow`] when the platform length cannot be
    /// represented by the checked scheduler counters.
    pub fn for_store_pack(
        operations: u64,
        pack: &backend_store::Pack,
    ) -> Result<Self, AdmissionError> {
        let bytes = u64::try_from(pack.bytes().len()).map_err(|_| AdmissionError::Overflow)?;
        Ok(Self::new(operations, bytes).with_resources(ResourceVector {
            storage_bytes: bytes,
            ..ResourceVector::zero()
        }))
    }

    fn amount(self) -> Budget {
        Budget {
            operations: self.operations,
            bytes: self.bytes,
            hedges: u64::from(self.hedge),
            resources: self.resources,
        }
    }
}

/// Failure to reserve a resource envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    /// One request can never fit in the selected envelope, even when it is idle.
    Impossible(ImpossibleAdmission),
    /// The operation envelope has no remaining capacity.
    Operations,
    /// The byte envelope has no remaining capacity.
    Bytes,
    /// The hedge envelope has no remaining capacity.
    Hedges,
    /// One typed resource dimension has no remaining capacity.
    Resources,
    /// A checked counter operation overflowed.
    Overflow,
}

/// The exact capacity axis that makes one request permanently inadmissible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionDimension {
    /// Concurrent operation slots.
    Operations,
    /// Retained payload bytes.
    Bytes,
    /// Duplicate pure attempts.
    Hedges,
    /// CPU-time credits.
    CpuMillis,
    /// Mutable memory credits.
    MemoryBytes,
    /// Network-transfer credits.
    NetworkBytes,
    /// Temporary durable-storage credits.
    StorageBytes,
}

/// Proof that retrying an unchanged request can never succeed in this envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImpossibleAdmission {
    /// Resource axis that exceeded total capacity.
    pub dimension: AdmissionDimension,
    /// Amount requested by the single operation.
    pub requested: u64,
    /// Total configured capacity, independent of current use.
    pub capacity: u64,
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "admission error: {self:?}")
    }
}

impl std::error::Error for AdmissionError {}

struct State {
    budgets: EnvelopeBudgets,
    used: EnvelopeBudgets,
}

/// Thread-safe, nonblocking admission across the four scheduler envelopes.
pub struct Admission {
    state: Arc<Mutex<State>>,
}

impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Admission")
            .field("budgets", &self.budgets())
            .finish()
    }
}

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

fn reject_impossible(amount: Budget, capacity: Budget) -> Result<(), AdmissionError> {
    let dimensions = [
        (
            AdmissionDimension::Operations,
            amount.operations,
            capacity.operations,
        ),
        (AdmissionDimension::Bytes, amount.bytes, capacity.bytes),
        (AdmissionDimension::Hedges, amount.hedges, capacity.hedges),
        (
            AdmissionDimension::CpuMillis,
            amount.resources.cpu_millis,
            capacity.resources.cpu_millis,
        ),
        (
            AdmissionDimension::MemoryBytes,
            amount.resources.memory_bytes,
            capacity.resources.memory_bytes,
        ),
        (
            AdmissionDimension::NetworkBytes,
            amount.resources.network_bytes,
            capacity.resources.network_bytes,
        ),
        (
            AdmissionDimension::StorageBytes,
            amount.resources.storage_bytes,
            capacity.resources.storage_bytes,
        ),
    ];
    if let Some((dimension, requested, capacity)) = dimensions
        .into_iter()
        .find(|(_, requested, capacity)| requested > capacity)
    {
        return Err(AdmissionError::Impossible(ImpossibleAdmission {
            dimension,
            requested,
            capacity,
        }));
    }
    Ok(())
}

fn set_envelope(all: &mut EnvelopeBudgets, envelope: Envelope, value: Budget) {
    match envelope {
        Envelope::Interactive => all.interactive = value,
        Envelope::Background => all.background = value,
        Envelope::Transfer => all.transfer = value,
        Envelope::Compaction => all.compaction = value,
    }
}

fn checked_remaining(available: u64, held: u64) -> u64 {
    // A reservation is affine, so a valid release always fits.  `Drop` cannot
    // report an error if a corrupted/foreign state is observed; fail closed
    // to zero rather than using saturating arithmetic or an aborting assertion.
    available.saturating_sub(held)
}

/// Affine reservation released on drop or explicit consumption.
#[must_use = "a reservation must be held until the admitted phase finishes"]
pub struct Reservation {
    state: Arc<Mutex<State>>,
    envelope: Envelope,
    held: Budget,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("envelope", &self.envelope)
            .field("held", &self.held)
            .finish_non_exhaustive()
    }
}

impl Reservation {
    /// Returns the resource amount owned by this reservation.
    #[must_use]
    pub const fn held(&self) -> Budget {
        self.held
    }

    /// Returns the envelope charged by this reservation.
    #[must_use]
    pub const fn envelope(&self) -> Envelope {
        self.envelope
    }

    /// Consumes the guard and releases its capacity immediately.
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let used = state.used.for_envelope(self.envelope);
        set_envelope(&mut state.used, self.envelope, used.checked_sub(self.held));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Admission, AdmissionDimension, AdmissionError, AdmissionRequest, Budget,
        ImpossibleAdmission, ResourceVector,
    };

    #[test]
    fn impossible_request_is_distinct_from_transient_pressure() -> Result<(), AdmissionError> {
        let admission = Admission::new(Budget {
            operations: 1,
            bytes: 8,
            hedges: 0,
            resources: ResourceVector {
                memory_bytes: 4,
                ..ResourceVector::zero()
            },
        });
        assert!(matches!(
            admission.try_admit(AdmissionRequest::new(1, 9)),
            Err(AdmissionError::Impossible(ImpossibleAdmission {
                dimension: AdmissionDimension::Bytes,
                requested: 9,
                capacity: 8,
            }))
        ));
        assert_eq!(admission.available().bytes, 8);

        let held = admission.try_admit(AdmissionRequest::new(1, 8))?;
        assert!(matches!(
            admission.try_admit(AdmissionRequest::new(1, 1)),
            Err(AdmissionError::Operations)
        ));
        drop(held);

        assert!(matches!(
            admission.try_admit(AdmissionRequest::new(1, 1).with_resources(ResourceVector {
                memory_bytes: 5,
                ..ResourceVector::zero()
            })),
            Err(AdmissionError::Impossible(ImpossibleAdmission {
                dimension: AdmissionDimension::MemoryBytes,
                requested: 5,
                capacity: 4,
            }))
        ));
        Ok(())
    }
}
