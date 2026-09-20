//! Correctness-first structural performance contracts.
//!
//! These checks compare the optimized APIs with deliberately separate
//! BTreeMap/weighted-row models. They assert bounded work counters and
//! resource conservation; they never make a wall-clock claim.

#![forbid(unsafe_code)]

use backend_execution::{
    AuthorityValidationError, AuthorityVersion, Budget, CompletionCost, CostObservation,
    CostSnapshot, EnvelopeBudgets, HedgeSide, LocalCapability, LocalState, ObservationError,
    OutputAdmission, OutputEquivalence, OutputValidationError, OutputVersion, PlacementClass,
    RemoteCapability, RemoteState, ResourceVector, ResultCoverage, ResultReceipt, ReuseContext,
    ScheduleOutcome, ScheduleRequest, Scheduled, Scheduler, UntrustedAuthorityClaim,
    UntrustedResultReceipt, VersionedWorkIdentity,
};
use backend_flow::{
    Arrangement, Delta as FlowDelta, Epoch, Microbatch, ObjectIdentity, RelationIdentity, RowKey,
    Time, WorkCounters, filter_checked, incremental_join, join_checked_counted,
};
use backend_performance_tests::{Lcg, key, map_entries, map_fixture, percentile, value};
use backend_replication::{
    AuthorityClaim, AuthorityEpoch, ByteRange, ChunkChain, ChunkParts, Frame, ObjectKey,
    ObjectRequest, ObjectVersion, ResumeRequest, SparseCoverage, Transfer, TransferId,
    TransportLimits,
};
use backend_store::{Change, OrderedMap, StoredValue, UpdateStats};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationVerifier, Relation, RelationState,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    partial_coverage,
};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU64;
use std::sync::Arc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn as_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn as_i64_u64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn boxed_store<T>(
    result: Result<T, backend_store::StoreError>,
) -> Result<T, Box<dyn std::error::Error>> {
    result.map_err(|error| std::io::Error::other(format!("store error: {error:?}")).into())
}

/// Fixture-only producer observation; product authorities must use the
/// engine's sealed admitted capability.
fn complete_store_scope_equality_fixture() -> Result<CoverageWitness, Box<dyn std::error::Error>> {
    let authority = AuthorityVersion::from_value(b"performance-store-authority");
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let scope = declaration.scope_root();
    let observation = UntrustedProducerObservation::new(
        *scope.as_bytes(),
        scope,
        *scope.as_bytes(),
        scope.as_bytes().to_vec(),
    );
    let admitted = admit_producer_observation(
        observation.clone(),
        &PerformanceCoverageVerifier(observation),
    )?;
    Ok(CoverageWitness::Complete(admit_complete_scope(
        declaration,
        admitted,
    )?))
}

#[derive(Clone, Debug)]
struct PerformanceCoverageVerifier(UntrustedProducerObservation);

impl ProducerObservationVerifier for PerformanceCoverageVerifier {
    type Error = &'static str;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        (observation == &self.0)
            .then_some(())
            .ok_or("producer observation mismatch")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModelMap {
    entries: BTreeMap<Vec<u8>, StoredValue>,
}

impl ModelMap {
    fn from_map(map: &OrderedMap) -> Self {
        Self {
            entries: map_entries(map).into_iter().collect(),
        }
    }

    fn apply(&mut self, changes: &[Change]) {
        assert!(
            changes.windows(2).all(|pair| pair[0].key < pair[1].key),
            "the independent model received an unordered change set"
        );
        for change in changes {
            assert_eq!(self.entries.get(&change.key), change.before.as_ref());
            match &change.after {
                Some(after) => {
                    self.entries.insert(change.key.clone(), after.clone());
                }
                None => {
                    self.entries.remove(&change.key);
                }
            }
        }
    }

