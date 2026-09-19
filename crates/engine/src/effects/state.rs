//! Small, allocation-free phase values and persistence/sink seams.

use super::spec::{EffectKey, EffectSpec};
use crate::fault::InjectedCrash;
use crate::journal::{JournalDomain, JournalError, JournalReceipt};
use blake3::Hasher;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;

/// Durable effect phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectPhase {
    /// The intent is durable and no call has started.
    Prepared,
    /// The pre-call fence is durable and the sink call is in flight.
    Executing,
    /// The sink outcome requires idempotent reconciliation.
    Ambiguous,
    /// A receipt was validated and durably recorded.
    Confirmed,
}

/// Why an executing effect became ambiguous.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AmbiguousReason {
    /// The sink was unavailable before a definitive response.
    Timeout,
    /// Recovery found a call fence without a terminal record.
    CrashAfterCall,
    /// The sink explicitly returned an unknown outcome.
    UnknownOutcome,
    /// The sink receipt failed local binding validation.
    InvalidReceipt,
}

/// Opaque execution fence for an effect key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EffectFence(pub(crate) [u8; 32]);

impl EffectFence {
    /// Returns bytes for a transport or journal record.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

pub(crate) fn fence(key: EffectKey, ordinal: u64) -> EffectFence {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.engine.effect.fence.v2\0");
    hasher.update(key.as_bytes());
    hasher.update(&ordinal.to_be_bytes());
    EffectFence(*hasher.finalize().as_bytes())
}

/// State carried by a durable effect entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectState<R> {
    /// Durable intent, no sink call yet.
    Prepared,
    /// Sink call owns this fence.
    Executing {
        /// Per-attempt idempotency fence.
        fence: EffectFence,
        /// Monotonic attempt ordinal.
        ordinal: u64,
    },
    /// Outcome needs reconciliation.
    Ambiguous {
        /// Fence of the external call whose outcome is unknown.
        fence: EffectFence,
        /// Monotonic attempt ordinal.
        ordinal: u64,
        /// Why the result could not be confirmed.
        reason: AmbiguousReason,
    },
    /// Sink receipt has been validated and durably recorded.
    Confirmed {
        /// Exact receipt returned by the sink.
        receipt: R,
    },
}

impl<R> EffectState<R> {
    /// Returns the phase without exposing receipt details.
    #[must_use]
    pub const fn phase(&self) -> EffectPhase {
        match self {
            Self::Prepared => EffectPhase::Prepared,
            Self::Executing { .. } => EffectPhase::Executing,
            Self::Ambiguous { .. } => EffectPhase::Ambiguous,
            Self::Confirmed { .. } => EffectPhase::Confirmed,
        }
    }
    /// Returns the persisted fence, when this state owns an external call.
    #[must_use]
    pub const fn fence(&self) -> Option<EffectFence> {
        match self {
            Self::Executing { fence, .. } | Self::Ambiguous { fence, .. } => Some(*fence),
            Self::Prepared | Self::Confirmed { .. } => None,
        }
    }
    /// Returns the external call ordinal, when one has been attempted.
    #[must_use]
    pub const fn ordinal(&self) -> Option<u64> {
        match self {
            Self::Executing { ordinal, .. } | Self::Ambiguous { ordinal, .. } => Some(*ordinal),
            Self::Prepared | Self::Confirmed { .. } => None,
        }
    }
    /// Returns the ambiguity reason, when reconciliation is required.
    #[must_use]
    pub const fn ambiguous_reason(&self) -> Option<AmbiguousReason> {
        match self {
            Self::Ambiguous { reason, .. } => Some(*reason),
            Self::Prepared | Self::Executing { .. } | Self::Confirmed { .. } => None,
        }
    }
}

/// Sink call result. `Unknown` always enters reconciliation.
pub enum SinkApply<R> {
    /// The sink accepted the key and returned a receipt.
    Confirmed(R),
    /// The sink may have applied the key but gave no receipt.
    Unknown,
}

/// Reconciliation observation for an idempotency key.
pub enum SinkObservation<R> {
    /// The sink has applied the key and can return its receipt.
    Applied(R),
    /// The sink has no record of the key.
    Absent,
    /// The sink cannot determine the key's outcome yet.
    Unknown,
}

