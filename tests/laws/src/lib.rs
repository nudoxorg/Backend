//! Independent executable laws for the public v2 version, store, and flow APIs.
//!
//! The oracle code in this crate deliberately has no access to production
//! internals. In particular, map roots are encoded from the public wire
//! grammar here, weighted rows are consolidated by a separate model, and the
//! graph tests recompute closure from scratch. The tests are bounded so that
//! they remain useful in ordinary `cargo test` runs as well as in mutation
//! jobs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Metadata describing one executable law family.
///
/// Keeping this data beside the law implementation makes a reviewable
/// contract inventory. The strings name the grammar and fault boundary; the
/// actual expected values are computed by the independent helpers below.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestContract {
    /// Stable law identifier.
    pub law: &'static str,
    /// Canonical input grammar exercised by the law.
    pub grammar: &'static str,
    /// Small ingredients that are known to expose the fault.
    pub bug_ingredients: &'static [&'static str],
    /// Description of the independent oracle.
    pub independent_oracle: &'static str,
    /// Reduction used to keep generated cases bounded and reproducible.
    pub reducer: &'static str,
    /// Coverage represented by the case family.
    pub coverage: &'static str,
    /// Implementation fault sites guarded by the law.
    pub fault_sites: &'static [&'static str],
    /// Explicit execution budget.
    pub budget: &'static str,
}

const CONTRACTS: &[TestContract] = &[
    TestContract {
        law: "version_store.root_and_delta",
        grammar: "sorted unique byte keys; optional complete StoredValue",
        bug_ingredients: &["reorder", "delete", "re-add", "value-only edit", "re-key"],
        independent_oracle: "literal V2NODE grammar plus domain-separated BLAKE3 root",
        reducer: "at most 2,049 keys and 24 history operations",
        coverage: "empty, one leaf, several anchored leaves, multi-level branches",
        fault_sites: &["canonical cuts", "before checks", "delta target fence"],
        budget: "proptest 32 cases; maps <= 2,049 rows",
    },
    TestContract {
        law: "flow.weighted_operators",
        grammar: "nonzero signed rows ordered by (key,value,time)",
        bug_ingredients: &[
            "duplicate coordinates",
            "negative support",
            "cross term",
            "retraction",
        ],
        independent_oracle: "BTreeMap support algebra and explicit three-term join expansion",
        reducer: "at most 18 rows, six keys, and five times",
        coverage: "batch, map, filter, join, group, distinct, reduce, top-k",
        fault_sites: &[
            "consolidation",
            "old-state joins",
            "support crossings",
            "ranking boundary",
        ],
        budget: "proptest 48 cases; values in -8..=8",
    },
    TestContract {
        law: "flow.recursion_and_demand",
        grammar: "finite directed graph and fully typed WorkKey",
        bug_ingredients: &[
            "SCC merge",
            "SCC split",
            "edge delete",
            "no-op",
            "root/sequence fence",
            "raw coverage self-admission",
        ],
        independent_oracle: "fresh transitive closure, SCC partition, and demand state model",
        reducer: "at most 6 graph nodes and 36 directed edges",
        coverage: "cycles, disconnected components, demand add/remove, reset and gap",
        fault_sites: &[
            "recursive rederivation",
            "coverage admission",
            "subscriber fence",
            "typed producer linkage",
        ],
        budget: "deterministic cases plus proptest 24 graphs",
    },
    TestContract {
        law: "cutover.shadow_order_budget_and_churn",
        grammar: "randomized multi-leaf rows, owned variable payloads, bounded process transitions",
        bug_ingredients: &[
            "full recompute shadow",
            "multi-row ordering",
            "large payload",
            "one-row delta",
            "disjoint 100k join",
            "stalled observer",
        ],
        independent_oracle: "public API result compared with BTreeMap/full join and checked process observations",
        reducer: "fixed seeds; at most 100,000 rows, 917,504 payload bytes, and 64 churn epochs",
        coverage: "canonical rebuild, delta delivery, byte admission, join, pin fence, and compaction release",
        fault_sites: &[
            "arrangement ordering",
            "owned payload clone",
            "full-view subscription",
            "nested-loop join",
            "compaction debt",
        ],
        budget: "one integration target with a 45-second process deadline",
    },
];

/// Returns the law metadata consumed by review and mutation runners.
#[must_use]
pub const fn test_contracts() -> &'static [TestContract] {
    CONTRACTS
}

#[cfg(test)]
mod laws {
    #![allow(clippy::panic, clippy::unwrap_used)]

