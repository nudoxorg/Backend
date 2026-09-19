//! Single-owner relation for daemon-owned remote attempts.
//!
//! Callback payloads are process-local affine values.  The logical metadata
//! for those callbacks is a checked `backend_version::RelationState`; the
//! B-tree maps below are materialized routing indexes only.  Every mutation
//! prepares one exact before/after row transition and commits it against the
//! relation root that was current at preparation time.  This keeps the
//! owner root, accounting, and callback indexes at one linearization point.

use super::{DaemonError, PendingRemoteEnvelope, PendingRemoteKey, RemoteCorrelationKey};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, Delta, DeltaWork, MapChange, ObjectVersion,
    ProducerObservationVerifier, Relation, RelationState, Schema, ScopeRoot, StateRoot,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    prepare_delta_with_state,
};
use std::collections::BTreeMap;

/// Canonical relation for daemon-owned pending-attempt metadata.
///
/// The callback is deliberately absent from this relation: it cannot be
/// serialized or recovered safely. The relation retains the data needed to
/// fence a stale result, account for retained bytes, and recover the owner
/// state root.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct PendingAttemptRelationSchema;

/// A private schema used to mint the relation's initial authority witness.
/// The declaration and observation are made by this owner module through the
/// version crate's linked producer records; no raw scope bytes are accepted.
#[derive(Debug)]
struct PendingAttemptScopeSchema;

impl Schema for PendingAttemptScopeSchema {
    const DOMAIN: u8 = 0xE5;
    const TYPE: u16 = 0x0002;
    type Value = [u8];

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// The daemon's internal metadata relation has no external authority, so its
/// producer evidence is minted by this private owner verifier.  Keeping the
/// complete observation (identity, scope, context, and evidence) explicit
/// prevents a raw scope digest from becoming an authoritative witness.
struct PendingAttemptCoverageVerifier {
    scope: ScopeRoot,
}

impl PendingAttemptCoverageVerifier {
    fn observation(&self) -> UntrustedProducerObservation {
        let mut evidence = Vec::with_capacity(64);
        evidence.extend_from_slice(b"backend.engine.daemon.pending-attempt.v1\0");
        evidence.extend_from_slice(self.scope.as_bytes());
        UntrustedProducerObservation::new(
            *blake3::hash(evidence.as_slice()).as_bytes(),
            self.scope,
            *blake3::hash(b"backend.engine.daemon.pending-attempt.context.v1").as_bytes(),
            evidence,
        )
    }
}

impl ProducerObservationVerifier for PendingAttemptCoverageVerifier {
    type Error = &'static str;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        if observation == &self.observation() {
            Ok(())
        } else {
            Err("pending-attempt authority observation mismatch")
        }
    }
}

/// Produces the checked coverage carried by every pending relation state.
///
/// The error branch is deliberately `Unsupported`: a relation with no
/// admitted producer evidence is unusable for mutation because
/// `prepare_delta_with_state` rejects it. It never downgrades the state to a
/// fabricated partial witness.
fn pending_relation_coverage() -> CoverageWitness {
    let scope = ObjectVersion::<PendingAttemptScopeSchema>::from_value(
        b"backend-engine/daemon/pending-attempt/v1".as_slice(),
    );
    let declared = AuthorityScopeClaim::from_object_version(scope);
    let verifier = PendingAttemptCoverageVerifier {
        scope: declared.scope_root(),
    };
    let Ok(observation) = admit_producer_observation(verifier.observation(), &verifier) else {
        return CoverageWitness::Unsupported(backend_version::UntrustedCoverageScope::new(0));
    };
    match admit_complete_scope(declared, observation) {
        Ok(complete) => CoverageWitness::Complete(complete),
        Err(_) => CoverageWitness::Unsupported(backend_version::UntrustedCoverageScope::new(0)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PendingAttemptValue {
    correlation: RemoteCorrelationKey,
    /// Canonical, nonnegative byte count. Keeping this as `u64` makes the
    /// relation encoding checked and avoids lossy `usize` saturation.
    retained_bytes: u64,
    generation: u64,
}

impl Relation for PendingAttemptRelationSchema {
    const DOMAIN: u8 = 0xE5;
    const TYPE: u16 = 0x0001;

    type Key = PendingRemoteKey;
    type Value = PendingAttemptValue;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(&key.work_key().to_bytes());
        output.extend_from_slice(&key.attempt().get().to_be_bytes());
        output.extend_from_slice(&key.generation().to_be_bytes());
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.correlation.work_key().as_bytes());
        output.extend_from_slice(&value.correlation.attempt().get().to_be_bytes());
        output.extend_from_slice(&value.retained_bytes.to_be_bytes());
        output.extend_from_slice(&value.generation.to_be_bytes());
    }
}

type PendingAttemptRelationState = RelationState<PendingAttemptRelationSchema>;

/// Owner root includes the checked relation commitment and monotonic
/// lifecycle/accounting counters that distinguish a replacement from a stale
/// callback. The counters are canonical `u64` values so the root never
/// depends on a platform-width conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingAttemptStateRoot {
    relation: StateRoot<PendingAttemptRelationSchema>,
    revision: u64,
    next_generation: u64,
    count: u64,
    retained_bytes: u64,
}