impl<R: fmt::Debug> fmt::Debug for SinkApply<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Confirmed(receipt) => f.debug_tuple("Confirmed").field(receipt).finish(),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}
impl<R: fmt::Debug> fmt::Debug for SinkObservation<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Applied(receipt) => f.debug_tuple("Applied").field(receipt).finish(),
            Self::Absent => f.write_str("Absent"),
            Self::Unknown => f.write_str("Unknown"),
        }
    }
}

/// External effect endpoint. Implementations must use `key` for idempotency.
pub trait EffectSink<E: EffectSpec> {
    /// Applies a request under the supplied deterministic key.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn apply(
        &mut self,
        key: EffectKey,
        request: E::Request,
    ) -> Result<SinkApply<E::Receipt>, SinkError>;
    /// Borrowed fast path; the default preserves compatibility for old sinks.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn apply_borrowed(
        &mut self,
        key: EffectKey,
        request: &E::Request,
    ) -> Result<SinkApply<E::Receipt>, SinkError> {
        self.apply(key, request.clone())
    }
    /// Reconciles an outcome after a lost response or restart.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn reconcile(&mut self, key: EffectKey) -> Result<SinkObservation<E::Receipt>, SinkError>;
}

/// Failure at the sink boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SinkError {
    /// The endpoint is unavailable; the effect remains ambiguous.
    Unavailable,
    /// The endpoint rejected the request.
    Rejected,
    /// The endpoint could not answer reconciliation.
    Unknown,
}
impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "effect sink error: {self:?}")
    }
}
impl std::error::Error for SinkError {}

/// Journal seam. Each method must append and sync before returning.
pub trait EffectPersistence<E: EffectSpec> {
    /// Persists the original intent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn prepared(&mut self, key: EffectKey, intent: &E::Intent) -> Result<(), EffectError>;
    /// Persists the fenced execution claim.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn executing(&mut self, key: EffectKey, fence: EffectFence) -> Result<(), EffectError>;
    /// Persists the claim together with its monotonic ordinal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn executing_with_ordinal(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        ordinal: u64,
    ) -> Result<(), EffectError> {
        if ordinal == 0 {
            return self.executing(key, fence);
        }
        // A legacy persistence adapter that only stores the key and fence
        // cannot safely accept a coordinator attempt: replay would lose the
        // monotonic ordinal and could admit a stale fence.  Failing here is
        // safer than silently persisting a fake counter.
        Err(EffectError::Persistence(
            "persistence adapter does not record effect ordinals".into(),
        ))
    }
    /// Persists an ambiguous outcome.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn ambiguous(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError>;
    /// Persists an ambiguous phase with its exact attempt ordinal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn ambiguous_with_ordinal(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError> {
        if ordinal == 0 {
            return self.ambiguous(key, fence, reason);
        }
        Err(EffectError::Persistence(
            "persistence adapter does not record effect ordinals".into(),
        ))
    }
    /// Persists a validated receipt.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    fn confirmed(&mut self, key: EffectKey, receipt: &E::Receipt) -> Result<(), EffectError>;

    /// Returns the fault controller shared with the persistence adapter, when
    /// it exposes production crash seams.  The coordinator adopts this
    /// controller so a fault armed for the effect call is observed by the
    /// same owner that controls the durable pre-call fence.
    fn fault_controller(&self) -> Option<std::sync::Arc<crate::fault::Faults>> {
        None
    }

    /// Returns the highest durable attempt ordinal known by this adapter.
    ///
    /// Confirmed entries do not expose their attempt ordinal through
    /// [`EffectState`], so a restart needs this separate monotonic watermark
    /// before admitting another execution.
    fn max_ordinal(&self) -> u64 {
        0
    }
}

/// In-memory persistence adapter useful for deterministic tests.
pub struct MemoryPersistence<E: EffectSpec> {
    /// Phase records in append order.
    pub records: Vec<EffectRecord<E::Receipt>>,
    /// Test seam for the sink-success/receipt-persistence crash window.
    #[cfg(test)]
    pub(crate) fail_next_confirmation: bool,
    /// Exact attempt ordinals retained by the in-memory adapter so tests do
    /// not accidentally exercise a persistence seam that drops the counter.
    attempt_ordinals: BTreeMap<EffectKey, u64>,
    last_ordinal: u64,
    _intent: PhantomData<fn() -> E>,
}

