//! The linear in-memory transition machine.

use super::journal_codec::RecoveredEffect;
use super::spec::{EffectKey, EffectSpec};
use super::state::{
    AmbiguousReason, EffectError, EffectFence, EffectPersistence, EffectPhase, EffectSink,
    EffectState, SinkApply, SinkError, SinkObservation, fence,
};
use crate::fault::{Boundary, Faults};
use std::collections::BTreeMap;
use std::sync::Arc;

struct Entry<E: EffectSpec> {
    intent: E::Intent,
    state: EffectState<E::Receipt>,
}

/// Coordinates durable intent phases and sink reconciliation.
pub struct EffectCoordinator<E: EffectSpec, P, S> {
    spec: E,
    persistence: P,
    sink: S,
    entries: BTreeMap<EffectKey, Entry<E>>,
    next_ordinal: u64,
    faults: Arc<Faults>,
}

impl<E, P, S> EffectCoordinator<E, P, S>
where
    E: EffectSpec,
    P: EffectPersistence<E>,
    S: EffectSink<E>,
{
    /// Creates an empty coordinator.
    #[must_use]
    pub fn new(spec: E, persistence: P, sink: S) -> Self {
        let next_ordinal = persistence.max_ordinal();
        let faults = persistence
            .fault_controller()
            .unwrap_or_else(|| Arc::new(Faults::default()));
        Self {
            spec,
            persistence,
            sink,
            entries: BTreeMap::new(),
            next_ordinal,
            faults,
        }
    }

    /// Installs production fault seams for crash-boundary tests. The default
    /// coordinator uses inert hooks, so normal callers do not pay a policy
    /// decision at the sink boundary.
    #[must_use]
    pub fn with_faults(mut self, faults: Arc<Faults>) -> Self {
        self.faults = faults;
        self
    }

    /// Makes the intent durable. Replaying the same key is idempotent.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn prepare(&mut self, intent: E::Intent) -> Result<EffectHandle, EffectError> {
        let key = self.spec.key(&intent);
        if let Some(entry) = self.entries.get(&key) {
            return Ok(EffectHandle {
                key,
                phase: entry.state.phase(),
            });
        }
        self.persistence
            .prepared(key, &intent)
            .map_err(persistence_error)?;
        self.entries.insert(
            key,
            Entry {
                intent,
                state: EffectState::Prepared,
            },
        );
        Ok(EffectHandle {
            key,
            phase: EffectPhase::Prepared,
        })
    }

    /// Returns a phase snapshot.
    #[must_use]
    pub fn state(&self, key: EffectKey) -> Option<EffectPhase> {
        self.entries.get(&key).map(|entry| entry.state.phase())
    }

    /// Starts the sink call under a newly persisted fence.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn execute(&mut self, key: EffectKey) -> Result<EffectHandle, EffectError> {
        if !matches!(
            self.entries.get(&key).ok_or(EffectError::Missing)?.state,
            EffectState::Prepared
        ) {
            return Err(EffectError::InvalidTransition);
        }
        let ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(EffectError::OrdinalOverflow)?;
        let effect_fence = fence(key, ordinal);
        self.persistence
            .executing_with_ordinal(key, effect_fence, ordinal)
            .map_err(persistence_error)?;
        self.next_ordinal = ordinal;
        self.entries
            .get_mut(&key)
            .ok_or(EffectError::Missing)?
            .state = EffectState::Executing {
            fence: effect_fence,
            ordinal,
        };

        // The request is the only owned value needed across the sink call.
        // `apply_borrowed` lets large request implementations keep their
        // backing buffers in place instead of cloning at this boundary.
        let request = {
            let entry = self.entries.get(&key).ok_or(EffectError::Missing)?;
            self.spec.request(&entry.intent)
        };
        self.faults
            .trip(Boundary::EffectCall)
            .map_err(EffectError::Injected)?;
        let outcome = match self.sink.apply_borrowed(key, &request) {
            Ok(outcome) => outcome,
            Err(error) => {
                let reason = sink_reason(&error);
                return self.ambiguous_after_call(
                    key,
                    effect_fence,
                    ordinal,
                    reason,
                    Err(EffectError::Sink(error)),
                );
            }
        };
        match outcome {
            SinkApply::Confirmed(receipt) => {
                if let Err(error) = self.spec.validate_receipt(key, &request, &receipt) {
                    return self.ambiguous_after_call(
                        key,
                        effect_fence,
                        ordinal,
                        AmbiguousReason::InvalidReceipt,
                        Err(error),
                    );
                }
                if let Err(error) = self.persistence.confirmed(key, &receipt) {
                    // The sink has returned success. We cannot know whether
                    // the receipt append reached stable storage, so the live
                    // coordinator must become reconcilable immediately.
                    self.set_ambiguous(key, effect_fence, ordinal, AmbiguousReason::UnknownOutcome);
                    return Err(persistence_error(error));
                }
                self.entries
                    .get_mut(&key)
                    .ok_or(EffectError::Missing)?
                    .state = EffectState::Confirmed { receipt };
                Ok(EffectHandle {
                    key,
                    phase: EffectPhase::Confirmed,
                })
            }
            SinkApply::Unknown => self.ambiguous_after_call(
                key,
                effect_fence,
                ordinal,
                AmbiguousReason::UnknownOutcome,
                Ok(EffectHandle {
                    key,
                    phase: EffectPhase::Ambiguous,
                }),
            ),
        }
    }

    fn ambiguous_after_call(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
        result: Result<EffectHandle, EffectError>,
    ) -> Result<EffectHandle, EffectError> {
        let persist = self
            .persistence
            .ambiguous_with_ordinal(key, fence, ordinal, reason);
        self.set_ambiguous(key, fence, ordinal, reason);
        if let Err(error) = persist {
            return Err(persistence_error(error));
        }
        result
    }

    fn set_ambiguous(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    ) {
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.state = EffectState::Ambiguous {
                fence,
                ordinal,
                reason,
            };
        }
    }

    /// Marks an executing phase ambiguous after timeout or process recovery.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn mark_ambiguous(
        &mut self,
        key: EffectKey,
        reason: AmbiguousReason,
    ) -> Result<EffectHandle, EffectError> {
        let EffectState::Executing {
            fence: effect_fence,
            ordinal,
        } = self.entries.get(&key).ok_or(EffectError::Missing)?.state
        else {
            return Err(EffectError::InvalidTransition);
        };
        let persist = self
            .persistence
            .ambiguous_with_ordinal(key, effect_fence, ordinal, reason);
        self.set_ambiguous(key, effect_fence, ordinal, reason);
        persist.map_err(persistence_error)?;
        Ok(EffectHandle {
            key,
            phase: EffectPhase::Ambiguous,
        })
    }

    /// Resolves an ambiguous phase by querying the sink's idempotency table.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn reconcile(&mut self, key: EffectKey) -> Result<EffectHandle, EffectError> {
        let EffectState::Ambiguous {
            fence: effect_fence,
            ordinal,
            ..
        } = self.entries.get(&key).ok_or(EffectError::Missing)?.state
        else {
            return Err(EffectError::InvalidTransition);
        };
        let request = {
            let entry = self.entries.get(&key).ok_or(EffectError::Missing)?;
            self.spec.request(&entry.intent)
        };
        match self.sink.reconcile(key).map_err(EffectError::Sink)? {
            SinkObservation::Applied(receipt) => {
                if let Err(error) = self.spec.validate_receipt(key, &request, &receipt) {
                    return self.ambiguous_after_call(
                        key,
                        effect_fence,
                        ordinal,
                        AmbiguousReason::InvalidReceipt,
                        Err(error),
                    );
                }
                if let Err(error) = self.persistence.confirmed(key, &receipt) {
                    self.set_ambiguous(key, effect_fence, ordinal, AmbiguousReason::UnknownOutcome);
                    return Err(persistence_error(error));
                }
                self.entries
                    .get_mut(&key)
                    .ok_or(EffectError::Missing)?
                    .state = EffectState::Confirmed { receipt };
                Ok(EffectHandle {
                    key,
                    phase: EffectPhase::Confirmed,
                })
            }
            SinkObservation::Absent => {
                let persist = {
                    let intent = &self.entries.get(&key).ok_or(EffectError::Missing)?.intent;
                    self.persistence.prepared(key, intent)
                };
                persist.map_err(persistence_error)?;
                self.entries
                    .get_mut(&key)
                    .ok_or(EffectError::Missing)?
                    .state = EffectState::Prepared;
                Ok(EffectHandle {
                    key,
                    phase: EffectPhase::Prepared,
                })
            }
            SinkObservation::Unknown => {
                self.set_ambiguous(key, effect_fence, ordinal, AmbiguousReason::UnknownOutcome);
                Ok(EffectHandle {
                    key,
                    phase: EffectPhase::Ambiguous,
                })
            }
        }
    }

    /// Restores typed entries recovered from a hash-chain persistence adapter.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn restore_recovered(
        &mut self,
        recovered: impl IntoIterator<Item = RecoveredEffect<E::Intent, E::Receipt>>,
    ) -> Result<(), EffectError> {
        let mut pending = BTreeMap::new();
        let mut next_ordinal = self.next_ordinal.max(self.persistence.max_ordinal());
        for recovered in recovered {
            let key = self.spec.key(&recovered.intent);
            if self.entries.contains_key(&key) || pending.contains_key(&key) {
                return Err(EffectError::ConflictingHistory);
            }
            let state = recover_state(recovered.state);
            validate_restored_state(&self.spec, key, &recovered.intent, &state)?;
            next_ordinal = next_ordinal.max(state.ordinal().unwrap_or(0));
            pending.insert(
                key,
                Entry {
                    intent: recovered.intent,
                    state,
                },
            );
        }
        self.next_ordinal = next_ordinal;
        self.entries.extend(pending);
        Ok(())
    }

    /// Restores a journal fold and reconciles every nonterminal external
    /// outcome before the caller is allowed to replay new work. Executing
    /// records are treated as ambiguous by recovery, so a lost receipt is
    /// queried through the sink's idempotency table rather than called again
    /// blindly.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn restore_and_reconcile(
        &mut self,
        recovered: impl IntoIterator<Item = RecoveredEffect<E::Intent, E::Receipt>>,
    ) -> Result<Vec<EffectHandle>, EffectError> {
        self.restore_recovered(recovered)?;
        let keys = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                matches!(entry.state.phase(), EffectPhase::Ambiguous).then_some(*key)
            })
            .collect::<Vec<_>>();
        keys.into_iter().map(|key| self.reconcile(key)).collect()
    }

    /// Returns the persisted entry state for recovery inspection.
    #[must_use]
    pub fn state_detail(&self, key: EffectKey) -> Option<&EffectState<E::Receipt>> {
        self.entries.get(&key).map(|entry| &entry.state)
    }

    /// Restores one entry from a replayed effect journal.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn restore(
        &mut self,
        intent: E::Intent,
        state: EffectState<E::Receipt>,
    ) -> Result<EffectHandle, EffectError> {
        let key = self.spec.key(&intent);
        if self.entries.contains_key(&key) {
            return Err(EffectError::InvalidTransition);
        }
        let state = recover_state(state);
        validate_restored_state(&self.spec, key, &intent, &state)?;
        self.next_ordinal = self
            .next_ordinal
            .max(self.persistence.max_ordinal())
            .max(state.ordinal().unwrap_or(0));
        let phase = state.phase();
        self.entries.insert(key, Entry { intent, state });
        Ok(EffectHandle { key, phase })
    }

    /// Borrows the persistence adapter for journal inspection or shutdown.
    pub fn persistence_mut(&mut self) -> &mut P {
        &mut self.persistence
    }
    /// Borrows the sink for reconciliation metrics and health checks.
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }
}