/// Exact-base metadata transition recorded for recovery and race histories.
///
/// The backend delta is retained alongside the outer owner root pair. A
/// recovery consumer therefore cannot mistake two equal counters for a valid
/// transition: the relation delta itself carries the exact base root,
/// ordered before values, and target root.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct PendingAttemptDelta {
    base: PendingAttemptStateRoot,
    target: PendingAttemptStateRoot,
    relation: Delta<PendingAttemptRelationSchema>,
    work: DeltaWork,
}

struct PendingAttempt<V, A> {
    envelope: PendingRemoteEnvelope<V, A>,
}

/// Immutable owner snapshot for diagnostics, recovery fencing, and race
/// histories. It does not serialize the callback or scheduler guard; those
/// affine values remain process-local.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingAttemptSnapshot {
    revision: u64,
    root: PendingAttemptStateRoot,
    count: u64,
    retained_bytes: u64,
}

/// The daemon's single owner for pending remote attempts.
pub(super) struct PendingAttemptRelation<V, A> {
    /// Process-local callback payloads, indexed by the exact typed owner key.
    entries: BTreeMap<PendingRemoteKey, PendingAttempt<V, A>>,
    /// Materialized routing index; the checked relation is the logical source.
    by_correlation: BTreeMap<RemoteCorrelationKey, PendingRemoteKey>,
    metadata: PendingAttemptRelationState,
    state_root: PendingAttemptStateRoot,
    last_delta: Option<PendingAttemptDelta>,
}

impl<V, A> Default for PendingAttemptRelation<V, A> {
    fn default() -> Self {
        let metadata = PendingAttemptRelationState::empty(pending_relation_coverage());
        let state_root = PendingAttemptStateRoot {
            relation: metadata.root(),
            revision: 0,
            next_generation: 1,
            count: 0,
            retained_bytes: 0,
        };
        Self {
            entries: BTreeMap::new(),
            by_correlation: BTreeMap::new(),
            metadata,
            state_root,
            last_delta: None,
        }
    }
}

impl<V, A> PendingAttemptRelation<V, A> {
    pub(super) const fn len(&self) -> u64 {
        self.state_root.count
    }

    /// Captures the exact owner state root and accounting values at one
    /// linearization point. The count is maintained by the owner and does not
    /// require traversing the relation tree.
    pub(super) fn snapshot(&self) -> PendingAttemptSnapshot {
        PendingAttemptSnapshot {
            revision: self.state_root.revision,
            root: self.state_root,
            count: self.state_root.count,
            retained_bytes: self.state_root.retained_bytes,
        }
    }

    pub(super) fn get(&self, key: &PendingRemoteKey) -> Option<&PendingRemoteEnvelope<V, A>> {
        self.entries.get(key).map(|attempt| &attempt.envelope)
    }

    pub(super) fn get_mut(
        &mut self,
        key: &PendingRemoteKey,
    ) -> Option<&mut PendingRemoteEnvelope<V, A>> {
        self.entries
            .get_mut(key)
            .map(|attempt| &mut attempt.envelope)
    }

    pub(super) fn key_for_correlation(
        &self,
        correlation: &RemoteCorrelationKey,
    ) -> Option<PendingRemoteKey> {
        self.by_correlation.get(correlation).copied()
    }

