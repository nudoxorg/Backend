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
    ProducerObservationClaims, ProducerObservationVerifier, Relation, RelationState, Schema,
    ScopeRoot, StateRoot, UntrustedProducerObservation, admit_complete_scope,
    admit_producer_observation, prepare_delta_with_state,
};
use std::collections::BTreeMap;

mod relation;

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

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        let expected = self.observation();
        if observation == &expected {
            Ok(ProducerObservationClaims::new(
                expected.producer_identity(),
                expected.scope_root(),
                expected.context(),
                *blake3::hash(expected.evidence()).as_bytes(),
            ))
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
