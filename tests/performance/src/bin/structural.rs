//! Reproducible structural benchmark harness.
//!
//! Timing distributions are descriptive. Every workload also checks an
//! independent semantic result and prints the public work/resource counters
//! that explain the measured operation.

#![forbid(unsafe_code)]

use backend_execution::{
    AuthorityValidationError, AuthorityVersion, CompletionCost, CostObservation, EnvelopeBudgets,
    HedgeSide, LocalCapability, LocalState, ObservationError, OutputAdmission, OutputEquivalence,
    OutputValidationError, OutputVersion, PlacementClass, RemoteCapability, RemoteState,
    ResourceVector, ResultCoverage, ResultReceipt, ScheduleOutcome, ScheduleRequest, Scheduler,
    UntrustedAuthorityClaim, UntrustedResultReceipt, VersionedWorkIdentity,
};
use backend_flow::{
    Arrangement, Delta as FlowDelta, Epoch, Microbatch, ObjectIdentity, RelationIdentity, RowKey,
    Time, WorkCounters, incremental_join, join_checked_counted,
};
use backend_performance_tests::{Lcg, key, map_entries, map_fixture, percentile, value};
use backend_replication::{
    AuthorityClaim, AuthorityEpoch, ChunkChain, ChunkParts, Frame, ObjectKey, ObjectRequest,
    ObjectVersion, ResumeRequest, Transfer, TransferId, TransportLimits,
};
use backend_store::{Change, OrderedMap, StoredValue, UpdateStats};
use backend_version::{CoverageWitness, Relation, RelationState, partial_coverage};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;

type BenchResult = Result<(), Box<dyn std::error::Error>>;

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn as_i64_u64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn boxed_store<T>(
    result: Result<T, backend_store::StoreError>,
) -> Result<T, Box<dyn std::error::Error>> {
    result.map_err(|error| std::io::Error::other(format!("store error: {error:?}")).into())
}

fn samples<F>(repetitions: usize, mut operation: F) -> Result<Vec<u128>, Box<dyn std::error::Error>>
where
    F: FnMut() -> Result<(), Box<dyn std::error::Error>>,
{
    let mut timings = Vec::with_capacity(repetitions);
    for _ in 0..repetitions {
        let started = Instant::now();
        operation()?;
        timings.push(started.elapsed().as_nanos());
    }
    Ok(timings)
}

fn report(name: &str, rows: usize, timings: Vec<u128>, counters: &str) {
    println!(
        "workload={name} rows={rows} repetitions={} p50_ns={} p95_ns={} p99_ns={} {counters}",
        timings.len(),
        percentile(timings.clone(), 50),
        percentile(timings.clone(), 95),
        percentile(timings, 99),
    );
}

fn map_model_apply(
    model: &mut BTreeMap<Vec<u8>, StoredValue>,
    changes: &[Change],
) -> Result<(), Box<dyn std::error::Error>> {
    for change in changes {
        if model.get(&change.key) != change.before.as_ref() {
            return Err(std::io::Error::other("benchmark model before mismatch").into());
        }
        match &change.after {
            Some(after) => {
                model.insert(change.key.clone(), after.clone());
            }
            None => {
                model.remove(&change.key);
            }
        }
    }
    Ok(())
}