    /// Reserves exact result bytes while admission is in flight. The metadata
    /// row is changed with one exact-base delta; no unrelated rows are cloned
    /// or rebuilt.
    pub(super) fn charge_result_bytes(
        &mut self,
        key: PendingRemoteKey,
        bytes: usize,
        max_bytes: usize,
    ) -> Result<(), DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let bytes_u64 = checked_u64(bytes)?;
        let max_bytes_u64 = checked_u64(max_bytes)?;
        let next_attempt_bytes = current
            .retained_bytes
            .checked_add(bytes_u64)
            .ok_or(DaemonError::Backpressure)?;
        let next_value = PendingAttemptValue {
            correlation: current.correlation,
            retained_bytes: next_attempt_bytes,
            generation: current.generation,
        };
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), Some(&next_value))?;
        let next_count = project_count(current_count, Some(&current), Some(&next_value))?;
        if next_bytes > max_bytes || checked_u64(next_bytes)? > max_bytes_u64 {
            return Err(DaemonError::Backpressure);
        }
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current),
                after: Some(next_value),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(())
    }

    /// Rolls back a result-byte reservation while retaining the affine
    /// envelope for fallback or retry.
    pub(super) fn release_result_bytes(
        &mut self,
        key: PendingRemoteKey,
        bytes: usize,
    ) -> Result<(), DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let bytes_u64 = checked_u64(bytes)?;
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        if current.retained_bytes < bytes_u64 || current_bytes < bytes {
            return Err(DaemonError::Accounting);
        }
        let next_attempt_bytes = current.retained_bytes - bytes_u64;
        let next_value = PendingAttemptValue {
            correlation: current.correlation,
            retained_bytes: next_attempt_bytes,
            generation: current.generation,
        };
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), Some(&next_value))?;
        let next_count = project_count(current_count, Some(&current), Some(&next_value))?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current),
                after: Some(next_value),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(())
    }

    /// Inserts an attempt and its materialized indexes as one owner
    /// operation. All fallible identity, capacity, and relation checks run
    /// before either index or callback is changed.
    pub(super) fn insert(
        &mut self,
        key: PendingRemoteKey,
        envelope: PendingRemoteEnvelope<V, A>,
        retained_bytes: usize,
        max_count: usize,
        max_bytes: usize,
    ) -> Result<PendingRemoteKey, DaemonError> {
        if key.generation() != 0 {
            return Err(DaemonError::Accounting);
        }
        let current_count = self.count_usize()?;
        let current_bytes = self.retained_bytes_usize()?;
        if current_count >= max_count {
            return Err(DaemonError::Backpressure);
        }
        let correlation = envelope.correlation();
        if self.entries.contains_key(&key) || self.by_correlation.contains_key(&correlation) {
            return Err(DaemonError::Dispatch("duplicate remote attempt".into()));
        }
        let retained_bytes_u64 = checked_u64(retained_bytes)?;
        let max_bytes_u64 = checked_u64(max_bytes)?;
        let next_count = project_count(
            current_count,
            None,
            Some(&PendingAttemptValue {
                correlation,
                retained_bytes: retained_bytes_u64,
                generation: self.state_root.next_generation,
            }),
        )?;
        let next_bytes = project_retained_bytes(
            current_bytes,
            None,
            Some(&PendingAttemptValue {
                correlation,
                retained_bytes: retained_bytes_u64,
                generation: self.state_root.next_generation,
            }),
        )?;
        if next_bytes > max_bytes || checked_u64(next_bytes)? > max_bytes_u64 {
            return Err(DaemonError::Backpressure);
        }
        let generation = self.state_root.next_generation;
        if generation == 0 {
            return Err(DaemonError::Accounting);
        }
        let next_generation = checked_increment(generation)?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let bound_key = key.with_generation(generation);
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key: bound_key,
                before: None,
                after: Some(PendingAttemptValue {
                    correlation,
                    retained_bytes: retained_bytes_u64,
                    generation,
                }),
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            next_generation,
            next_count,
            next_bytes,
        )?;

        // The checks above make these map operations infallible in the owner
        // model. If an internal invariant is nevertheless broken, restore the
        // changed entry before returning an accounting error and leave the
        // checked relation untouched.
        let mut envelope = envelope;
        envelope.bind_generation(generation);
        if let Some(previous) = self.entries.insert(bound_key, PendingAttempt { envelope }) {
            self.entries.insert(bound_key, previous);
            return Err(DaemonError::Accounting);
        }
        if let Some(previous) = self.by_correlation.insert(correlation, bound_key) {
            self.by_correlation.insert(correlation, previous);
            let _ = self.entries.remove(&bound_key);
            return Err(DaemonError::Accounting);
        }
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(bound_key)
    }

    /// Removes one attempt and its materialized indexes together. The
    /// relation transition is prepared before either callback index changes.
    pub(super) fn remove(
        &mut self,
        key: PendingRemoteKey,
    ) -> Result<PendingRemoteEnvelope<V, A>, DaemonError> {
        let current = self
            .metadata
            .get(&key)
            .cloned()
            .ok_or(DaemonError::RemoteResultUnmatched)?;
        if self.by_correlation.get(&current.correlation) != Some(&key) {
            return Err(DaemonError::Accounting);
        }
        if !self.entries.contains_key(&key) {
            return Err(DaemonError::RemoteResultUnmatched);
        }
        let current_bytes = self.retained_bytes_usize()?;
        let current_count = self.count_usize()?;
        let next_bytes = project_retained_bytes(current_bytes, Some(&current), None)?;
        let next_count = project_count(current_count, Some(&current), None)?;
        let next_revision = checked_increment(self.state_root.revision)?;
        let (metadata, relation, work) = Self::prepare_change(
            &self.metadata,
            MapChange {
                key,
                before: Some(current.clone()),
                after: None,
            },
        )?;
        let target = Self::target_root(
            metadata.root(),
            next_revision,
            self.state_root.next_generation,
            next_count,
            next_bytes,
        )?;

        let Some(attempt) = self.entries.remove(&key) else {
            return Err(DaemonError::RemoteResultUnmatched);
        };
        let Some(index_key) = self.by_correlation.remove(&current.correlation) else {
            self.entries.insert(key, attempt);
            return Err(DaemonError::Accounting);
        };
        if index_key != key {
            self.by_correlation.insert(current.correlation, index_key);
            self.entries.insert(key, attempt);
            return Err(DaemonError::Accounting);
        }
        self.commit_metadata(metadata, relation, work, target);
        self.debug_assert_invariants();
        Ok(attempt.envelope)
    }

    /// Drops all pending callbacks through the same one-row exact-base path.
    /// This is lifecycle cleanup and may perform one bounded tree update per
    /// retained row; ordinary admission never materializes unrelated rows.
    pub(super) fn clear(&mut self) -> Result<(), DaemonError> {
        loop {
            let next_key = self.metadata.iter().next().map(|(key, _)| *key);
            let Some(key) = next_key else {
                break;
            };
            let envelope = self.remove(key)?;
            drop(envelope);
        }
        self.debug_assert_invariants();
        Ok(())
    }

    fn count_usize(&self) -> Result<usize, DaemonError> {
        usize::try_from(self.state_root.count).map_err(|_| DaemonError::Accounting)
    }

    fn retained_bytes_usize(&self) -> Result<usize, DaemonError> {
        usize::try_from(self.state_root.retained_bytes).map_err(|_| DaemonError::Accounting)
    }

    fn prepare_change(
        base: &PendingAttemptRelationState,
        change: MapChange<PendingAttemptRelationSchema>,
    ) -> Result<
        (
            PendingAttemptRelationState,
            Delta<PendingAttemptRelationSchema>,
            DeltaWork,
        ),
        DaemonError,
    > {
        let (prepared, work) =
            prepare_delta_with_state(base, vec![change]).map_err(|_| DaemonError::Accounting)?;
        let relation = prepared.delta().clone();
        let metadata = prepared.commit(base).map_err(|_| DaemonError::Accounting)?;
        Ok((metadata, relation, work))
    }

    fn target_root(
        relation: StateRoot<PendingAttemptRelationSchema>,
        revision: u64,
        next_generation: u64,
        count: usize,
        retained_bytes: usize,
    ) -> Result<PendingAttemptStateRoot, DaemonError> {
        Ok(PendingAttemptStateRoot {
            relation,
            revision,
            next_generation,
            count: checked_u64(count)?,
            retained_bytes: checked_u64(retained_bytes)?,
        })
    }

    fn commit_metadata(
        &mut self,
        metadata: PendingAttemptRelationState,
        relation: Delta<PendingAttemptRelationSchema>,
        work: DeltaWork,
        target: PendingAttemptStateRoot,
    ) {
        let base = self.state_root;
        self.metadata = metadata;
        self.state_root = target;
        self.last_delta = Some(PendingAttemptDelta {
            base,
            target,
            relation,
            work,
        });
    }

    #[cfg(test)]
    fn debug_assert_invariants(&self) {
        debug_assert_eq!(
            u64::try_from(self.entries.len()).ok(),
            Some(self.state_root.count)
        );
        debug_assert_eq!(self.entries.len(), self.by_correlation.len());
        debug_assert_eq!(self.state_root.relation, self.metadata.root());
        if let Some(delta) = &self.last_delta {
            debug_assert_eq!(delta.relation.base(), delta.base.relation);
            debug_assert_eq!(delta.relation.target(), delta.target.relation);
            debug_assert_eq!(delta.target, self.state_root);
            debug_assert_eq!(
                delta.base.revision.checked_add(1),
                Some(delta.target.revision)
            );
        }
    }

    #[cfg(not(test))]
    const fn debug_assert_invariants(&self) {
        let _ = self.state_root;
    }
}