fn recover_state<R>(state: EffectState<R>) -> EffectState<R> {
    match state {
        EffectState::Executing { fence, ordinal } => EffectState::Ambiguous {
            fence,
            ordinal,
            reason: AmbiguousReason::CrashAfterCall,
        },
        state => state,
    }
}

fn validate_restored_state<E: EffectSpec>(
    spec: &E,
    key: EffectKey,
    intent: &E::Intent,
    state: &EffectState<E::Receipt>,
) -> Result<(), EffectError> {
    match state {
        EffectState::Prepared => Ok(()),
        EffectState::Executing {
            fence: effect_fence,
            ordinal,
        }
        | EffectState::Ambiguous {
            fence: effect_fence,
            ordinal,
            ..
        } => {
            if *ordinal == 0 || fence(key, *ordinal) != *effect_fence {
                return Err(EffectError::ConflictingHistory);
            }
            Ok(())
        }
        EffectState::Confirmed { receipt } => {
            let request = spec.request(intent);
            spec.validate_receipt(key, &request, receipt)
        }
    }
}

fn sink_reason(error: &SinkError) -> AmbiguousReason {
    match error {
        SinkError::Unavailable => AmbiguousReason::Timeout,
        SinkError::Rejected => AmbiguousReason::InvalidReceipt,
        SinkError::Unknown => AmbiguousReason::UnknownOutcome,
    }
}

fn persistence_error(error: EffectError) -> EffectError {
    // Persistence adapters already return the package's typed error.  Keep
    // fault, conflict, and I/O classifications intact at this boundary.
    error
}

/// Opaque handle returned after a phase transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectHandle {
    /// Stable idempotency key.
    pub key: EffectKey,
    /// Current durable phase.
    pub phase: EffectPhase,
}

impl EffectHandle {
    /// Stable idempotency key.
    #[must_use]
    pub const fn key(self) -> EffectKey {
        self.key
    }
    /// Current durable phase.
    #[must_use]
    pub const fn phase(self) -> EffectPhase {
        self.phase
    }
}