impl<E: EffectSpec> Default for MemoryPersistence<E> {
    fn default() -> Self {
        Self {
            records: Vec::new(),
            #[cfg(test)]
            fail_next_confirmation: false,
            attempt_ordinals: BTreeMap::new(),
            last_ordinal: 0,
            _intent: PhantomData,
        }
    }
}

/// A compact effect phase record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectRecord<R> {
    /// Prepared key.
    Prepared(EffectKey),
    /// Executing key and fence.
    Executing(EffectKey, EffectFence),
    /// Ambiguous key, fence, and reason.
    Ambiguous(EffectKey, EffectFence, AmbiguousReason),
    /// Confirmed key and receipt.
    Confirmed(EffectKey, R),
}

impl<E: EffectSpec> EffectPersistence<E> for MemoryPersistence<E> {
    fn prepared(&mut self, key: EffectKey, _intent: &E::Intent) -> Result<(), EffectError> {
        self.records.push(EffectRecord::Prepared(key));
        Ok(())
    }
    fn executing(&mut self, key: EffectKey, fence: EffectFence) -> Result<(), EffectError> {
        self.executing_with_ordinal(key, fence, 0)
    }
    fn executing_with_ordinal(
        &mut self,
        key: EffectKey,
        effect_fence: EffectFence,
        ordinal: u64,
    ) -> Result<(), EffectError> {
        if ordinal == 0 || ordinal <= self.last_ordinal || fence(key, ordinal) != effect_fence {
            return Err(EffectError::ConflictingHistory);
        }
        self.last_ordinal = ordinal;
        self.attempt_ordinals.insert(key, ordinal);
        self.records
            .push(EffectRecord::Executing(key, effect_fence));
        Ok(())
    }
    fn ambiguous(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError> {
        self.ambiguous_with_ordinal(key, fence, 0, reason)
    }
    fn ambiguous_with_ordinal(
        &mut self,
        key: EffectKey,
        effect_fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError> {
        if ordinal == 0
            || self.attempt_ordinals.get(&key) != Some(&ordinal)
            || fence(key, ordinal) != effect_fence
        {
            return Err(EffectError::ConflictingHistory);
        }
        self.records
            .push(EffectRecord::Ambiguous(key, effect_fence, reason));
        Ok(())
    }
    fn confirmed(&mut self, key: EffectKey, receipt: &E::Receipt) -> Result<(), EffectError> {
        #[cfg(test)]
        if self.fail_next_confirmation {
            self.fail_next_confirmation = false;
            return Err(EffectError::Persistence(
                "injected confirmation failure".into(),
            ));
        }
        self.records
            .push(EffectRecord::Confirmed(key, receipt.clone()));
        Ok(())
    }

    fn max_ordinal(&self) -> u64 {
        self.last_ordinal
    }
}

/// Effect coordinator errors.
#[derive(Debug)]
pub enum EffectError {
    /// A persistence operation failed.
    Persistence(String),
    /// A journal operation failed, preserving append uncertainty and its
    /// fixed-width retry identity for callers that need to reconcile it.
    Journal(JournalError),
    /// The requested key does not exist.
    Missing,
    /// A phase transition was invalid.
    InvalidTransition,
    /// A sink call failed.
    Sink(SinkError),
    /// The monotonic effect fence cannot be advanced.
    OrdinalOverflow,
    /// A persisted history contains conflicting or duplicate phase data.
    ConflictingHistory,
    /// A persisted receipt failed typed decoding or validation.
    InvalidReceipt(String),
    /// A production crash seam fired at an effect durability boundary.
    Injected(InjectedCrash),
}
impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "effect error: {self:?}")
    }
}
impl std::error::Error for EffectError {}

impl EffectError {
    /// Returns the fixed-width retry identity when a journal append reached
    /// an uncertain post-write state. The identity is not a durability claim;
    /// callers must retry the exact record or reopen before admitting it.
    #[must_use]
    pub fn uncertain_receipt<D: JournalDomain>(&self) -> Option<JournalReceipt<D>> {
        match self {
            Self::Journal(error) => error.uncertain_receipt(),
            Self::Persistence(_)
            | Self::Missing
            | Self::InvalidTransition
            | Self::Sink(_)
            | Self::OrdinalOverflow
            | Self::ConflictingHistory
            | Self::InvalidReceipt(_)
            | Self::Injected(_) => None,
        }
    }
}