fn checked_u64(value: usize) -> Result<u64, DaemonError> {
    u64::try_from(value).map_err(|_| DaemonError::Accounting)
}

fn project_retained_bytes(
    total: usize,
    before: Option<&PendingAttemptValue>,
    after: Option<&PendingAttemptValue>,
) -> Result<usize, DaemonError> {
    let before = before
        .map(|value| usize::try_from(value.retained_bytes))
        .transpose()
        .map_err(|_| DaemonError::Accounting)?
        .unwrap_or(0);
    let after = after
        .map(|value| usize::try_from(value.retained_bytes))
        .transpose()
        .map_err(|_| DaemonError::Accounting)?
        .unwrap_or(0);
    total
        .checked_sub(before)
        .and_then(|value| value.checked_add(after))
        .ok_or(DaemonError::Accounting)
}

fn project_count(
    count: usize,
    before: Option<&PendingAttemptValue>,
    after: Option<&PendingAttemptValue>,
) -> Result<usize, DaemonError> {
    let count = if before.is_some() && after.is_none() {
        count.checked_sub(1).ok_or(DaemonError::Accounting)?
    } else if before.is_none() && after.is_some() {
        count.checked_add(1).ok_or(DaemonError::Accounting)?
    } else {
        count
    };
    Ok(count)
}