    use super::*;
    use backend_flow::{
        Arrangement, ArrangementRoot, Delta, Demand, DemandGraph, DistinctState, Epoch,
        FactorDelta, FactorSchema, Frontier, InputSchema, Microbatch, ObjectIdentity, RecipeSchema,
        RecursionBarrier, RelationIdentity, RowKey, SignedBatch, Time, TopKState, WorkCounters,
        WorkKey, affected_region, distinct_checked, filter_checked, group_count, group_sum,
        incremental_join, join_checked, map_checked, recursive_recompute, reduce_checked,
        reduce_rows, top_k_changes, top_k_checked,
    };
    use backend_store::{
        Change, LayoutId, OrderedMap, StoreError, StoredValue, decode_pack, encode_pack,
    };
    use backend_version::{
        AuthorityScopeClaim, ClosedRelationScope, CoverageWitness, MapChange, ObjectClosure,
        ObjectVersion, ProducerObservationClaims, ProducerObservationVerifier, Relation,
        RelationBinding, RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
        admit_complete_scope, admit_producer_observation, apply_delta, bind_scope_equality,
        partial_coverage, prepare_delta,
    };
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};

    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
    const CUT_MIN: usize = 64;
    const CUT_TARGET: usize = 256;
    const CUT_MAX: usize = 1024;
    const CUT_MIN_WIRE: u16 = 64;
    const CUT_TARGET_WIRE: u16 = 256;
    const CUT_MAX_WIRE: u16 = 1024;

    fn value(n: u8) -> StoredValue {
        StoredValue::new(vec![n], 1, vec![])
    }

    fn value_with_refs(n: u8, refs: &[[u8; 32]]) -> StoredValue {
        StoredValue::new(vec![n], 1, refs.to_vec())
    }

    fn map_from(items: impl IntoIterator<Item = (Vec<u8>, StoredValue)>) -> OrderedMap {
        OrderedMap::try_from_iter_with_coverage(items, complete_scope_equality_fixture(73))
            .unwrap_or_else(|_| panic!("valid checked map fixture"))
    }

    fn empty_map() -> OrderedMap {
        OrderedMap::try_empty_with_coverage(complete_scope_equality_fixture(73))
            .unwrap_or_else(|_| panic!("valid checked empty-map fixture"))
    }

    fn flow_relation(seed: u8) -> RelationIdentity {
        RelationIdentity::from_value(&[seed; 32])
    }

    fn flow_object(seed: u8) -> ObjectIdentity {
        ObjectIdentity::from_value(&[seed; 32])
    }

    fn flow_time(epoch: u64) -> Time {
        Time::new(Epoch(epoch), 0)
    }

    fn flow_row(key: u64, payload: i64, epoch: u64, diff: i64) -> Delta<i64> {
        Delta::checked(
            RowKey {
                relation: flow_relation(1),
                object: flow_object(1),
                key,
            },
            payload,
            flow_time(epoch),
            diff,
        )
        .unwrap_or_else(|_| panic!("nonzero fixture weight"))
    }

    fn flow_row_for(
        relation: u8,
        object: u8,
        key: u64,
        payload: i64,
        epoch: u64,
        diff: i64,
    ) -> Delta<i64> {
        Delta::checked(
            RowKey {
                relation: flow_relation(relation),
                object: flow_object(object),
                key,
            },
            payload,
            flow_time(epoch),
            diff,
        )
        .unwrap_or_else(|_| panic!("nonzero fixture weight"))
    }

    // This fixture crosses the same sealed producer observation boundary as a
    // product authority adapter, while keeping the verifier local to the
    // independent law crate.
    fn complete_scope_equality_fixture(seed: u8) -> CoverageWitness {
        let authority_version = ObjectVersion::<FactorSchema>::from_value(&vec![seed]);
        let observed_version = ObjectVersion::<FactorSchema>::from_value(&vec![seed]);
        let authority = AuthorityScopeClaim::from_object_version(authority_version);
        let observed = ScopeRoot::from_bytes(observed_version.to_bytes());
        let observation = admit_producer_observation(
            UntrustedProducerObservation::new(
                *observed.as_bytes(),
                observed,
                *observed.as_bytes(),
                observed.as_bytes().to_vec(),
            ),
            &LawProducerVerifier(observed),
        )
        .unwrap_or_else(|_| panic!("matching producer observation"));
        CoverageWitness::Complete(
            admit_complete_scope(authority, observation)
                .unwrap_or_else(|_| panic!("matching admitted scope fixture")),
        )
    }

    struct LawProducerVerifier(ScopeRoot);

    impl ProducerObservationVerifier for LawProducerVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            let scope = self.0;
            let expected_identity = *scope.as_bytes();
            let expected_evidence = scope.as_bytes();
            if observation.producer_identity() != expected_identity
                || observation.context() != expected_identity
                || observation.scope_root() != scope
                || observation.evidence() != expected_evidence
            {
                return Err("producer identity, context, or evidence mismatch");
            }
            Ok(ProducerObservationClaims::new(
                expected_identity,
                scope,
                expected_identity,
                *blake3::hash(expected_evidence).as_bytes(),
            ))
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ModelMap {
        entries: BTreeMap<Vec<u8>, StoredValue>,
    }

    impl ModelMap {
        fn empty() -> Self {
            Self {
                entries: BTreeMap::new(),
            }
        }

        fn from_entries(items: impl IntoIterator<Item = (Vec<u8>, StoredValue)>) -> Self {
            let mut entries = BTreeMap::new();
            for (key, value) in items {
                assert!(entries.insert(key, value).is_none(), "model duplicate key");
            }
            Self { entries }
        }

        fn apply(&self, changes: &[Change]) -> Result<Self, &'static str> {
            if changes.windows(2).any(|pair| pair[0].key >= pair[1].key) {
                return Err("unordered or duplicate changes");
            }
            let mut next = self.entries.clone();
            for change in changes {
                if next.get(&change.key) != change.before.as_ref() {
                    return Err("before mismatch");
                }
                match &change.after {
                    Some(after) => {
                        next.insert(change.key.clone(), after.clone());
                    }
                    None => {
                        next.remove(&change.key);
                    }
                }
            }
            Ok(Self { entries: next })
        }

        fn changes_to(&self, other: &Self) -> Vec<Change> {
            let keys = self
                .entries
                .keys()
                .chain(other.entries.keys())
                .cloned()
                .collect::<BTreeSet<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let before = self.entries.get(&key).cloned();
                    let after = other.entries.get(&key).cloned();
                    (before != after).then_some(Change { key, before, after })
                })
                .collect()
        }

        fn rows(&self) -> Vec<(Vec<u8>, StoredValue)> {
            self.entries
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        }
    }

    fn append_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn append_u32_le(out: &mut Vec<u8>, value: usize) {
        let value = u32::try_from(value).unwrap_or_else(|_| panic!("bounded oracle length"));
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn append_u64_be(out: &mut Vec<u8>, value: usize) {
        let value = u64::try_from(value).unwrap_or_else(|_| panic!("bounded oracle length"));
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn field(out: &mut Vec<u8>, payload: &[u8]) {
        append_u64_be(out, payload.len());
        out.extend_from_slice(payload);
    }

    fn model_key_bytes(key: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        append_u32_le(&mut out, key.len());
        out.extend_from_slice(key);
        out
    }

    fn model_value_bytes(value: &StoredValue) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(value.availability);
        append_u32_le(&mut out, value.value.len());
        out.extend_from_slice(&value.value);
        append_u32_le(&mut out, value.references.len());
        for reference in &value.references {
            out.extend_from_slice(reference);
        }
        out
    }

    fn cut_word(key: &[u8], level: u16) -> u64 {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.version.cut\0");
        hasher.update(&[1, 2]);
        hasher.update(&[DOMAIN]);
        hasher.update(&TYPE.to_be_bytes());
        hasher.update(&[VERSION]);
        hasher.update(&level.to_be_bytes());
        hasher.update(&CUT_MIN_WIRE.to_be_bytes());
        hasher.update(&CUT_TARGET_WIRE.to_be_bytes());
        hasher.update(&CUT_MAX_WIRE.to_be_bytes());
        let encoded_key = model_key_bytes(key);
        hasher.update(&(encoded_key.len() as u64).to_be_bytes());
        hasher.update(&encoded_key);
        let digest = hasher.finalize();
        let mut word = [0_u8; 8];
        word.copy_from_slice(&digest.as_bytes()[..8]);
        u64::from_be_bytes(word)
    }

    fn model_cuts(keys: &[Vec<u8>], level: u16) -> Vec<usize> {
        let mut cuts = Vec::new();
        let mut start = 0;
        for (index, key) in keys.iter().enumerate() {
            let count = index - start + 1;
            if count < CUT_MIN {
                continue;
            }
            let word = cut_word(key, level);
            let threshold = u64::MAX / CUT_TARGET as u64;
            if count >= CUT_MAX || word <= threshold {
                cuts.push(index + 1);
                start = index + 1;
            }
        }
        if start < keys.len() {
            cuts.push(keys.len());
        }
        cuts
    }

    #[derive(Clone)]
    struct ModelNode {
        first_key: Option<Vec<u8>>,
        digest: [u8; 32],
        row_count: u64,
    }

    fn state_digest(node_bytes: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.version.v2\0");
        hasher.update(&[0x52, DOMAIN]);
        hasher.update(&TYPE.to_be_bytes());
        hasher.update(&[VERSION]);
        hasher.update(&(node_bytes.len() as u64).to_be_bytes());
        hasher.update(node_bytes);
        *hasher.finalize().as_bytes()
    }

    fn leaf_node(entries: &[(Vec<u8>, StoredValue)]) -> ModelNode {
        let mut payload = Vec::new();
        for (key, value) in entries {
            field(&mut payload, &model_key_bytes(key));
            field(&mut payload, &model_value_bytes(value));
        }
        let mut bytes = b"V2NODE\0".to_vec();
        bytes.push(1);
        bytes.push(2);
        bytes.push(0x01);
        bytes.push(VERSION);
        bytes.push(DOMAIN);
        append_u16(&mut bytes, TYPE);
        append_u16(&mut bytes, 0);
        field(&mut bytes, &payload);
        ModelNode {
            first_key: entries.first().map(|(key, _)| key.clone()),
            digest: state_digest(&bytes),
            row_count: u64::try_from(entries.len()).unwrap_or(u64::MAX),
        }
    }

    fn branch_node(level: u16, children: &[ModelNode]) -> ModelNode {
        let mut payload = Vec::new();
        for child in children {
            let key = child
                .first_key
                .as_ref()
                .unwrap_or_else(|| panic!("nonempty model child"));
            field(&mut payload, &model_key_bytes(key));
            field(&mut payload, &child.digest);
            payload.extend_from_slice(&child.row_count.to_be_bytes());
        }
        let mut bytes = b"V2NODE\0".to_vec();
        bytes.push(1);
        bytes.push(2);
        bytes.push(0x02);
        bytes.push(VERSION);
        bytes.push(DOMAIN);
        append_u16(&mut bytes, TYPE);
        append_u16(&mut bytes, level);
        field(&mut bytes, &payload);
        ModelNode {
            first_key: children.first().and_then(|child| child.first_key.clone()),
            digest: state_digest(&bytes),
            row_count: children
                .iter()
                .map(|child| child.row_count)
                .try_fold(0_u64, u64::checked_add)
                .unwrap_or(u64::MAX),
        }
    }

    fn model_root(map: &ModelMap) -> [u8; 32] {
        if map.entries.is_empty() {
            return leaf_node(&[]).digest;
        }
        let entries = map.rows();
        let keys = entries
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut level_nodes = Vec::new();
        let mut start = 0;
        for end in model_cuts(&keys, 0) {
            level_nodes.push(leaf_node(&entries[start..end]));
            start = end;
        }
        let mut level = 1_u16;
        while level_nodes.len() > 1 {
            let anchors = level_nodes
                .iter()
                .map(|node| node.first_key.clone().unwrap_or_default())
                .collect::<Vec<_>>();
            let cuts = model_cuts(&anchors, level);
            let mut next = Vec::new();
            let mut start = 0;
            for end in cuts {
                next.push(branch_node(level, &level_nodes[start..end]));
                start = end;
            }
            level_nodes = next;
            level = level
                .checked_add(1)
                .unwrap_or_else(|| panic!("bounded tree height"));
        }
        level_nodes[0].digest
    }

    fn digest_bytes(class: u8, payloads: &[&[u8]]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.version.v2\0");
        hasher.update(&[class, DOMAIN]);
        hasher.update(&TYPE.to_be_bytes());
        hasher.update(&[VERSION]);
        for payload in payloads {
            hasher.update(&(payload.len() as u64).to_be_bytes());
            hasher.update(payload);
        }
        *hasher.finalize().as_bytes()
    }

    fn model_delta_bytes(changes: &[Change]) -> Vec<u8> {
        let mut out = Vec::new();
        for change in changes {
            out.push(0x03);
            field(&mut out, &model_key_bytes(&change.key));
            match (&change.before, &change.after) {
                (None, None) => out.push(0),
                (Some(before), None) => {
                    out.push(1);
                    field(&mut out, &model_value_bytes(before));
                }
                (None, Some(after)) => {
                    out.push(2);
                    field(&mut out, &model_value_bytes(after));
                }
                (Some(before), Some(after)) => {
                    out.push(3);
                    field(&mut out, &model_value_bytes(before));
                    field(&mut out, &model_value_bytes(after));
                }
            }
        }
        out
    }

    fn model_delta_id(base: [u8; 32], target: [u8; 32], changes: &[Change]) -> [u8; 32] {
        let bytes = model_delta_bytes(changes);
        digest_bytes(0x44, &[&base, &target, &bytes])
    }

    fn map_root(map: &OrderedMap) -> [u8; 32] {
        map.state_root().to_bytes()
    }

    fn model_of(map: &OrderedMap) -> ModelMap {
        ModelMap::from_entries(map.iter().map(|(key, value)| (key.clone(), value.clone())))
    }

    #[test]
    fn contract_inventory_is_concrete_and_nonvacuous() {
        assert_eq!(test_contracts().len(), 4);
        for contract in test_contracts() {
            assert!(!contract.law.is_empty());
            assert!(!contract.grammar.is_empty());
            assert!(!contract.independent_oracle.is_empty());
            assert!(!contract.reducer.is_empty());
            assert!(!contract.coverage.is_empty());
            assert!(!contract.budget.is_empty());
            assert!(!contract.bug_ingredients.is_empty());
            assert!(!contract.fault_sites.is_empty());
        }
    }

    #[test]
    fn canonical_root_matches_independent_literal_encoder() {
        let map = map_from([
            (vec![0], value_with_refs(2, &[[7; 32]])),
            (vec![1], value(3)),
            (vec![255], value(4)),
        ]);
        let model = model_of(&map);
        assert_eq!(map_root(&map), model_root(&model));

        let mut large = ModelMap::empty();
        for number in 0_u16..=2048 {
            let key = number.to_be_bytes().to_vec();
            large.entries.insert(key.clone(), value(key[1] % 251));
        }
        let large_map = map_from(large.rows());
        assert_eq!(map_root(&large_map), model_root(&large));
    }

    #[test]
    fn value_only_edit_reuses_unchanged_tree_and_honors_budget() {
        let entries = (0_u16..=2048).map(|number| {
            let key = number.to_be_bytes().to_vec();
            (key.clone(), value(key[1] % 251))
        });
        let map = map_from(entries);
        let key = 1024_u16.to_be_bytes().to_vec();
        let changes = [Change {
            key: key.clone(),
            before: Some(value(0)),
            after: Some(value(9)),
        }];
        let (updated, stats) = map
            .apply_with_stats(&changes)
            .unwrap_or_else(|_| panic!("value edit"));
        assert_ne!(updated, map);
        assert!(stats.visited_nodes > 0);
        assert!(stats.copied_nodes > 0);
        assert!(stats.reused_nodes > 0);
        assert!(matches!(
            map.apply_with_budget(&changes, backend_store::WorkBudget::new(0)),
            Err(StoreError::NeedsScopedRebuild)
        ));
    }

    #[derive(Clone, Debug)]
    struct RawOp {
        kind: u8,
        key: u8,
        other: u8,
        value: u8,
    }

    fn raw_op_strategy() -> impl Strategy<Value = RawOp> {
        (0_u8..8, 0_u8..8, 0_u8..8, any::<u8>()).prop_map(|(kind, key, other, value)| RawOp {
            kind,
            key,
            other,
            value,
        })
    }

    fn key_byte(key: u8) -> Vec<u8> {
        vec![key]
    }

    fn one_change(model: &ModelMap, key: u8, new_value: u8) -> Option<Change> {
        let key = key_byte(key);
        let before = model.entries.get(&key).cloned();
        let after = Some(value(new_value));
        (before != after).then_some(Change { key, before, after })
    }

    fn delete_change(model: &ModelMap, key: u8) -> Option<Change> {
        let key = key_byte(key);
        model.entries.get(&key).cloned().map(|before| Change {
            key,
            before: Some(before),
            after: None,
        })
    }

    fn valid_history(raw: &[RawOp]) -> Vec<Vec<Change>> {
        let mut model = ModelMap::empty();
        let mut history = Vec::new();
        for op in raw {
            let kind = op.kind % 5;
            let mut changes = match kind {
                0 => one_change(&model, op.key, op.value).map(|change| vec![change]),
                1 => delete_change(&model, op.key).map(|change| vec![change]),
                2 => {
                    let source = key_byte(op.key);
                    let target = key_byte(op.other);
                    if source != target
                        && model.entries.contains_key(&source)
                        && !model.entries.contains_key(&target)
                    {
                        Some(vec![
                            Change {
                                key: source.clone(),
                                before: model.entries.get(&source).cloned(),
                                after: None,
                            },
                            Change {
                                key: target,
                                before: None,
                                after: Some(value(op.value)),
                            },
                        ])
                    } else {
                        None
                    }
                }
                3 => {
                    let first = one_change(&model, op.key, op.value);
                    let second = one_change(&model, op.other, op.value.wrapping_add(1));
                    match (first, second) {
                        (Some(first), Some(second)) if first.key != second.key => {
                            Some(vec![first, second])
                        }
                        (Some(first), _) => Some(vec![first]),
                        (_, Some(second)) => Some(vec![second]),
                        _ => None,
                    }
                }
                _ => {
                    let key = op.key % 4;
                    if model.entries.contains_key(&key_byte(key)) {
                        delete_change(&model, key).map(|change| vec![change])
                    } else {
                        one_change(&model, key, op.value).map(|change| vec![change])
                    }
                }
            };
            let Some(mut changes) = changes.take() else {
                continue;
            };
            if !changes.is_empty() {
                changes.sort_by(|left, right| left.key.cmp(&right.key));
                model = model
                    .apply(&changes)
                    .unwrap_or_else(|_| panic!("history operation is valid"));
                history.push(changes);
            }
        }
        history
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn arbitrary_valid_map_histories_match_model(raw in proptest::collection::vec(raw_op_strategy(), 1..25)) {
            let mut candidate = empty_map();
            let mut model = ModelMap::empty();
            for changes in valid_history(&raw) {
                let next = model.apply(&changes).unwrap_or_else(|_| panic!("valid model transition"));
                let previous = candidate.clone();
                candidate = candidate.apply(&changes).unwrap_or_else(|_| panic!("valid store transition"));
                let delta = previous.delta_to(&candidate).unwrap_or_else(|_| panic!("history delta"));
                assert_eq!(
                    delta.apply(&previous).unwrap_or_else(|_| panic!("history delta apply")),
                    candidate
                );
                let inverse = delta.inverse().unwrap_or_else(|_| panic!("history inverse"));
                assert_eq!(
                    inverse.apply(&candidate).unwrap_or_else(|_| panic!("history inverse apply")),
                    previous
                );
                assert_eq!(candidate.iter().map(|(key, value)| (key.clone(), value.clone())).collect::<Vec<_>>(), next.rows());
                assert_eq!(map_root(&candidate), model_root(&next));
                model = next;
            }
        }
    }

    #[test]
    fn map_history_covers_insert_delete_readd_value_edit_rekey_group_and_order() {
        let mut model = ModelMap::empty();
        let mut candidate = empty_map();
        let batches = [
            vec![
                Change {
                    key: vec![3],
                    before: None,
                    after: Some(value(3)),
                },
                Change {
                    key: vec![7],
                    before: None,
                    after: Some(value(7)),
                },
            ],
            vec![Change {
                key: vec![3],
                before: Some(value(3)),
                after: Some(value(9)),
            }],
            vec![Change {
                key: vec![3],
                before: Some(value(9)),
                after: None,
            }],
            vec![Change {
                key: vec![3],
                before: None,
                after: Some(value(11)),
            }],
            vec![
                Change {
                    key: vec![2],
                    before: None,
                    after: Some(value(2)),
                },
                Change {
                    key: vec![3],
                    before: Some(value(11)),
                    after: None,
                },
            ],
            vec![
                Change {
                    key: vec![7],
                    before: Some(value(7)),
                    after: None,
                },
                Change {
                    key: vec![8],
                    before: None,
                    after: Some(value(7)),
                },
            ],
        ];
        for changes in batches {
            let next = model
                .apply(&changes)
                .unwrap_or_else(|_| panic!("valid history"));
            candidate = candidate
                .apply(&changes)
                .unwrap_or_else(|_| panic!("valid history"));
            assert_eq!(map_root(&candidate), model_root(&next));
            model = next;
        }
        let shuffled = map_from(vec![(vec![8], value(7)), (vec![2], value(2))]);
        assert_eq!(map_root(&shuffled), model_root(&model));
    }

    #[test]
    fn untrusted_map_constructors_cannot_prepare_deltas() {
        let raw_empty =
            OrderedMap::try_empty().unwrap_or_else(|_| panic!("valid unchecked empty-map fixture"));
        let raw_empty_target = raw_empty
            .apply(&[Change {
                key: vec![1],
                before: None,
                after: Some(value(1)),
            }])
            .unwrap_or_else(|_| panic!("valid raw transition"));
        assert_eq!(
            raw_empty.delta_to(&raw_empty_target),
            Err(StoreError::IncompleteCoverage)
        );

        let raw_base = OrderedMap::try_from_iter([(vec![1], value(1))])
            .unwrap_or_else(|_| panic!("valid raw map fixture"));
        let raw_target = raw_base
            .apply(&[Change {
                key: vec![1],
                before: Some(value(1)),
                after: Some(value(2)),
            }])
            .unwrap_or_else(|_| panic!("valid raw transition"));
        assert_eq!(
            raw_base.delta_to(&raw_target),
            Err(StoreError::IncompleteCoverage)
        );
    }

    #[test]
    fn exact_map_delta_laws_include_wrong_base_and_target_fences() {
        let a = map_from([(vec![1], value(1)), (vec![3], value(3))]);
        let b = a
            .apply(&[
                Change {
                    key: vec![1],
                    before: Some(value(1)),
                    after: Some(value(4)),
                },
                Change {
                    key: vec![2],
                    before: None,
                    after: Some(value(2)),
                },
            ])
            .unwrap_or_else(|_| panic!("valid transition"));
        let c = b
            .apply(&[
                Change {
                    key: vec![1],
                    before: Some(value(4)),
                    after: None,
                },
                Change {
                    key: vec![4],
                    before: None,
                    after: Some(value(8)),
                },
            ])
            .unwrap_or_else(|_| panic!("valid transition"));
        let model_a = model_of(&a);
        let model_b = model_of(&b);
        let model_c = model_of(&c);
        let ab = a.delta_to(&b).unwrap_or_else(|_| panic!("delta"));
        let bc = b.delta_to(&c).unwrap_or_else(|_| panic!("delta"));
        let expected_ab = model_a.changes_to(&model_b);
        assert_eq!(ab.changes().cloned().collect::<Vec<_>>(), expected_ab);
        assert_eq!(ab.base().to_bytes(), model_root(&model_a));
        assert_eq!(ab.target().to_bytes(), model_root(&model_b));
        assert_eq!(
            ab.id().as_bytes(),
            &model_delta_id(model_root(&model_a), model_root(&model_b), &expected_ab)
        );
        assert_eq!(ab.apply(&a).unwrap_or_else(|_| panic!("apply")), b);
        let inverse = ab.inverse().unwrap_or_else(|_| panic!("inverse"));
        assert_eq!(
            inverse
                .apply(&b)
                .unwrap_or_else(|_| panic!("inverse apply")),
            a
        );
        let composed = ab.compose(&bc).unwrap_or_else(|_| panic!("compose"));
        let expected_final = model_a.changes_to(&model_c);
        assert_eq!(
            composed.changes().cloned().collect::<Vec<_>>(),
            expected_final
        );
        assert_eq!(
            composed
                .apply(&a)
                .unwrap_or_else(|_| panic!("compose apply")),
            c
        );
        assert_eq!(
            composed.id().as_bytes(),
            &model_delta_id(map_root(&a), map_root(&c), &expected_final)
        );
        assert!(matches!(ab.apply(&c), Err(StoreError::WrongBase)));
        assert!(matches!(ab.compose(&ab), Err(StoreError::WrongBase)));
        assert_eq!(ab.target(), b.state_root());
        assert_eq!(composed.target(), c.state_root());
    }

    #[test]
    fn version_delta_apply_inverse_compose_and_base_fences_are_exact() {
        let coverage = complete_scope_equality_fixture(41);
        let base = RelationState::<TestRelation>::from_entries(
            [(1_u64, 10_i64), (2_u64, 20_i64)],
            coverage,
        )
        .unwrap_or_else(|_| panic!("base state"));
        let first_changes = vec![
            MapChange {
                key: 1,
                before: Some(10),
                after: Some(11),
            },
            MapChange {
                key: 3,
                before: None,
                after: Some(30),
            },
        ];
        let first =
            prepare_delta(&base, first_changes.clone()).unwrap_or_else(|_| panic!("first delta"));
        assert_eq!(first.changes().cloned().collect::<Vec<_>>(), first_changes);
        assert_eq!(first.base(), base.root());
        let after_first = apply_delta(&base, &first).unwrap_or_else(|_| panic!("first apply"));
        assert_eq!(after_first.get(&1), Some(&11));
        assert_eq!(after_first.get(&3), Some(&30));
        assert_eq!(first.target(), after_first.root());

        let second_changes = vec![
            MapChange {
                key: 1,
                before: Some(11),
                after: Some(12),
            },
            MapChange {
                key: 2,
                before: Some(20),
                after: None,
            },
        ];
        let second = prepare_delta(&after_first, second_changes.clone())
            .unwrap_or_else(|_| panic!("second delta"));
        let composed =
            backend_version::Delta::compose(&first, &second).unwrap_or_else(|_| panic!("compose"));
        assert_eq!(
            backend_version::Delta::compose(&first, &first),
            Err(backend_version::DeltaError::NonAdjacent)
        );
        let after_second =
            apply_delta(&after_first, &second).unwrap_or_else(|_| panic!("second apply"));
        assert_eq!(composed.base(), base.root());
        assert_eq!(composed.target(), after_second.root());
        assert_eq!(
            composed.changes().cloned().collect::<Vec<_>>(),
            vec![
                MapChange {
                    key: 1,
                    before: Some(10),
                    after: Some(12),
                },
                MapChange {
                    key: 2,
                    before: Some(20),
                    after: None,
                },
                MapChange {
                    key: 3,
                    before: None,
                    after: Some(30),
                },
            ]
        );
        assert_eq!(
            apply_delta(&base, &composed).unwrap_or_else(|_| panic!("composed apply")),
            after_second
        );
        let inverse = composed.inverse();
        assert_eq!(
            apply_delta(&after_second, &inverse).unwrap_or_else(|_| panic!("inverse apply")),
            base
        );

        let wrong_base = RelationState::<TestRelation>::from_entries([(1_u64, 9_i64)], coverage)
            .unwrap_or_else(|_| panic!("wrong base"));
        assert_eq!(
            apply_delta(&wrong_base, &first),
            Err(backend_version::DeltaError::BaseMismatch)
        );
        let no_op = prepare_delta(
            &base,
            vec![MapChange {
                key: 1,
                before: Some(10),
                after: Some(10),
            }],
        )
        .unwrap_or_else(|_| panic!("no-op delta"));
        assert!(no_op.changes().next().is_none());
        assert_eq!(no_op.base(), no_op.target());
    }

    #[test]
    fn physical_pack_identity_uses_bytes_and_layout_is_separate() {
        let map = map_from([
            (vec![1], value(1)),
            (vec![2], value_with_refs(2, &[[9; 32], [8; 32]])),
        ]);
        let first = encode_pack(&map, LayoutId::derive(b"layout-a"), 4096)
            .unwrap_or_else(|_| panic!("pack"));
        let second = encode_pack(&map, LayoutId::derive(b"layout-b"), 4096)
            .unwrap_or_else(|_| panic!("pack"));
        assert_eq!(first.id(), second.id());
        assert_ne!(first.layout(), second.layout());
        assert_eq!(*first.id().as_bytes(), pack_digest(first.bytes()));
        assert_eq!(
            decode_pack(&first).unwrap_or_else(|_| panic!("decode")),
            map
        );
        assert_eq!(
            decode_pack(&second).unwrap_or_else(|_| panic!("decode")),
            map
        );

        let mut wire = first.to_wire();
        wire.id[0] ^= 1;
        assert!(matches!(wire.admit(4096), Err(StoreError::Corrupt)));
    }

    fn pack_digest(bytes: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"pack");
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
        *hasher.finalize().as_bytes()
    }

    fn row_tuple<V: Clone>(row: &backend_flow::RowRef<'_, V>) -> (RowKey, V, Time, i64) {
        (row.key, row.value.clone(), row.time, row.diff.value())
    }

    fn model_consolidate<V: Clone + Ord>(
        input: impl IntoIterator<Item = Delta<V>>,
    ) -> Vec<Delta<V>> {
        let mut rows = input.into_iter().collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            left.key
                .cmp(&right.key)
                .then(left.value.cmp(&right.value))
                .then(left.time.cmp(&right.time))
        });
        let mut out: Vec<Delta<V>> = Vec::new();
        for row in rows {
            if let Some(last) = out.last_mut()
                && last.key == row.key
                && last.value == row.value
                && last.time == row.time
            {
                let next = last
                    .diff
                    .value()
                    .checked_add(row.diff.value())
                    .unwrap_or_else(|| panic!("bounded support"));
                if next == 0 {
                    out.pop();
                } else {
                    last.diff = Delta::checked(row.key, row.value.clone(), row.time, next)
                        .unwrap_or_else(|_| panic!("nonzero consolidated support"))
                        .diff;
                }
            } else {
                out.push(row);
            }
        }
        out
    }

    fn model_support<V: Clone + Ord>(
        input: impl IntoIterator<Item = Delta<V>>,
    ) -> BTreeMap<(RowKey, V), i64> {
        let mut support: BTreeMap<(RowKey, V), i64> = BTreeMap::new();
        for row in input {
            let key = (row.key, row.value);
            let next = support
                .get(&key)
                .copied()
                .unwrap_or(0)
                .checked_add(row.diff.value())
                .unwrap_or_else(|| panic!("bounded support"));
            if next == 0 {
                support.remove(&key);
            } else {
                support.insert(key, next);
            }
        }
        support
    }

    fn model_top_k<V: Clone + Ord>(
        support: &BTreeMap<(RowKey, V), i64>,
        k: usize,
        mut score: impl FnMut(&V) -> i64,
    ) -> Vec<(RowKey, V, i64)> {
        let mut ranked = support
            .iter()
            .filter(|(_, support)| **support > 0)
            .map(|((key, value), _)| (*key, value.clone(), score(value)))
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .2
                .cmp(&left.2)
                .then(left.0.cmp(&right.0))
                .then(left.1.cmp(&right.1))
        });
        ranked.truncate(k);
        ranked
    }

    fn candidate_tuples<V: Clone>(
        candidates: &[backend_flow::Candidate<V>],
    ) -> Vec<(RowKey, V, i64)> {
        candidates
            .iter()
            .map(|candidate| (candidate.key, candidate.value.clone(), candidate.score))
            .collect()
    }

    fn weighted_row_strategy() -> impl Strategy<Value = (u8, i64, u64, i64)> {
        (
            0_u8..6,
            -8_i64..=8,
            0_u64..5,
            (-3_i64..=3).prop_filter("nonzero weight", |weight| *weight != 0),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        #[test]
        fn arbitrary_weighted_batches_match_independent_consolidation(
            raw in proptest::collection::vec(weighted_row_strategy(), 1..19)
        ) {
            let input = raw
                .iter()
                .map(|(key, value, time, diff)| flow_row(u64::from(*key), *value, *time, *diff))
                .collect::<Vec<_>>();
            let expected = model_consolidate(input.clone());
            let batch = Microbatch::seal(&input).unwrap_or_else(|_| panic!("batch"));
            assert_eq!(batch.input_len(), input.len());
            assert_eq!(batch.len(), expected.len());
            assert_eq!(
                batch.cursor().map(|row| row_tuple(&row)).collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|row| (row.key, row.value, row.time, row.diff.value()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn weighted_microbatch_consolidation_and_arrangement_match_support_model() {
        let input = vec![
            flow_row(2, 4, 2, 1),
            flow_row(1, 4, 1, 1),
            flow_row(2, 4, 2, 1),
            flow_row(1, 8, 1, 1),
            flow_row(1, 8, 1, -1),
        ];
        let expected = model_consolidate(input.clone());
        let batch = Microbatch::seal(&input).unwrap_or_else(|_| panic!("batch"));
        assert_eq!(batch.input_len(), input.len());
        assert_eq!(batch.len(), expected.len());
        assert!(!batch.has_shared_time());
        assert_eq!(
            batch
                .cursor()
                .map(|row| row_tuple(&row))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|row| (row.key, row.value, row.time, row.diff.value()))
                .collect::<Vec<_>>()
        );

        let mut arrangement = Arrangement::new();
        arrangement
            .append(batch)
            .unwrap_or_else(|_| panic!("append"));
        let mut model = BTreeMap::<(RowKey, i64), i64>::new();
        for row in expected {
            let key = (row.key, row.value);
            let next = model.get(&key).copied().unwrap_or(0) + row.diff.value();
            if next == 0 {
                model.remove(&key);
            } else {
                model.insert(key, next);
            }
        }
        assert_eq!(arrangement.visible(), model);
        assert_eq!(arrangement.work_counters().input_rows, input.len() as u64);
    }

    #[test]
    fn arrangement_frontier_pin_and_retention_fences_are_exact() {
        let mut arrangement = Arrangement::new();
        arrangement
            .append(
                Microbatch::seal(&[flow_row(1, 10, 1, 1)])
                    .unwrap_or_else(|_| panic!("first batch")),
            )
            .unwrap_or_else(|_| panic!("first append"));
        arrangement
            .append(
                Microbatch::seal(&[flow_row(2, 20, 2, 1)])
                    .unwrap_or_else(|_| panic!("second batch")),
            )
            .unwrap_or_else(|_| panic!("second append"));
        assert!(arrangement.upper().upper() > flow_time(2));
        assert_eq!(
            arrangement.snapshot_at_checked(flow_time(99)),
            Err(backend_flow::FlowError::ObservationBeyondUpper)
        );

        let pin = arrangement
            .pin_at(flow_time(1))
            .unwrap_or_else(|_| panic!("pin"));
        assert_eq!(
            arrangement.consolidate(flow_time(2)),
            Err(backend_flow::FlowError::PinnedFrontier)
        );
        assert!(arrangement.release_pin(pin));
        assert!(!arrangement.release_pin(pin));
        arrangement
            .consolidate(flow_time(2))
            .unwrap_or_else(|_| panic!("consolidate"));
        assert_eq!(arrangement.since(), flow_time(2));
        assert_eq!(
            arrangement.snapshot_at_checked(flow_time(1)),
            Err(backend_flow::FlowError::ObservationOutsideRetention)
        );
    }

    #[test]
    fn map_filter_group_reduce_and_join_use_signed_oracles() {
        let input = vec![
            flow_row(1, 2, 1, 1),
            flow_row(1, 2, 1, -1),
            flow_row(2, 5, 2, 2),
            flow_row(3, -2, 3, 1),
        ];
        let mut map_fn = |_: RowKey, input: &i64| input * 3;
        let mapped = map_checked(input.clone(), &mut map_fn).unwrap_or_else(|_| panic!("map"));
        let expected_map = model_consolidate(input.iter().map(|row| {
            Delta::checked(row.key, row.value * 3, row.time, row.diff.value())
                .unwrap_or_else(|_| panic!("mapped row"))
        }));
        assert_eq!(mapped, expected_map);

        let mut keep = |_: RowKey, input: &i64| *input >= 0;
        let filtered =
            filter_checked(input.clone(), &mut keep).unwrap_or_else(|_| panic!("filter"));
        let expected_filter = model_consolidate(input.iter().filter(|row| row.value >= 0).cloned());
        assert_eq!(filtered, expected_filter);

        let groups = group_count(input.clone(), |row| row.key.key % 2)
            .unwrap_or_else(|_| panic!("group count"));
        assert_eq!(groups, BTreeMap::from([(0, 2), (1, 1)]));
        let sums = group_sum(input.clone(), |row| row.key.key % 2, |input| *input)
            .unwrap_or_else(|_| panic!("group sum"));
        assert_eq!(sums, BTreeMap::from([(0, 10), (1, -2)]));

        let reduced_rows = reduce_rows(input.clone()).unwrap_or_else(|_| panic!("reduce rows"));
        assert_eq!(reduced_rows.get(&(flow_row(2, 5, 2, 1).key, 5)), Some(&2));
        let reduced = reduce_checked(input.clone()).unwrap_or_else(|_| panic!("reduce"));
        assert_eq!(reduced.get(&flow_row(2, 5, 2, 1).key), Some(&(5, 2)));
        assert!(matches!(
            reduce_checked([flow_row(1, 2, 1, 1), flow_row(1, 3, 1, 1)]),
            Err(backend_flow::FlowError::ConflictingReduceValue)
        ));

        let left_old = vec![
            flow_row_for(1, 1, 10, 2, 1, 1),
            flow_row_for(1, 1, 11, 4, 1, 1),
        ];
        let right_old = vec![
            flow_row_for(2, 1, 20, 2, 1, 1),
            flow_row_for(2, 1, 21, 2, 1, 1),
        ];
        let left_delta = vec![
            flow_row_for(1, 1, 12, 2, 2, -1),
            flow_row_for(1, 1, 13, 6, 2, 1),
        ];
        let right_delta = vec![
            flow_row_for(2, 1, 22, 2, 2, -1),
            flow_row_for(2, 1, 23, 8, 2, 1),
        ];
        let actual = incremental_join(
            &left_old,
            &left_delta,
            &right_old,
            &right_delta,
            |input| (*input).cast_unsigned(),
            |input| (*input).cast_unsigned(),
            |left, right| left + right,
        )
        .unwrap_or_else(|_| panic!("incremental join"));
        let mut expected = join_checked(
            &left_delta,
            &right_old,
            |input| (*input).cast_unsigned(),
            |input| (*input).cast_unsigned(),
            |left, right| left + right,
        )
        .unwrap_or_else(|_| panic!("join"));
        expected.extend(
            join_checked(
                &left_old,
                &right_delta,
                |input| (*input).cast_unsigned(),
                |input| (*input).cast_unsigned(),
                |left, right| left + right,
            )
            .unwrap_or_else(|_| panic!("join")),
        );
        expected.extend(
            join_checked(
                &left_delta,
                &right_delta,
                |input| (*input).cast_unsigned(),
                |input| (*input).cast_unsigned(),
                |left, right| left + right,
            )
            .unwrap_or_else(|_| panic!("join")),
        );
        assert_eq!(actual, model_consolidate(expected));
    }

    #[test]
    fn distinct_and_top_k_track_support_crossings_and_retractions() {
        let key = flow_row(1, 7, 1, 1).key;
        let mut distinct = DistinctState::new();
        let first = distinct
            .apply([flow_row(1, 7, 1, 2)])
            .unwrap_or_else(|_| panic!("distinct"));
        assert_eq!(first.len(), 1);
        let second = distinct
            .apply([flow_row(1, 7, 2, -1)])
            .unwrap_or_else(|_| panic!("distinct"));
        assert!(second.is_empty(), "support 2 -> 1 does not retract");
        let third = distinct
            .apply([flow_row(1, 7, 3, -1)])
            .unwrap_or_else(|_| panic!("distinct"));
        assert_eq!(
            third.iter().map(|row| row.diff.value()).collect::<Vec<_>>(),
            vec![-1]
        );
        assert!(distinct.members().is_empty());
        assert_eq!(key, third[0].key);

        let mut top = TopKState::new();
        let initial_input = [
            flow_row(1, 10, 1, 1),
            flow_row(2, 20, 1, 1),
            flow_row(3, 15, 1, 1),
        ];
        let initial_support = model_support(initial_input.clone());
        let expected_initial = model_top_k(&initial_support, 2, |input| *input);
        let initial = top
            .apply(initial_input, 2, |input| *input)
            .unwrap_or_else(|_| panic!("top k"));
        assert_eq!(candidate_tuples(&initial), expected_initial);
        let before = initial;
        let update = [flow_row(2, 20, 2, -1)];
        let mut after_support = initial_support;
        for row in update.iter().cloned() {
            let key = (row.key, row.value);
            let next = after_support
                .get(&key)
                .copied()
                .unwrap_or(0)
                .checked_add(row.diff.value())
                .unwrap_or_else(|| panic!("bounded support"));
            if next == 0 {
                after_support.remove(&key);
            } else {
                after_support.insert(key, next);
            }
        }
        let expected_after = model_top_k(&after_support, 2, |input| *input);
        let after = top
            .apply(update, 2, |input| *input)
            .unwrap_or_else(|_| panic!("top k"));
        assert_eq!(candidate_tuples(&after), expected_after);
        let changes = top_k_changes(&before, &after, flow_time(2))
            .unwrap_or_else(|_| panic!("top k changes"));
        assert!(changes.iter().any(|row| row.diff.value() < 0));
        assert!(changes.iter().any(|row| row.diff.value() > 0));
        assert_eq!(changes.iter().map(|row| row.diff.value()).sum::<i64>(), 0);
        assert_eq!(
            top_k_checked([flow_row(1, 3, 1, 1), flow_row(2, 8, 1, 1)], 1, |input| {
                *input
            })
            .unwrap_or_else(|_| panic!("top k"))
            .first()
            .map(|candidate| candidate.value),
            Some(8)
        );
    }

    #[test]
    fn recursion_oracle_covers_scc_merge_split_and_delete() {
        let mut edges = BTreeMap::<u8, BTreeSet<u8>>::new();
        edges.insert(1, BTreeSet::from([2]));
        edges.insert(2, BTreeSet::from([1]));
        edges.insert(3, BTreeSet::from([4]));
        edges.insert(4, BTreeSet::from([3]));
        edges.insert(5, BTreeSet::new());
        let changed = BTreeSet::from([2]);
        let region = affected_region(&edges, &changed, 8).unwrap_or_else(|_| panic!("region"));
        assert_eq!(region, BTreeSet::from([1, 2]));
        assert_eq!(
            recursive_recompute(&edges, &changed, 128).unwrap_or_else(|_| panic!("closure")),
            closure_oracle(&edges)
        );

        edges
            .get_mut(&2)
            .unwrap_or_else(|| panic!("edge"))
            .insert(3);
        edges
            .get_mut(&4)
            .unwrap_or_else(|| panic!("edge"))
            .insert(1);
        let merged = recursive_recompute(&edges, &BTreeSet::from([2, 3]), 128)
            .unwrap_or_else(|_| panic!("merged closure"));
        assert!(merged.contains(&(1, 4)));
        assert_eq!(scc_partition(&edges), vec![vec![1, 2, 3, 4], vec![5]]);

        edges
            .get_mut(&2)
            .unwrap_or_else(|| panic!("edge"))
            .remove(&3);
        edges
            .get_mut(&4)
            .unwrap_or_else(|| panic!("edge"))
            .remove(&1);
        let split = recursive_recompute(&edges, &BTreeSet::from([2, 3]), 128)
            .unwrap_or_else(|_| panic!("split closure"));
        assert_eq!(split, closure_oracle(&edges));
        assert_eq!(scc_partition(&edges), vec![vec![1, 2], vec![3, 4], vec![5]]);
        edges
            .get_mut(&1)
            .unwrap_or_else(|| panic!("edge"))
            .remove(&2);
        let deleted = recursive_recompute(&edges, &BTreeSet::from([1, 2]), 128)
            .unwrap_or_else(|_| panic!("deleted closure"));
        assert_eq!(deleted, closure_oracle(&edges));
        assert!(recursive_recompute(&edges, &BTreeSet::from([1]), 1).is_err());

        let mut barrier = RecursionBarrier::new(4);
        barrier.mark([1, 2]);
        assert!(!barrier.is_complete());
        barrier.close(2).unwrap_or_else(|_| panic!("barrier"));
        assert!(barrier.is_complete());
        barrier.reopen();
        assert!(!barrier.is_complete());
    }

    fn closure_oracle(edges: &BTreeMap<u8, BTreeSet<u8>>) -> BTreeSet<(u8, u8)> {
        let mut out = BTreeSet::new();
        for source in edges.keys() {
            let mut queue = vec![*source];
            let mut seen = BTreeSet::new();
            while let Some(node) = queue.pop() {
                if !seen.insert(node) {
                    continue;
                }
                if node != *source {
                    out.insert((*source, node));
                }
                if let Some(targets) = edges.get(&node) {
                    queue.extend(targets.iter().copied());
                }
            }
        }
        out
    }

    fn scc_partition(edges: &BTreeMap<u8, BTreeSet<u8>>) -> Vec<Vec<u8>> {
        let mut remaining = edges.keys().copied().collect::<BTreeSet<_>>();
        let closure = closure_oracle(edges);
        let mut components = Vec::new();
        while let Some(&seed) = remaining.iter().next() {
            let mut component = remaining
                .iter()
                .copied()
                .filter(|node| {
                    *node == seed
                        || (closure.contains(&(seed, *node)) && closure.contains(&(*node, seed)))
                })
                .collect::<Vec<_>>();
            component.sort_unstable();
            for node in &component {
                remaining.remove(node);
            }
            components.push(component);
        }
        components
    }

    fn graph_strategy() -> impl Strategy<Value = (BTreeMap<u8, BTreeSet<u8>>, BTreeSet<u8>)> {
        (proptest::collection::vec(any::<bool>(), 36), 0_u8..6).prop_map(|(bits, changed)| {
            let mut edges = BTreeMap::new();
            for source in 0_u8..6 {
                let mut targets = BTreeSet::new();
                for target in 0_u8..6 {
                    if bits[usize::from(source) * 6 + usize::from(target)] {
                        targets.insert(target);
                    }
                }
                edges.insert(source, targets);
            }
            (edges, BTreeSet::from([changed]))
        })
    }

    fn weak_region_oracle(
        edges: &BTreeMap<u8, BTreeSet<u8>>,
        changed: &BTreeSet<u8>,
    ) -> BTreeSet<u8> {
        let mut undirected = BTreeMap::<u8, BTreeSet<u8>>::new();
        for (source, targets) in edges {
            for target in targets {
                undirected.entry(*source).or_default().insert(*target);
                undirected.entry(*target).or_default().insert(*source);
            }
        }
        let mut out = changed.clone();
        let mut queue = changed.iter().copied().collect::<Vec<_>>();
        while let Some(node) = queue.pop() {
            for neighbor in undirected.get(&node).into_iter().flatten() {
                if out.insert(*neighbor) {
                    queue.push(*neighbor);
                }
            }
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn arbitrary_graphs_match_fresh_closure_and_affected_region(
            (edges, changed) in graph_strategy()
        ) {
            let expected_region = weak_region_oracle(&edges, &changed);
            let actual_region = affected_region(&edges, &changed, 64)
                .unwrap_or_else(|_| panic!("bounded graph region"));
            assert_eq!(actual_region, expected_region);
            assert_eq!(
                recursive_recompute(&edges, &changed, 512)
                    .unwrap_or_else(|_| panic!("bounded graph closure")),
                closure_oracle(&edges)
            );
        }
    }

    #[test]
    fn demand_graph_handles_typed_keys_noops_and_lifecycle() {
        let key = work_key(1);
        let input = work_key(2);
        let mut graph = DemandGraph::default();
        assert!(graph.try_depends_on(key, input).is_ok());
        assert_eq!(graph.dependency_count(key), 1);
        assert_eq!(graph.ready(&BTreeSet::new()), Some(input));
        graph.suppress_noop(input);
        assert!(graph.is_noop(input));
        assert_eq!(graph.ready(&BTreeSet::new()), Some(key));
        graph.mark_dirty(input);
        assert!(!graph.is_noop(input));
        assert_eq!(graph.ready(&BTreeSet::new()), Some(input));
        assert!(matches!(
            graph.try_depends_on(input, key),
            Err(backend_flow::FlowError::DependencyCycle)
        ));

        graph
            .add_demand(Demand {
                consumer: 17,
                work: key,
                range: Some(2..=5),
                freshness: Frontier::new(flow_time(5)),
                priority: 9,
            })
            .expect("law demand admission");
        assert!(graph.is_demanded(key));
        let removed = graph.remove_demand(17).unwrap_or_else(|| panic!("demand"));
        assert_eq!(removed.consumer, 17);
        assert!(!graph.is_demanded(key));
        assert!(graph.remove_demand(17).is_none());
    }

    fn work_key(seed: u8) -> WorkKey {
        WorkKey::new(
            ObjectVersion::<RecipeSchema>::from_value(&[seed; 32]),
            ObjectVersion::<InputSchema>::from_value(&[seed.wrapping_add(1); 32]),
            ObjectVersion::<backend_flow::ReadSchema>::from_value(&[seed.wrapping_add(2); 32]),
            ObjectVersion::<backend_flow::AuthoritySchema>::from_value(&[seed.wrapping_add(3); 32]),
            ObjectVersion::<backend_flow::EquivalenceSchema>::from_value(
                &[seed.wrapping_add(4); 32],
            ),
        )
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn coverage_and_signed_batch_fences_reject_mutants() {
        struct MismatchedProducerClaims;

        impl ProducerObservationVerifier for MismatchedProducerClaims {
            type Error = core::convert::Infallible;

            fn verify(
                &self,
                observation: &UntrustedProducerObservation,
            ) -> Result<ProducerObservationClaims, Self::Error> {
                Ok(ProducerObservationClaims::new(
                    [0xd4; 32],
                    observation.scope_root(),
                    observation.context(),
                    *blake3::hash(observation.evidence()).as_bytes(),
                ))
            }
        }

        let complete = complete_scope_equality_fixture(7);
        let typed_claim = AuthorityScopeClaim::from_object_version(
            ObjectVersion::<FactorSchema>::from_value(&vec![7]),
        );
        let typed_scope = typed_claim.scope_root();
        // Raw scope equality is intentionally weaker than producer admission:
        // it can prove only that two roots match, not that an authority made
        // the observation.
        assert!(matches!(
            bind_scope_equality(typed_claim, ScopeRoot::from_u64(7)),
            Err(backend_version::CoverageAdmissionError::ScopeMismatch { .. })
        ));
        assert!(bind_scope_equality(typed_claim, typed_scope).is_ok());

        let valid_observation = UntrustedProducerObservation::new(
            *typed_scope.as_bytes(),
            typed_scope,
            *typed_scope.as_bytes(),
            typed_scope.as_bytes().to_vec(),
        );
        assert!(
            admit_producer_observation(
                valid_observation.clone(),
                &LawProducerVerifier(typed_scope)
            )
            .is_ok()
        );
        assert!(matches!(
            admit_producer_observation(valid_observation, &MismatchedProducerClaims),
            Err(backend_version::ProducerObservationAdmissionError::ClaimsMismatch)
        ));
        let wrong_producer = UntrustedProducerObservation::new(
            [0xa1; 32],
            typed_scope,
            *typed_scope.as_bytes(),
            typed_scope.as_bytes().to_vec(),
        );
        assert!(matches!(
            admit_producer_observation(wrong_producer, &LawProducerVerifier(typed_scope)),
            Err(backend_version::ProducerObservationAdmissionError::Rejected(_))
        ));
        let wrong_context = UntrustedProducerObservation::new(
            *typed_scope.as_bytes(),
            typed_scope,
            [0xb2; 32],
            typed_scope.as_bytes().to_vec(),
        );
        assert!(matches!(
            admit_producer_observation(wrong_context, &LawProducerVerifier(typed_scope)),
            Err(backend_version::ProducerObservationAdmissionError::Rejected(_))
        ));
        let wrong_evidence = UntrustedProducerObservation::new(
            *typed_scope.as_bytes(),
            typed_scope,
            *typed_scope.as_bytes(),
            vec![0xc3; 32],
        );
        assert!(matches!(
            admit_producer_observation(wrong_evidence, &LawProducerVerifier(typed_scope)),
            Err(backend_version::ProducerObservationAdmissionError::Rejected(_))
        ));

        let wrong_scope =
            ScopeRoot::from_bytes(ObjectVersion::<FactorSchema>::from_value(&vec![8]).to_bytes());
        let admitted_wrong_scope = admit_producer_observation(
            UntrustedProducerObservation::new(
                *wrong_scope.as_bytes(),
                wrong_scope,
                *wrong_scope.as_bytes(),
                wrong_scope.as_bytes().to_vec(),
            ),
            &LawProducerVerifier(wrong_scope),
        )
        .unwrap_or_else(|_| panic!("admit independently valid wrong scope"));
        assert!(matches!(
            admit_complete_scope(typed_claim, admitted_wrong_scope),
            Err(backend_version::CoverageAdmissionError::ScopeMismatch { .. })
        ));

        let mismatch_authority =
            AuthorityScopeClaim::from_object_version(ObjectVersion::<FactorSchema>::from_value(
                &vec![7],
            ));
        assert!(matches!(
            bind_scope_equality(
                mismatch_authority,
                ScopeRoot::from_bytes(
                    ObjectVersion::<FactorSchema>::from_value(&vec![8]).to_bytes(),
                ),
            ),
            Err(backend_version::CoverageAdmissionError::ScopeMismatch { .. })
        ));
        let partial = CoverageWitness::Partial(partial_coverage(7));
        let base = RelationState::<TestRelation>::from_entries([(1_u64, 10_i64)], complete)
            .unwrap_or_else(|_| panic!("state"));
        let change = vec![MapChange {
            key: 1,
            before: Some(10),
            after: Some(11),
        }];
        assert!(prepare_delta(&base, change.clone()).is_ok());
        let partial_base = RelationState::<TestRelation>::from_entries([(1_u64, 10_i64)], partial)
            .unwrap_or_else(|_| panic!("partial state"));
        assert!(matches!(
            prepare_delta(&partial_base, change.clone()),
            Err(backend_version::DeltaError::IncompleteBase)
        ));
        assert!(mutant_without_coverage_accepts(&partial_base, &change));

        let base_factor = ObjectVersion::<FactorSchema>::from_value(&b"base".to_vec());
        let target_factor = ObjectVersion::<FactorSchema>::from_value(&b"target".to_vec());
        let input = ObjectVersion::<InputSchema>::from_value(&[4; 32]);
        let factor_rows = vec![flow_row(1, 10, 1, 1)];
        assert!(
            FactorDelta::admit(
                base_factor,
                target_factor,
                input,
                Frontier::new(flow_time(2)),
                factor_rows.clone(),
                complete,
            )
            .is_ok()
        );
        assert!(matches!(
            FactorDelta::admit(
                base_factor,
                target_factor,
                input,
                Frontier::new(flow_time(2)),
                factor_rows,
                partial,
            ),
            Err(backend_flow::FlowError::IncompleteFrontier)
        ));

        let closed =
            CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(typed_scope));
        let closed_base = RelationState::<TestRelation>::from_entries([(1_u64, 10_i64)], closed)
            .unwrap_or_else(|_| panic!("closed state"));
        let closed_delta = prepare_delta(
            &closed_base,
            vec![MapChange {
                key: 1,
                before: Some(10),
                after: Some(11),
            }],
        )
        .unwrap_or_else(|_| panic!("closed relation delta"));
        let closed_target = apply_delta(&closed_base, &closed_delta)
            .unwrap_or_else(|_| panic!("closed relation apply"));
        assert_eq!(closed_target.coverage(), closed);
        assert!(matches!(
            WorkspaceManifest::new_checked(
                1,
                vec![RelationBinding::from_state(&closed_target)],
                Vec::new(),
                ObjectClosure::from_version(ObjectVersion::<FactorSchema>::from_value(&vec![7])),
                closed,
            ),
            Err(backend_version::WorkspaceError::UnverifiedCoverage
                | backend_version::WorkspaceError::IncompleteRelation,)
        ));

        let mut signed =
            SignedBatch::canonicalize(vec![flow_row(1, 10, 1, 1)], root_for_arrangement(), vec![1])
                .unwrap_or_else(|_| panic!("signed batch"));
        let verifier = AcceptingVerifier;
        assert!(signed.verify(&verifier).is_ok());
        signed.deltas[0].diff = Delta::checked(signed.deltas[0].key, 10, signed.deltas[0].time, 2)
            .unwrap_or_else(|_| panic!("mutate"))
            .diff;
        assert!(matches!(
            signed.verify(&verifier),
            Err(backend_flow::FlowError::InvalidBatch)
        ));
        assert!(mutant_without_digest_fence_accepts(&signed));
    }

    fn mutant_without_coverage_accepts(
        base: &RelationState<TestRelation>,
        changes: &[MapChange<TestRelation>],
    ) -> bool {
        let mut entries = base
            .iter()
            .map(|(key, value)| (*key, *value))
            .collect::<BTreeMap<_, _>>();
        for change in changes {
            if entries.get(&change.key) != change.before.as_ref() {
                return false;
            }
            match change.after {
                Some(value) => {
                    entries.insert(change.key, value);
                }
                None => {
                    entries.remove(&change.key);
                }
            }
        }
        true
    }

    fn mutant_without_digest_fence_accepts<
        V: Clone + Ord + std::fmt::Debug + Eq + backend_flow::CanonicalValue + 'static,
    >(
        batch: &SignedBatch<V>,
    ) -> bool {
        <AcceptingVerifier as backend_flow::BatchVerifier>::verify(
            &AcceptingVerifier,
            &[],
            batch.parent.as_bytes(),
            &batch.signature,
        )
        .is_ok()
    }

    fn mutant_without_subscription_fence<
        V: Clone + Ord + std::fmt::Debug + Eq + backend_flow::CanonicalValue + 'static,
    >(
        arrangement: &Arrangement<V>,
        _root: ArrangementRoot<V>,
        _sequence: u64,
    ) -> backend_flow::SubscriptionEvent<V> {
        backend_flow::SubscriptionEvent::Delta {
            sequence: arrangement.subscribe().sequence(),
            root: arrangement.root(),
            deltas: arrangement.visible_rows(),
        }
    }

    fn root_for_arrangement() -> ArrangementRoot<i64> {
        Arrangement::new().root()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct TestRelation;
    impl Relation for TestRelation {
        const DOMAIN: u8 = 0x91;
        const TYPE: u16 = 1;
        type Key = u64;
        type Value = i64;
        fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
            out.extend_from_slice(&key.to_be_bytes());
        }
        fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
            out.extend_from_slice(&value.to_be_bytes());
        }
    }

    struct AcceptingVerifier;
    impl backend_flow::BatchVerifier for AcceptingVerifier {
        type Error = ();
        fn verify(
            &self,
            _bytes: &[u8],
            _parent: &[u8],
            _signature: &[u8],
        ) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[test]
    fn mutation_falsifiers_are_red_for_retractions_and_join_cross_terms() {
        let rows = [flow_row(1, 5, 1, 1), flow_row(1, 5, 2, -1)];
        let actual = distinct_checked(rows.clone()).unwrap_or_else(|_| panic!("distinct"));
        let mutant = rows
            .iter()
            .filter(|row| row.diff.value() > 0)
            .cloned()
            .collect::<Vec<_>>();
        assert_ne!(actual, mutant, "dropping retractions must be observable");

        let old_left = [flow_row_for(1, 1, 1, 2, 1, 1)];
        let old_right = [flow_row_for(2, 1, 2, 2, 1, 1)];
        let delta_left = [flow_row_for(1, 1, 3, 2, 2, 1)];
        let delta_right = [flow_row_for(2, 1, 4, 2, 2, 1)];
        let actual = incremental_join(
            &old_left,
            &delta_left,
            &old_right,
            &delta_right,
            |input| (*input).cast_unsigned(),
            |input| (*input).cast_unsigned(),
            |left, right| left + right,
        )
        .unwrap_or_else(|_| panic!("join"));
        let mut mutant = join_checked(
            &delta_left,
            &old_right,
            |input| (*input).cast_unsigned(),
            |input| (*input).cast_unsigned(),
            |left, right| left + right,
        )
        .unwrap_or_else(|_| panic!("join"));
        mutant.extend(
            join_checked(
                &old_left,
                &delta_right,
                |input| (*input).cast_unsigned(),
                |input| (*input).cast_unsigned(),
                |left, right| left + right,
            )
            .unwrap_or_else(|_| panic!("join")),
        );
        assert_ne!(
            actual,
            model_consolidate(mutant),
            "dropping the delta/delta cross term must be red"
        );
    }

    #[test]
    fn subscription_root_and_sequence_fences_are_observable() {
        let first = Microbatch::seal(&[flow_row(1, 1, 1, 1)]).unwrap_or_else(|_| panic!("batch"));
        let second = Microbatch::seal(&[flow_row(2, 2, 2, 1)]).unwrap_or_else(|_| panic!("batch"));
        let mut arrangement = Arrangement::new();
        arrangement
            .append(first)
            .unwrap_or_else(|_| panic!("append"));
        let old_root = arrangement.root();
        let old_sequence = arrangement.subscribe().sequence();
        arrangement
            .append(second)
            .unwrap_or_else(|_| panic!("append"));
        let mut delta_subscription = arrangement
            .subscribe_from(old_root, old_sequence)
            .unwrap_or_else(|_| panic!("subscription"));
        assert!(matches!(
            delta_subscription.poll(),
            Some(backend_flow::SubscriptionEvent::Delta { sequence: 2, .. })
        ));

        let reset_root = arrangement.root();
        let wrong_root = {
            let mut other = Arrangement::new();
            other
                .append(
                    Microbatch::seal(&[flow_row(99, 99, 1, 1)]).unwrap_or_else(|_| panic!("batch")),
                )
                .unwrap_or_else(|_| panic!("append"));
            other.root()
        };
        let mut reset = arrangement
            .subscribe_from(wrong_root, 0)
            .unwrap_or_else(|_| panic!("reset subscription"));
        let reset_event = reset.poll();
        let mutant_event = mutant_without_subscription_fence(&arrangement, wrong_root, 0);
        assert!(
            matches!(reset_event.as_ref(), Some(backend_flow::SubscriptionEvent::Reset { snapshot }) if snapshot.root() == reset_root),
            "unexpected reset event {reset_event:?} for root {reset_root:?}"
        );
        assert_ne!(
            reset_event,
            Some(mutant_event),
            "ignoring a mismatched root must change the event kind"
        );
        assert!(matches!(
            arrangement.cursor_from(wrong_root, 99),
            Err(backend_flow::FlowError::Gap)
        ));
        assert!(matches!(
            arrangement.cursor_from(wrong_root, 0),
            Err(backend_flow::FlowError::ResetRequired)
        ));

        let mut short = Arrangement::new();
        short.set_history_limit(1);
        short
            .append(Microbatch::seal(&[flow_row(1, 1, 1, 1)]).unwrap_or_else(|_| panic!("batch")))
            .unwrap_or_else(|_| panic!("append"));
        let stale_root = short.root();
        let stale_seq = short.subscribe().sequence();
        short
            .append(Microbatch::seal(&[flow_row(2, 2, 2, 1)]).unwrap_or_else(|_| panic!("batch")))
            .unwrap_or_else(|_| panic!("append"));
        short
            .append(Microbatch::seal(&[flow_row(3, 3, 3, 1)]).unwrap_or_else(|_| panic!("batch")))
            .unwrap_or_else(|_| panic!("append"));
        let mut reset = short
            .subscribe_from(stale_root, stale_seq)
            .unwrap_or_else(|_| panic!("stale subscription"));
        assert!(matches!(
            reset.poll(),
            Some(backend_flow::SubscriptionEvent::Reset { .. })
        ));
    }

    #[test]
    fn independent_oracles_have_distinct_fault_ingredients() {
        let mut counters = WorkCounters::default();
        let input = [flow_row(1, 1, 1, 1)];
        let _ = backend_flow::join_checked_counted(
            &input,
            &input,
            |input| (*input).cast_unsigned(),
            |input| (*input).cast_unsigned(),
            |left, right| left + right,
            &mut counters,
        )
        .unwrap_or_else(|_| panic!("counted join"));
        assert_eq!(counters.join_rows, 1);
        let mut graph = DemandGraph::default();
        let first = work_key(30);
        let second = work_key(31);
        graph.depends_on(first, second);
        graph.suppress_noop(second);
        assert_eq!(graph.ready(&BTreeSet::new()), Some(first));
    }
}