    fn entries(&self) -> Vec<(Vec<u8>, StoredValue)> {
        self.entries
            .iter()
            .map(|(entry_key, entry_value)| (entry_key.clone(), entry_value.clone()))
            .collect()
    }
}

fn assert_map_matches(map: &OrderedMap, model: &ModelMap) {
    assert_eq!(map_entries(map), model.entries());
    assert_eq!(map.len(), model.entries.len());
}

fn change_for(
    model: &ModelMap,
    entry_key: Vec<u8>,
    ordinal: usize,
    delete_every: usize,
) -> Option<Change> {
    let before = model.entries.get(&entry_key).cloned();
    let after = if before.is_some() && ordinal.is_multiple_of(delete_every) {
        None
    } else {
        Some(value(100_000 + as_u64(ordinal)))
    };
    (before != after).then_some(Change {
        key: entry_key,
        before,
        after,
    })
}

fn broad_changes(model: &ModelMap, rows: usize, seed: u64, clustered: bool) -> Vec<Change> {
    let mut rng = Lcg::new(seed);
    let mut keys = BTreeSet::new();
    for _ordinal in 0..384 {
        let index = if clustered {
            rows / 2 + rng.below(768)
        } else {
            rng.below(rows + 768)
        };
        keys.insert(key(index));
    }
    keys.into_iter()
        .enumerate()
        .filter_map(|(ordinal, entry_key)| change_for(model, entry_key, ordinal, 5))
        .collect()
}

fn apply_store(
    map: &OrderedMap,
    model: &mut ModelMap,
    changes: &[Change],
) -> Result<(OrderedMap, UpdateStats), Box<dyn std::error::Error>> {
    let (next, stats) = boxed_store(map.apply_with_stats(changes))?;
    model.apply(changes);
    assert_map_matches(&next, model);
    Ok((next, stats))
}

#[test]
fn store_one_key_path_copy_is_logarithmic_and_matches_full_rebuild() -> TestResult {
    let mut prior_visits: Option<usize> = None;
    for rows in [4_096_usize, 16_384, 65_536] {
        let map = boxed_store(map_fixture(rows))?;
        let mut model = ModelMap::from_map(&map);
        let index = rows / 2;
        let changes = [Change {
            key: key(index),
            before: Some(value(as_u64(index))),
            after: Some(value(900_000 + as_u64(rows))),
        }];
        let (edited, stats) = apply_store(&map, &mut model, &changes)?;
        assert!(
            stats.visited_nodes
                <= 32usize
                    .saturating_mul(usize::try_from(rows.ilog2()).unwrap_or(usize::MAX))
                    .saturating_add(128),
            "path-copy visits exceeded logarithmic envelope: rows={rows} stats={stats:?}"
        );
        assert!(stats.copied_nodes <= stats.visited_nodes);
        assert!(stats.encoded_bytes > 0);
        assert!(stats.reused_nodes > stats.copied_nodes);
        if let Some(previous) = prior_visits {
            assert!(
                stats.visited_nodes <= previous.saturating_mul(3).saturating_add(16),
                "visits grew faster than a tree-height envelope: previous={previous} current={stats:?}"
            );
        }
        prior_visits = Some(stats.visited_nodes);

        let rebuilt = boxed_store(OrderedMap::try_from_iter(model.entries()))?;
        assert_eq!(map_entries(&edited), map_entries(&rebuilt));
        assert_eq!(edited.state_root(), rebuilt.state_root());

        let delta = boxed_store(
            map.delta_to_with_coverage(&edited, complete_store_scope_equality_fixture()?),
        )?;
        let applied = boxed_store(delta.apply(&map))?;
        assert_eq!(map_entries(&applied), model.entries());
        let inverse = boxed_store(delta.inverse())?;
        assert_eq!(
            map_entries(&boxed_store(inverse.apply(&edited))?),
            map_entries(&map)
        );
    }
    Ok(())
}

#[test]
fn store_broad_random_distributions_delete_readd_and_hot_keys_match_oracle() -> TestResult {
    let rows = 4_096;
    let original = boxed_store(map_fixture(rows))?;
    let mut model = ModelMap::from_map(&original);
    let uniform_set = broad_changes(&model, rows, 0x51_7e, false);
    let (uniform, uniform_stats) = apply_store(&original, &mut model, &uniform_set)?;
    assert!(uniform_stats.visited_nodes > 0);
    assert!(original.diff_stats(&uniform).changed_leaves > 1);

    let clustered_changes = broad_changes(&model, rows, 0x9a_31, true);
    let (mut clustered, clustered_stats) = apply_store(&uniform, &mut model, &clustered_changes)?;
    assert!(clustered_stats.visited_nodes > 0);
    assert_map_matches(&clustered, &model);

    let hot = key(rows / 3);
    if !model.entries.contains_key(&hot) {
        let insert = [Change {
            key: hot.clone(),
            before: None,
            after: Some(value(199_999)),
        }];
        let (inserted, _) = apply_store(&clustered, &mut model, &insert)?;
        clustered = inserted;
    }
    let hot_before =
        model.entries.get(&hot).cloned().ok_or_else(|| {
            std::io::Error::other("hot-key fixture was not present after insertion")
        })?;
    let hot_root = clustered.state_root();
    let delete = [Change {
        key: hot.clone(),
        before: Some(hot_before.clone()),
        after: None,
    }];
    let (deleted, _) = apply_store(&clustered, &mut model, &delete)?;
    let readd = [Change {
        key: hot.clone(),
        before: None,
        after: Some(hot_before),
    }];
    let (readded, _) = apply_store(&deleted, &mut model, &readd)?;
    assert_eq!(readded.state_root(), hot_root);
    assert_map_matches(&readded, &model);

    let mut current = readded;
    for ordinal in 0..96_usize {
        let before = model.entries.get(&hot).cloned();
        let changes = [Change {
            key: hot.clone(),
            before,
            after: Some(value(200_000 + as_u64(ordinal))),
        }];
        let (next, stats) = apply_store(&current, &mut model, &changes)?;
        assert!(stats.visited_nodes < 128);
        current = next;
    }
    let rebuilt = boxed_store(OrderedMap::try_from_iter(model.entries()))?;
    assert_eq!(map_entries(&current), map_entries(&rebuilt));
    assert_eq!(current.state_root(), rebuilt.state_root());
    Ok(())
}

fn flow_relation(seed: u8) -> RelationIdentity {
    RelationIdentity::from_value(&[seed; 32])
}

fn flow_object(seed: u8) -> ObjectIdentity {
    ObjectIdentity::from_value(&[seed; 32])
}

fn flow_row(
    relation: u8,
    object: u8,
    key_value: u64,
    payload: i64,
    epoch: u64,
    diff: i64,
) -> Result<FlowDelta<i64>, backend_flow::FlowError> {
    FlowDelta::checked(
        RowKey {
            relation: flow_relation(relation),
            object: flow_object(object),
            key: key_value,
        },
        payload,
        Time::new(Epoch(epoch), 0),
        diff,
    )
}

fn apply_flow_model(model: &mut BTreeMap<(RowKey, i64), i64>, rows: &[FlowDelta<i64>]) {
    for row in rows {
        let coordinate = (row.key, row.value);
        let next = model
            .get(&coordinate)
            .copied()
            .unwrap_or(0)
            .saturating_add(row.diff.value());
        if next == 0 {
            model.remove(&coordinate);
        } else {
            model.insert(coordinate, next);
        }
    }
}

#[test]
fn flow_incremental_batches_match_weighted_oracle_and_avoid_noop_work() -> TestResult {
    let mut arrangement = Arrangement::new();
    let mut model = BTreeMap::new();
    let initial = (0..1_024)
        .map(|index| flow_row(1, 1, index, as_i64_u64(index % 17), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    apply_flow_model(&mut model, &initial);
    arrangement.append(Microbatch::seal(&initial)?)?;
    assert_eq!(arrangement.visible(), model);

    let mut rng = Lcg::new(0x4f_6c_6f_77);
    let updates = (0..512_usize)
        .map(|ordinal| {
            let index = as_u64(rng.below(1_024));
            let payload = as_i64(rng.below(17)) + 20;
            let diff = if ordinal.is_multiple_of(4) { -1 } else { 1 };
            flow_row(1, 1, index, payload, 2, diff)
        })
        .collect::<Result<Vec<_>, _>>()?;
    apply_flow_model(&mut model, &updates);
    arrangement.append(Microbatch::seal(&updates)?)?;
    assert_eq!(arrangement.visible(), model);

    let before_visible = arrangement.visible();
    let before_work = arrangement.work_counters();
    let noop = vec![
        flow_row(1, 1, 77, 77, 3, 1)?,
        flow_row(1, 1, 77, 77, 4, -1)?,
    ];
    arrangement.append(Microbatch::seal(&noop)?)?;
    assert_eq!(arrangement.visible(), before_visible);
    let after_work = arrangement.work_counters();
    assert_eq!(after_work.input_rows - before_work.input_rows, 2);
    assert!(after_work.no_op_rows >= before_work.no_op_rows + 2);
    assert_eq!(after_work.output_rows - before_work.output_rows, 2);
    assert_eq!(after_work.root_rows, before_work.root_rows);
    assert_eq!(after_work.root_nodes, before_work.root_nodes);
    assert_eq!(after_work.root_probes, before_work.root_probes);
    assert_eq!(after_work.root_reused_nodes, before_work.root_reused_nodes);

    let mut keep_none = |_: RowKey, _: &i64| false;
    assert!(filter_checked(noop, &mut keep_none)?.is_empty());
    Ok(())
}

#[test]
fn flow_history_retention_stays_within_record_row_and_byte_caps() -> TestResult {
    let mut arrangement = Arrangement::new();
    arrangement.set_history_limit(8);
    arrangement.set_history_row_limit(16);
    arrangement.set_history_byte_limit(512);

    for ordinal in 0..64_u64 {
        let row = flow_row(1, 1, ordinal, as_i64_u64(ordinal), ordinal + 1, 1)?;
        arrangement.append(Microbatch::seal(&[row])?)?;
        assert!(arrangement.history_rows() <= 16);
        assert!(arrangement.history_bytes() <= 512);
    }
    Ok(())
}

fn join_model(
    left: &[FlowDelta<i64>],
    right: &[FlowDelta<i64>],
) -> BTreeMap<(RowKey, (i64, i64)), i64> {
    let mut output = BTreeMap::new();
    for left_row in left {
        for right_row in right {
            if left_row.value.rem_euclid(8) != right_row.value.rem_euclid(8) {
                continue;
            }
            let coordinate = (left_row.key, (left_row.value, right_row.value));
            let contribution = left_row.diff.value() * right_row.diff.value();
            let next = output.get(&coordinate).copied().unwrap_or(0) + contribution;
            if next == 0 {
                output.remove(&coordinate);
            } else {
                output.insert(coordinate, next);
            }
        }
    }
    output
}

fn add_model(
    destination: &mut BTreeMap<(RowKey, (i64, i64)), i64>,
    source: BTreeMap<(RowKey, (i64, i64)), i64>,
) {
    for (coordinate, contribution) in source {
        let next = destination.get(&coordinate).copied().unwrap_or(0) + contribution;
        if next == 0 {
            destination.remove(&coordinate);
        } else {
            destination.insert(coordinate, next);
        }
    }
}

fn output_model(rows: &[FlowDelta<(i64, i64)>]) -> BTreeMap<(RowKey, (i64, i64)), i64> {
    let mut output = BTreeMap::new();
    for row in rows {
        let coordinate = (row.key, row.value);
        let next = output.get(&coordinate).copied().unwrap_or(0) + row.diff.value();
        if next == 0 {
            output.remove(&coordinate);
        } else {
            output.insert(coordinate, next);
        }
    }
    output
}

#[test]
fn flow_join_counts_hot_fanout_and_matches_three_term_oracle() -> TestResult {
    let old_left = (0..32)
        .map(|index| flow_row(2, 1, index, as_i64_u64(index), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    let old_right = (0..128)
        .map(|index| flow_row(3, 1, index, as_i64_u64(index), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    let delta_left = vec![flow_row(2, 1, 10_000, 0, 2, 1)?];
    let delta_right = vec![flow_row(3, 1, 20_000, 16, 2, -1)?];
    let mut counters = WorkCounters::default();
    let hot_output = join_checked_counted(
        &delta_left,
        &old_right,
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |left, right| (*left, *right),
        &mut counters,
    )?;
    assert_eq!(counters.join_rows, as_u64(hot_output.len()));
    assert!(counters.join_rows >= 16, "hot-key fanout was not counted");

    let actual = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |left, right| (*left, *right),
    )?;
    let mut expected = BTreeMap::new();
    add_model(&mut expected, join_model(&delta_left, &old_right));
    add_model(&mut expected, join_model(&old_left, &delta_right));
    add_model(&mut expected, join_model(&delta_left, &delta_right));
    assert_eq!(output_model(&actual), expected);
    Ok(())
}

fn replication_limits() -> TransportLimits {
    TransportLimits {
        max_frame: 4_096,
        max_chunk: 64,
        max_object: 2_048,
        max_objects: 32,
        max_ranges: 32,
        max_capabilities: 32,
        max_key_bytes: 128,
        max_inputs: 32,
    }
}

fn replication_frames(
    bytes: &[u8],
    transfer: TransferId,
    key: ObjectKey,
    version: ObjectVersion,
    authority: AuthorityClaim,
    limits: TransportLimits,
) -> Result<Vec<Frame>, Box<dyn std::error::Error>> {
    let mut previous = ChunkChain([0; 32]);
    let mut frames = Vec::new();
    for (sequence, payload) in bytes.chunks(limits.max_chunk).enumerate() {
        let frame = Frame::new(
            transfer,
            key,
            version,
            ChunkParts {
                object_len: as_u64(bytes.len()),
                offset: as_u64(sequence * limits.max_chunk),
                sequence: as_u64(sequence),
                previous_chain: previous,
                payload: payload.to_vec(),
            },
            authority,
        )?;
        previous = frame.chain;
        frames.push(frame);
    }
    Ok(frames)
}

#[test]
fn replication_resume_reuses_checkpoint_bytes_and_accepts_exact_object() -> TestResult {
    let limits = replication_limits();
    let bytes = (0..1_024_u16)
        .map(|index| u8::try_from(index.wrapping_mul(37) & 0xff).unwrap_or_default())
        .collect::<Vec<_>>();
    let key = ObjectKey::from_value(b"performance-object".as_slice());
    let version = ObjectVersion::from_value(&bytes);
    let authority_version = AuthorityVersion::from_value(b"performance-authority");
    let authority = AuthorityClaim::from_typed(&authority_version, AuthorityEpoch(1));
    let transfer_id = TransferId::new(41)?;
    let request = ObjectRequest::whole(
        transfer_id,
        key,
        version,
        as_u64(bytes.len()),
        limits.max_ranges,
    )?;
    let frames = replication_frames(&bytes, transfer_id, key, version, authority, limits)?;
    let mut receiver = Transfer::with_limits(request.clone(), authority, limits)?;
    for index in [0_usize, 1, 3, 5] {
        receiver.stage(frames[index].clone().admit(limits)?)?;
    }
    let first_checkpoint = receiver.checkpoint();
    assert!(!first_checkpoint.coverage.is_complete(as_u64(bytes.len())));
    let retained_bytes = first_checkpoint.coverage.covered_bytes();
    assert!(retained_bytes > 0 && retained_bytes < as_u64(bytes.len()));
    assert_eq!(
        first_checkpoint
            .coverage
            .missing(as_u64(bytes.len()), limits.max_ranges)?
            .iter()
            .map(|range| range.len)
            .sum::<u64>(),
        as_u64(bytes.len()) - retained_bytes
    );
    receiver.stage(frames[0].clone().admit(limits)?)?;
    assert_eq!(
        receiver.checkpoint().chunks.len(),
        first_checkpoint.chunks.len()
    );

    let resume = ResumeRequest {
        transfer: transfer_id,
        key,
        version,
        len: as_u64(bytes.len()),
        coverage: first_checkpoint.coverage.clone(),
    };
    assert_eq!(resume.to_wire()?.admit_against(&resume, limits)?, resume);

    // Reconnects stay on the shared local path: checkpointing and resuming
    // retain the immutable chunk allocation instead of copying its payload.
    let shared_checkpoint = receiver.checkpoint_shared();
    let shared_bytes = shared_checkpoint
        .chunk_bytes_shared(0)
        .ok_or_else(|| std::io::Error::other("shared checkpoint lost staged chunk"))?;
    let resumed_shared =
        Transfer::resume_shared(request.clone(), authority, limits, shared_checkpoint)?;
    let resumed_shared_checkpoint = resumed_shared.checkpoint_shared();
    let resumed_bytes = resumed_shared_checkpoint
        .chunk_bytes_shared(0)
        .ok_or_else(|| std::io::Error::other("shared resume lost staged chunk"))?;
    assert!(Arc::ptr_eq(&shared_bytes, &resumed_bytes));

    let mut resumed = Transfer::resume(request, authority, limits, first_checkpoint.clone())?;
    for (index, frame) in frames.iter().enumerate() {
        if ![0_usize, 1, 3, 5].contains(&index) {
            resumed.stage(frame.clone().admit(limits)?)?;
        }
    }
    let accepted = resumed.accept_canonical()?;
    assert_eq!(accepted.key, key);
    assert_eq!(accepted.version, version);
    assert_eq!(accepted.bytes, bytes);
    Ok(())
}

#[derive(Debug)]
struct SchedulerRelation;

impl Relation for SchedulerRelation {
    const DOMAIN: u8 = 0x88;
    const TYPE: u16 = 1;
    type Key = u64;
    type Value = u64;

    fn encode_key(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }

    fn encode_value(value: &u64, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

fn scheduler_identity(
    seed: u64,
) -> Result<VersionedWorkIdentity<SchedulerRelation>, Box<dyn std::error::Error>> {
    let state = RelationState::<SchedulerRelation>::from_entries(
        [(seed, seed)],
        CoverageWitness::Partial(partial_coverage(seed)),
    )?;
    Ok(VersionedWorkIdentity::new(
        backend_execution::RecipeId::from_value(b"performance-recipe"),
        state.root(),
        backend_execution::ReadManifestId::from_value(b"performance-reads"),
        AuthorityVersion::from_value(b"performance-authority"),
        OutputEquivalence::from_value(b"performance-equivalence"),
    ))
}

fn scheduler_request(
    identity: VersionedWorkIdentity<SchedulerRelation>,
    local: LocalState,
    remote: RemoteState,
    class: PlacementClass,
    local_p95: u64,
    remote_p95: u64,
    hedge: bool,
) -> Result<ScheduleRequest<SchedulerRelation>, Box<dyn std::error::Error>> {
    let local = LocalCapability::admit(
        &identity,
        local,
        0,
        100,
        &|_: &VersionedWorkIdentity<SchedulerRelation>, _: LocalState| {
            Ok::<(), ObservationError>(())
        },
    )?;
    let capabilities = backend_replication::NegotiatedCapabilities {
        protocol: 1,
        schemas: Vec::new(),
        recipes: Vec::new(),
        limits: TransportLimits::default(),
        max_resources: backend_replication::ResourceEnvelope::UNBOUNDED,
    };
    let remote_state = remote;
    let remote = RemoteCapability::admit(
        &identity,
        &capabilities,
        0,
        100,
        &|_: &VersionedWorkIdentity<SchedulerRelation>,
          _: &backend_replication::NegotiatedCapabilities|
         -> Result<RemoteState, ObservationError> { Ok(remote_state) },
    )?;
    let costs = CostSnapshot::admit(
        &identity,
        CostObservation {
            local: local_p95,
            remote: CompletionCost {
                execution: remote_p95,
                input_transfer: 1,
                output_transfer: 1,
                warmup: u64::from(!remote_state.is_warm()),
                ..CompletionCost::default()
            },
            observed_at: 0,
            expires_at: 100,
            confidence_per_mille: 1_000,
        },
        &|_: &VersionedWorkIdentity<SchedulerRelation>, _: &CostObservation| {
            Ok::<(), ObservationError>(())
        },
    )?;
    Ok(
        ScheduleRequest::new(identity, 7, 10, class, local, remote, costs)
            .with_charge(64, ResourceVector::zero())
            .pure(true)
            .hedge(hedge)
            .with_fallback((class == PlacementClass::RemoteOptional).then_some(20)),
    )
}

fn validate_scheduler_output(
    identity: &VersionedWorkIdentity<SchedulerRelation>,
    output: OutputVersion,
    bytes: &[u8],
    coverage: ResultCoverage,
) -> Result<(), OutputValidationError> {
    if identity.output_equivalence != OutputEquivalence::from_value(b"performance-equivalence") {
        return Err(OutputValidationError::ContractMismatch);
    }
    if !matches!(coverage, ResultCoverage::Complete) {
        return Err(OutputValidationError::CoverageMismatch);
    }
    if bytes.is_empty() {
        return Err(OutputValidationError::Missing);
    }
    if output != OutputVersion::from_value(bytes) {
        return Err(OutputValidationError::ContentMismatch);
    }
    Ok(())
}

fn validate_scheduler_authority(
    identity: &VersionedWorkIdentity<SchedulerRelation>,
    lease: &backend_execution::AttemptLease,
    claim: &UntrustedAuthorityClaim,
    admission: &OutputAdmission,
) -> Result<(), AuthorityValidationError> {
    if claim.authority != identity.authority.to_bytes() || lease.key() != identity.work_key() {
        return Err(AuthorityValidationError::BindingMismatch);
    }
    if claim.authority_epoch == 0 || claim.revocation_version == 0 {
        return Err(AuthorityValidationError::Revoked);
    }
    if !matches!(admission.coverage(), ResultCoverage::Complete) {
        return Err(AuthorityValidationError::BindingMismatch);
    }
    Ok(())
}

fn scheduler_receipt(
    identity: VersionedWorkIdentity<SchedulerRelation>,
    lease: &backend_execution::AttemptLease,
    bytes: &[u8],
) -> Result<ResultReceipt<SchedulerRelation>, Box<dyn std::error::Error>> {
    let output = OutputVersion::from_value(bytes);
    Ok(ResultReceipt::admit_wire(
        identity,
        lease,
        UntrustedResultReceipt {
            key: identity.work_key().to_bytes(),
            input: identity.input.to_bytes(),
            recipe: identity.recipe.to_bytes(),
            read_manifest: identity.read_manifest.to_bytes(),
            authority: identity.authority.to_bytes(),
            output_equivalence: identity.output_equivalence.to_bytes(),
            output: output.to_bytes(),
            output_canonical_bytes: Arc::new(bytes.to_vec()),
            coverage: ResultCoverage::Complete,
            ordinal: lease.ordinal(),
            fence: *lease.fence().as_bytes(),
            authority_epoch: 1,
            revocation_version: 1,
            attestation: None,
        },
        &validate_scheduler_output,
        &validate_scheduler_authority,
    )?)
}

fn schedule_leader_and_verify_duplicate(
    scheduler: &Scheduler,
    request: &ScheduleRequest<SchedulerRelation>,
) -> Result<Box<Scheduled<SchedulerRelation>>, Box<dyn std::error::Error>> {
    let leader = match scheduler.schedule_or_reuse(*request)? {
        ScheduleOutcome::Scheduled(leader) => leader,
        ScheduleOutcome::Reused(_) => {
            return Err(std::io::Error::other("first request unexpectedly reused").into());
        }
        ScheduleOutcome::Waiting(_) => {
            return Err(std::io::Error::other("first request unexpectedly coalesced").into());
        }
    };
    if !matches!(
        scheduler.schedule_or_reuse(*request),
        Ok(ScheduleOutcome::Waiting(_))
    ) {
        return Err(std::io::Error::other("duplicate request was not coalesced").into());
    }
    Ok(leader)
}

fn assert_warm_reuse_is_lookup_only(
    scheduler: &Scheduler,
    request: &ScheduleRequest<SchedulerRelation>,
    receipt: &ResultReceipt<SchedulerRelation>,
    reuse_context: &ReuseContext,
    before_reuse: EnvelopeBudgets,
    published_bytes: &Arc<Vec<u8>>,
    retained_bytes: u64,
) -> TestResult {
    let reused = match scheduler.schedule_or_reuse_with_context(*request, Some(reuse_context))? {
        ScheduleOutcome::Reused(output) => output,
        ScheduleOutcome::Scheduled(_) => {
            return Err(std::io::Error::other("completed request was not reused").into());
        }
        ScheduleOutcome::Waiting(_) => {
            return Err(std::io::Error::other("completed request stayed coalesced").into());
        }
    };
    assert_eq!(reused.output(), receipt.result());
    assert_eq!(scheduler.output_lookup().len(), 1);
    assert_eq!(scheduler.admission().available_envelopes(), before_reuse);
    assert_eq!(scheduler.output_lookup().retained_bytes(), retained_bytes);
    assert!(Arc::ptr_eq(published_bytes, &reused.canonical_bytes_arc()));

    for _ in 0..64 {
        let warm = match scheduler.schedule_or_reuse_with_context(*request, Some(reuse_context))? {
            ScheduleOutcome::Reused(output) => output,
            ScheduleOutcome::Scheduled(_) => {
                return Err(std::io::Error::other("warm request unexpectedly scheduled").into());
            }
            ScheduleOutcome::Waiting(_) => {
                return Err(std::io::Error::other("warm request unexpectedly coalesced").into());
            }
        };
        assert_eq!(warm.output(), receipt.result());
        assert!(Arc::ptr_eq(published_bytes, &warm.canonical_bytes_arc()));
        assert_eq!(scheduler.admission().available_envelopes(), before_reuse);
        assert_eq!(scheduler.output_lookup().retained_bytes(), retained_bytes);
        assert_eq!(scheduler.output_lookup().len(), 1);
    }
    Ok(())
}

fn schedule_and_complete_hedge(
    scheduler: &Scheduler,
    identity: VersionedWorkIdentity<SchedulerRelation>,
) -> TestResult {
    let request = scheduler_request(
        identity,
        LocalState::Ready,
        RemoteState::Warm,
        PlacementClass::LocalPreferred,
        100,
        1,
        true,
    )?;
    let hedge = match scheduler.schedule_or_reuse(request)? {
        ScheduleOutcome::Scheduled(hedge) => hedge,
        ScheduleOutcome::Reused(_) => {
            return Err(std::io::Error::other("hedge request unexpectedly reused").into());
        }
        ScheduleOutcome::Waiting(_) => {
            return Err(std::io::Error::other("hedge request unexpectedly coalesced").into());
        }
    };
    assert!(hedge.is_hedged());
    assert!(hedge.reservations().has_local());
    assert!(hedge.reservations().has_remote());
    assert!(hedge.reservations().has_transfer());
    let held = scheduler.admission().available_envelopes();
    assert_eq!(held.interactive.operations, 15);
    assert_eq!(held.background.operations, 15);
    assert_eq!(held.background.hedges, 1);
    assert_eq!(held.transfer.operations, 15);
    let race = hedge
        .race()
        .cloned()
        .ok_or_else(|| std::io::Error::other("hedge had no race"))?;
    let receipt = scheduler_receipt(identity, hedge.lease(), b"hedge-output")?;
    scheduler.complete_side_with_reuse_policy(
        *hedge,
        HedgeSide::Remote,
        &receipt,
        10,
        true,
        NonZeroU64::new(1),
    )?;
    assert_eq!(race.winner(), Some(HedgeSide::Remote));
    assert!(race.is_cancelled(HedgeSide::Local));
    assert_eq!(
        scheduler.admission().available_envelopes(),
        scheduler_budgets()
    );
    Ok(())
}

fn scheduler_budgets() -> EnvelopeBudgets {
    let budget = Budget {
        operations: 16,
        bytes: 4_096,
        hedges: 2,
        ..Budget::zero()
    };
    EnvelopeBudgets {
        interactive: budget,
        background: budget,
        transfer: budget,
        compaction: budget,
    }
}

#[test]
fn scheduler_deduplicates_work_and_accounts_both_hedge_reservations() -> TestResult {
    let scheduler = Scheduler::with_envelopes(scheduler_budgets());
    let identity = scheduler_identity(1)?;
    let request = scheduler_request(
        identity,
        LocalState::Ready,
        RemoteState::Unavailable,
        PlacementClass::LocalPreferred,
        10,
        100,
        false,
    )?;
    let leader = schedule_leader_and_verify_duplicate(&scheduler, &request)?;
    let after_leader = scheduler.admission().available_envelopes();
    assert_eq!(after_leader.interactive.operations, 15);
    assert_eq!(after_leader.background.operations, 16);
    assert_eq!(scheduler.admission().available_envelopes(), after_leader);

    let receipt = scheduler_receipt(identity, leader.lease(), b"deduplicated-output")?;
    let completed =
        scheduler.complete_with_reuse_policy(*leader, &receipt, 10, true, NonZeroU64::new(1))?;
    assert_eq!(
        scheduler.admission().available_envelopes(),
        scheduler_budgets()
    );
    let before_reuse = scheduler.admission().available_envelopes();
    let retained_bytes = scheduler.output_lookup().retained_bytes();
    let reusable = completed
        .reusable()
        .ok_or_else(|| std::io::Error::other("reusable publication missing"))?;
    let reuse_context = reusable.context();
    let published_bytes = reusable.canonical_bytes_arc();
    assert_warm_reuse_is_lookup_only(
        &scheduler,
        &request,
        &receipt,
        &reuse_context,
        before_reuse,
        &published_bytes,
        retained_bytes,
    )?;
    schedule_and_complete_hedge(&scheduler, scheduler_identity(2)?)?;
    Ok(())
}

#[test]
fn percentile_reporter_is_deterministic_for_harness_samples() {
    assert_eq!(percentile(vec![9, 1, 4, 2], 50), 4);
    assert_eq!(percentile(vec![9, 1, 4, 2], 95), 9);
    assert_eq!(percentile(Vec::new(), 50), 0);
}

#[test]
fn replication_range_counter_uses_exact_byte_intervals() -> TestResult {
    let coverage = SparseCoverage::from_ranges([ByteRange::new(0, 4)?, ByteRange::new(8, 2)?], 8)?;
    let missing = coverage.missing(12, 8)?;
    assert_eq!(missing, vec![ByteRange::new(4, 4)?, ByteRange::new(10, 2)?]);
    assert_eq!(missing.iter().map(|range| range.len).sum::<u64>(), 6);
    Ok(())
}