fn checked_increment(value: u64) -> Result<u64, DaemonError> {
    value.checked_add(1).ok_or(DaemonError::Accounting)
}

#[cfg(test)]
mod tests {
    use super::pending_relation_coverage;
    use backend_version::{Coverage, MapChange, Relation, RelationState, prepare_delta_with_state};

    #[test]
    fn initial_pending_relation_has_admitted_complete_coverage() {
        let state = RelationState::<TestRelation>::empty(pending_relation_coverage());
        assert_eq!(state.coverage().state(), Coverage::Complete);
    }

    #[test]
    fn one_row_transition_has_logarithmic_tree_work() {
        let base_entries: Vec<_> = (0..10_000u64).map(|key| (key, key * 3)).collect();
        let source_state =
            RelationState::<TestRelation>::from_entries(base_entries, pending_relation_coverage());
        assert!(source_state.is_ok());
        let Ok(source_state) = source_state else {
            return;
        };
        let prepared = prepare_delta_with_state(
            &source_state,
            vec![MapChange {
                key: 5_000,
                before: None,
                after: Some(99_999),
            }],
        );
        // A one-attempt mutation must inspect/copy only the authenticated
        // path.  Rebuilding all 10,000 rows would violate the owner relation
        // boundary and make a flood proportional to retained history.
        assert!(prepared.is_err());

        let prepared = prepare_delta_with_state(
            &source_state,
            vec![MapChange {
                key: 5_000,
                before: Some(15_000),
                after: Some(99_999),
            }],
        );
        assert!(prepared.is_ok());
        let Ok((prepared, work)) = prepared else {
            return;
        };
        let next = prepared.commit(&source_state);
        assert!(next.is_ok());
        let Ok(next) = next else {
            return;
        };
        assert_eq!(next.get(&5_000), Some(&99_999));
        // The persistent tree may re-encode the bounded leaf neighborhood,
        // but it must never clone the complete relation.
        assert!(work.rows < 1_024);
        assert!(work.nodes <= 64);
        assert!(work.visited_nodes <= 64);
        assert!(work.copied_nodes <= 64);

        // The typed delta is bound to the exact relation root. A candidate
        // prepared against this root cannot be committed as another state.
        let other = RelationState::<TestRelation>::from_entries(
            [(5_000, 15_001)],
            pending_relation_coverage(),
        );
        assert!(other.is_ok());
        let Ok(other) = other else {
            return;
        };
        let candidate_delta = prepare_delta_with_state(
            &source_state,
            vec![MapChange {
                key: 5_000,
                before: Some(15_000),
                after: Some(99_999),
            }],
        );
        assert!(candidate_delta.is_ok());
        let Ok((candidate_delta, _)) = candidate_delta else {
            return;
        };
        assert!(candidate_delta.commit(&other).is_err());
    }

    #[derive(Debug)]
    struct TestRelation;

    impl Relation for TestRelation {
        const DOMAIN: u8 = 0xE5;
        const TYPE: u16 = 0x0003;
        type Key = u64;
        type Value = u64;

        fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
            output.extend_from_slice(&key.to_be_bytes());
        }

        fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(&value.to_be_bytes());
        }
    }
}