fn broad_store_changes(
    model: &BTreeMap<Vec<u8>, StoredValue>,
    rows: usize,
    seed: u64,
    clustered: bool,
) -> Vec<Change> {
    let mut rng = Lcg::new(seed);
    let mut indices = (0..384)
        .map(|_| {
            if clustered {
                rows / 2 + rng.below(768)
            } else {
                rng.below(rows + 768)
            }
        })
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    indices
        .into_iter()
        .enumerate()
        .filter_map(|(ordinal, index)| {
            let change_key = key(index);
            let before = model.get(&change_key).cloned();
            let after = if before.is_some() && ordinal.is_multiple_of(5) {
                None
            } else {
                Some(value(300_000 + as_u64(ordinal)))
            };
            (before != after).then_some(Change {
                key: change_key,
                before,
                after,
            })
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn store_benchmarks(repetitions: usize) -> BenchResult {
    for rows in [16_384_usize, 65_536] {
        let base = boxed_store(map_fixture(rows))?;
        let base_model = map_entries(&base).into_iter().collect::<BTreeMap<_, _>>();
        let index = rows / 2;
        let one_key = [Change {
            key: key(index),
            before: Some(value(as_u64(index))),
            after: Some(value(900_000 + as_u64(rows))),
        }];
        let one_expected = {
            let mut model = base_model.clone();
            map_model_apply(&mut model, &one_key)?;
            model.into_iter().collect::<Vec<_>>()
        };
        let mut one_stats = UpdateStats::default();
        let timings = samples(repetitions, || {
            let (next, stats) = boxed_store(base.apply_with_stats(&one_key))?;
            one_stats.visited_nodes = one_stats.visited_nodes.saturating_add(stats.visited_nodes);
            one_stats.copied_nodes = one_stats.copied_nodes.saturating_add(stats.copied_nodes);
            one_stats.reused_nodes = one_stats.reused_nodes.saturating_add(stats.reused_nodes);
            one_stats.encoded_bytes = one_stats.encoded_bytes.saturating_add(stats.encoded_bytes);
            if map_entries(&next) != one_expected {
                return Err(std::io::Error::other("one-key update mismatch").into());
            }
            std::hint::black_box(next);
            Ok(())
        })?;
        report(
            "store_incremental_one_key",
            rows,
            timings,
            &format!(
                "edit_rows=1 visits={} copied={} reused={} encoded_bytes={}",
                one_stats.visited_nodes / repetitions,
                one_stats.copied_nodes / repetitions,
                one_stats.reused_nodes / repetitions,
                one_stats.encoded_bytes / repetitions
            ),
        );

        let model = base_model;
        let uniform_changes = broad_store_changes(&model, rows, 0x51_7e, false);
        let clustered_changes = broad_store_changes(&model, rows, 0x9a_31, true);
        for (name, changes, clustered) in [
            ("store_broad_uniform_random", uniform_changes.clone(), false),
            ("store_broad_clustered_random", clustered_changes, true),
        ] {
            let mut broad_stats = UpdateStats::default();
            let broad_timings = samples(repetitions, || {
                let (next, stats) = boxed_store(base.apply_with_stats(&changes))?;
                let mut expected = model.clone();
                map_model_apply(&mut expected, &changes)?;
                if map_entries(&next) != expected.into_iter().collect::<Vec<_>>() {
                    return Err(std::io::Error::other("broad update mismatch").into());
                }
                broad_stats.visited_nodes = broad_stats
                    .visited_nodes
                    .saturating_add(stats.visited_nodes);
                broad_stats.copied_nodes =
                    broad_stats.copied_nodes.saturating_add(stats.copied_nodes);
                broad_stats.reused_nodes =
                    broad_stats.reused_nodes.saturating_add(stats.reused_nodes);
                broad_stats.encoded_bytes = broad_stats
                    .encoded_bytes
                    .saturating_add(stats.encoded_bytes);
                std::hint::black_box(next);
                Ok(())
            })?;
            report(
                name,
                rows,
                broad_timings,
                &format!(
                    "distribution={} edit_rows={} visits={} copied={} reused={} encoded_bytes={}",
                    if clustered { "clustered" } else { "uniform" },
                    changes.len(),
                    broad_stats.visited_nodes / repetitions,
                    broad_stats.copied_nodes / repetitions,
                    broad_stats.reused_nodes / repetitions,
                    broad_stats.encoded_bytes / repetitions
                ),
            );
        }

        let hot_key = key(rows / 3);
        let mut hot_stats = UpdateStats::default();
        let hot_timings = samples(repetitions, || {
            let mut current = base.clone();
            let mut before = value(as_u64(rows / 3));
            for ordinal in 0..64_u64 {
                let change = [Change {
                    key: hot_key.clone(),
                    before: Some(before),
                    after: Some(value(600_000 + ordinal)),
                }];
                let (next, stats) = boxed_store(current.apply_with_stats(&change))?;
                hot_stats.visited_nodes =
                    hot_stats.visited_nodes.saturating_add(stats.visited_nodes);
                hot_stats.copied_nodes = hot_stats.copied_nodes.saturating_add(stats.copied_nodes);
                hot_stats.reused_nodes = hot_stats.reused_nodes.saturating_add(stats.reused_nodes);
                hot_stats.encoded_bytes =
                    hot_stats.encoded_bytes.saturating_add(stats.encoded_bytes);
                before = change[0]
                    .after
                    .clone()
                    .ok_or_else(|| std::io::Error::other("hot-key benchmark produced no value"))?;
                current = next;
            }
            if current.get(&hot_key) != Some(&before) {
                return Err(std::io::Error::other("hot-key update mismatch").into());
            }
            std::hint::black_box(current);
            Ok(())
        })?;
        report(
            "store_hot_key_repeated",
            rows,
            hot_timings,
            &format!(
                "edit_rows=64 visits_per_edit={} copied_per_edit={} reused_per_edit={} encoded_bytes_per_edit={}",
                hot_stats.visited_nodes / repetitions.saturating_mul(64),
                hot_stats.copied_nodes / repetitions.saturating_mul(64),
                hot_stats.reused_nodes / repetitions.saturating_mul(64),
                hot_stats.encoded_bytes / repetitions.saturating_mul(64)
            ),
        );

        let delete_key = key(rows / 4);
        let old = base
            .get(&delete_key)
            .cloned()
            .ok_or_else(|| std::io::Error::other("delete fixture key missing"))?;
        let delete = [Change {
            key: delete_key.clone(),
            before: Some(old.clone()),
            after: None,
        }];
        let readd = [Change {
            key: delete_key,
            before: None,
            after: Some(old),
        }];
        let mut delete_readd_stats = UpdateStats::default();
        let delete_readd_timings = samples(repetitions, || {
            let (deleted, delete_stats) = boxed_store(base.apply_with_stats(&delete))?;
            let (restored, readd_stats) = boxed_store(deleted.apply_with_stats(&readd))?;
            delete_readd_stats.visited_nodes = delete_readd_stats
                .visited_nodes
                .saturating_add(delete_stats.visited_nodes)
                .saturating_add(readd_stats.visited_nodes);
            delete_readd_stats.copied_nodes = delete_readd_stats
                .copied_nodes
                .saturating_add(delete_stats.copied_nodes)
                .saturating_add(readd_stats.copied_nodes);
            delete_readd_stats.reused_nodes = delete_readd_stats
                .reused_nodes
                .saturating_add(delete_stats.reused_nodes)
                .saturating_add(readd_stats.reused_nodes);
            delete_readd_stats.encoded_bytes = delete_readd_stats
                .encoded_bytes
                .saturating_add(delete_stats.encoded_bytes)
                .saturating_add(readd_stats.encoded_bytes);
            if restored.state_root() != base.state_root() {
                return Err(std::io::Error::other("delete/readd root mismatch").into());
            }
            std::hint::black_box(restored);
            Ok(())
        })?;
        report(
            "store_delete_readd",
            rows,
            delete_readd_timings,
            &format!(
                "edit_rows=2 visits_per_sequence={} copied_per_sequence={} reused_per_sequence={} encoded_bytes_per_sequence={} root_reuse=history_independent",
                delete_readd_stats.visited_nodes / repetitions,
                delete_readd_stats.copied_nodes / repetitions,
                delete_readd_stats.reused_nodes / repetitions,
                delete_readd_stats.encoded_bytes / repetitions
            ),
        );

        let final_model = {
            let mut result = model;
            map_model_apply(&mut result, &uniform_changes)?;
            result
        };
        let incremental_final = {
            let (map, _) = boxed_store(base.apply_with_stats(&uniform_changes))?;
            map
        };
        let full_rebuild_timings = samples(repetitions, || {
            let rebuilt =
                boxed_store(OrderedMap::try_from_iter(final_model.iter().map(
                    |(entry_key, entry_value)| (entry_key.clone(), entry_value.clone()),
                )))?;
            if map_entries(&rebuilt) != final_model.clone().into_iter().collect::<Vec<_>>() {
                return Err(std::io::Error::other("full rebuild oracle mismatch").into());
            }
            if rebuilt.state_root() != incremental_final.state_root() {
                return Err(std::io::Error::other("incremental/rebuild root mismatch").into());
            }
            std::hint::black_box(rebuilt);
            Ok(())
        })?;
        report(
            "store_full_rebuild_oracle",
            rows,
            full_rebuild_timings,
            &format!(
                "edit_rows={} oracle=independent_BTreeMap",
                final_model.len()
            ),
        );
    }
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

fn add_join_model(
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

#[allow(
    clippy::too_many_lines,
    reason = "the structural benchmark keeps one reproducible flow scenario together"
)]
fn flow_benchmarks(repetitions: usize) -> BenchResult {
    let initial = (0..1_024)
        .map(|index| flow_row(1, 1, index, as_i64_u64(index % 17), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    let noop = vec![
        flow_row(1, 1, 77, 77, 2, 1)?,
        flow_row(1, 1, 77, 77, 3, -1)?,
    ];
    let mut expected = BTreeMap::new();
    apply_flow_model(&mut expected, &initial);
    let mut counters = WorkCounters::default();
    let timings = samples(repetitions, || {
        let mut arrangement = Arrangement::new();
        arrangement.append(Microbatch::seal(&initial)?)?;
        let before = arrangement.visible();
        let before_work = arrangement.work_counters();
        arrangement.append(Microbatch::seal(&noop)?)?;
        let after_work = arrangement.work_counters();
        if arrangement.visible() != expected
            || arrangement.visible() != before
            || after_work.root_rows != before_work.root_rows
            || after_work.output_rows - before_work.output_rows != as_u64(noop.len())
            || after_work.root_nodes != before_work.root_nodes
            || after_work.root_probes != before_work.root_probes
            || after_work.root_reused_nodes != before_work.root_reused_nodes
        {
            return Err(std::io::Error::other("flow no-op changed visible state").into());
        }
        counters = arrangement.work_counters();
        std::hint::black_box(arrangement);
        Ok(())
    })?;
    report(
        "flow_noop_work_avoidance",
        initial.len(),
        timings,
        &format!(
            "input_rows={} output_rows={} no_op_rows={} compacted_rows={} root_rows={} root_nodes={} root_probes={} root_reused_nodes={} seek_probes={}",
            counters.input_rows,
            counters.output_rows,
            counters.no_op_rows,
            counters.compacted_rows,
            counters.root_rows,
            counters.root_nodes,
            counters.root_probes,
            counters.root_reused_nodes,
            counters.seek_probes
        ),
    );

    let old_left = (0..32)
        .map(|index| flow_row(2, 1, index, as_i64_u64(index), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    let old_right = (0..128)
        .map(|index| flow_row(3, 1, index, as_i64_u64(index), 1, 1))
        .collect::<Result<Vec<_>, _>>()?;
    let delta_left = vec![flow_row(2, 1, 10_000, 0, 2, 1)?];
    let delta_right = vec![flow_row(3, 1, 20_000, 16, 2, -1)?];
    let mut join_counters = WorkCounters::default();
    let join_timings = samples(repetitions, || {
        let output = join_checked_counted(
            &delta_left,
            &old_right,
            |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
            |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
            |left, right| (*left, *right),
            &mut join_counters,
        )?;
        if output.len() < 16 {
            return Err(std::io::Error::other("hot-key join fanout was lost").into());
        }
        std::hint::black_box(output);
        Ok(())
    })?;
    report(
        "flow_hot_key_join_fanout",
        old_right.len(),
        join_timings,
        &format!(
            "delta_rows=1 join_rows={} fanout=16",
            join_counters.join_rows / as_u64(repetitions)
        ),
    );
    let joined = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |payload| u64::try_from(payload.rem_euclid(8)).unwrap_or_default(),
        |left, right| (*left, *right),
    )?;
    let mut expected_join = BTreeMap::new();
    add_join_model(&mut expected_join, join_model(&delta_left, &old_right));
    add_join_model(&mut expected_join, join_model(&old_left, &delta_right));
    add_join_model(&mut expected_join, join_model(&delta_left, &delta_right));
    if output_model(&joined) != expected_join {
        return Err(std::io::Error::other("incremental join oracle mismatch").into());
    }
    println!(
        "counter=flow_incremental_join emitted_rows={} status=verified",
        joined.len()
    );
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

fn replication_benchmark(repetitions: usize) -> BenchResult {
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
    let selected = [0_usize, 1, 3, 5];
    let mut retained_bytes = 0_u64;
    let mut resumed_bytes = 0_u64;
    let timings = samples(repetitions, || {
        let mut receiver = Transfer::with_limits(request.clone(), authority, limits)?;
        for index in selected {
            receiver.stage(frames[index].clone().admit(limits)?)?;
        }
        let checkpoint = receiver.checkpoint();
        retained_bytes = checkpoint.coverage.covered_bytes();
        let resume = ResumeRequest {
            transfer: transfer_id,
            key,
            version,
            len: as_u64(bytes.len()),
            coverage: checkpoint.coverage.clone(),
        };
        let _resume = resume.to_wire()?.admit_against(&resume, limits)?;
        let mut resumed = Transfer::resume(request.clone(), authority, limits, checkpoint)?;
        resumed_bytes = 0;
        for (index, frame) in frames.iter().enumerate() {
            if !selected.contains(&index) {
                resumed_bytes = resumed_bytes.saturating_add(as_u64(frame.payload.len()));
                resumed.stage(frame.clone().admit(limits)?)?;
            }
        }
        let accepted = resumed.accept_canonical()?;
        if accepted.bytes != bytes {
            return Err(std::io::Error::other("replication resume mismatch").into());
        }
        Ok(())
    })?;
    report(
        "replication_sparse_resume",
        bytes.len(),
        timings,
        &format!(
            "chunks={} retained_bytes={} resumed_bytes={} retransmit_bytes=0",
            frames.len(),
            retained_bytes,
            resumed_bytes
        ),
    );
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
    remote: RemoteState,
    hedge: bool,
) -> Result<ScheduleRequest<SchedulerRelation>, Box<dyn std::error::Error>> {
    let local = LocalCapability::admit(
        &identity,
        LocalState::Ready,
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
    let costs = backend_execution::CostSnapshot::admit(
        &identity,
        CostObservation {
            local: if hedge { 100 } else { 10 },
            remote: CompletionCost {
                execution: if hedge { 1 } else { 100 },
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
    Ok(ScheduleRequest::new(
        identity,
        7,
        10,
        PlacementClass::LocalPreferred,
        local,
        remote,
        costs,
    )
    .with_charge(64, ResourceVector::zero())
    .pure(true)
    .hedge(hedge))
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
    let authority_epoch = 1;
    let revocation_version = 1;
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
            authority_epoch,
            revocation_version,
            attestation: None,
        },
        &validate_scheduler_output,
        &validate_scheduler_authority,
    )?)
}

fn scheduler_benchmark_reuse(
    scheduler: &Scheduler,
    identity: VersionedWorkIdentity<SchedulerRelation>,
    request: &ScheduleRequest<SchedulerRelation>,
) -> BenchResult {
    let leader = match scheduler.schedule_or_reuse(*request)? {
        ScheduleOutcome::Scheduled(candidate) => candidate,
        ScheduleOutcome::Reused(_) => {
            return Err(std::io::Error::other("scheduler first work was reused").into());
        }
        ScheduleOutcome::Waiting(_) => {
            return Err(std::io::Error::other("scheduler first work was coalesced").into());
        }
    };
    if !matches!(
        scheduler.schedule_or_reuse(*request),
        Ok(ScheduleOutcome::Waiting(_))
    ) {
        return Err(std::io::Error::other("scheduler did not coalesce duplicate").into());
    }
    let receipt = scheduler_receipt(identity, leader.lease(), b"scheduler-output")?;
    let completion =
        scheduler.complete_with_reuse_policy(*leader, &receipt, 10, true, NonZeroU64::new(1))?;
    let reuse_context = completion
        .reusable()
        .ok_or_else(|| std::io::Error::other("scheduler reusable context missing"))?
        .context();
    if !matches!(
        scheduler.schedule_or_reuse_with_context(*request, Some(&reuse_context))?,
        ScheduleOutcome::Reused(_)
    ) {
        return Err(std::io::Error::other("scheduler output was not reusable").into());
    }
    for _ in 0..64 {
        let warm = match scheduler.schedule_or_reuse_with_context(*request, Some(&reuse_context))? {
            ScheduleOutcome::Reused(output) => output,
            ScheduleOutcome::Scheduled(_) => {
                return Err(std::io::Error::other("warm request unexpectedly scheduled").into());
            }
            ScheduleOutcome::Waiting(_) => {
                return Err(std::io::Error::other("warm request unexpectedly coalesced").into());
            }
        };
        if warm.output() != receipt.result() {
            return Err(std::io::Error::other("warm request returned a different output").into());
        }
    }
    Ok(())
}

fn scheduler_benchmark_hedge(
    scheduler: &Scheduler,
    budget: backend_execution::Budget,
) -> BenchResult {
    let hedge_identity = scheduler_identity(2)?;
    let hedge_request = scheduler_request(hedge_identity, RemoteState::Warm, true)?;
    let hedge = match scheduler.schedule_or_reuse(hedge_request)? {
        ScheduleOutcome::Scheduled(candidate) => candidate,
        ScheduleOutcome::Reused(_) => {
            return Err(std::io::Error::other("scheduler hedge was reused").into());
        }
        ScheduleOutcome::Waiting(_) => {
            return Err(std::io::Error::other("scheduler hedge was coalesced").into());
        }
    };
    let race = hedge
        .race()
        .cloned()
        .ok_or_else(|| std::io::Error::other("scheduler hedge race missing"))?;
    if !hedge.reservations().has_local()
        || !hedge.reservations().has_remote()
        || !hedge.reservations().has_transfer()
    {
        return Err(std::io::Error::other("scheduler hedge reservation missing").into());
    }
    if scheduler
        .admission()
        .available_envelopes()
        .transfer
        .operations
        != 15
    {
        return Err(std::io::Error::other("scheduler transfer reservation missing").into());
    }
    let receipt = scheduler_receipt(hedge_identity, hedge.lease(), b"hedge-output")?;
    scheduler.complete_side_with_reuse_policy(
        *hedge,
        HedgeSide::Remote,
        &receipt,
        10,
        true,
        NonZeroU64::new(1),
    )?;
    if race.winner() != Some(HedgeSide::Remote) || !race.is_cancelled(HedgeSide::Local) {
        return Err(std::io::Error::other("scheduler hedge winner mismatch").into());
    }
    let full_budgets = EnvelopeBudgets {
        interactive: budget,
        background: budget,
        transfer: budget,
        compaction: budget,
    };
    if scheduler.admission().available_envelopes() != full_budgets {
        return Err(std::io::Error::other("scheduler reservation leak").into());
    }
    Ok(())
}

fn scheduler_benchmarks(repetitions: usize) -> BenchResult {
    let budget = backend_execution::Budget {
        operations: 16,
        bytes: 4_096,
        hedges: 2,
        ..backend_execution::Budget::zero()
    };
    let timings = samples(repetitions, || {
        let scheduler = Scheduler::with_envelopes(EnvelopeBudgets {
            interactive: budget,
            background: budget,
            transfer: budget,
            compaction: budget,
        });
        let identity = scheduler_identity(1)?;
        let request = scheduler_request(identity, RemoteState::Unavailable, false)?;
        scheduler_benchmark_reuse(&scheduler, identity, &request)?;
        scheduler_benchmark_hedge(&scheduler, budget)?;
        Ok(())
    })?;
    report(
        "scheduler_dedupe_and_hedge",
        2,
        timings,
        "coalesced=1 hedge_attempts=2 local_reservation=1 remote_reservation=1 transfer_reservation=1 loser_cancelled=1",
    );
    Ok(())
}

fn run() -> BenchResult {
    let repetitions = 9;
    let revision = option_env!("GIT_COMMIT").unwrap_or("unknown");
    println!(
        "backend-performance-tests revision={revision} repetitions={repetitions} warmup=none timing=descriptive units=ns"
    );
    store_benchmarks(repetitions)?;
    flow_benchmarks(repetitions)?;
    replication_benchmark(repetitions)?;
    scheduler_benchmarks(repetitions)?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark failed: {error}");
        std::process::exit(2);
    }
}
