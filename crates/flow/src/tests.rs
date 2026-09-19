//! Behavioral and differential tests for flow semantics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use backend_version::{
    AuthorityScopeClaim, CoverageAdmissionError, ObjectVersion, ProducerObservationClaims,
    ProducerObservationVerifier, Schema, ScopeRoot, UntrustedProducerObservation,
    admit_complete_scope, admit_producer_observation,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::mem::size_of;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

fn time(epoch: u64) -> Time {
    Time::new(Epoch(epoch), 0)
}

fn relation() -> RelationIdentity {
    RelationIdentity::from_value(&[1; 32])
}

fn object() -> ObjectIdentity {
    ObjectIdentity::from_value(&[1; 32])
}

fn recipe(seed: u8) -> RecipeIdentity {
    ObjectVersion::<RecipeSchema>::from_value(&[seed; 32])
}

fn input(seed: u8) -> InputIdentity {
    ObjectVersion::<InputSchema>::from_value(&[seed; 32])
}

fn read(seed: u8) -> ReadIdentity {
    ObjectVersion::<ReadSchema>::from_value(&[seed; 32])
}

fn authority(seed: u8) -> AuthorityIdentity {
    ObjectVersion::<AuthoritySchema>::from_value(&[seed; 32])
}

fn equivalence(seed: u8) -> EquivalenceIdentity {
    ObjectVersion::<EquivalenceSchema>::from_value(&[seed; 32])
}

fn coverage() -> CoverageWitness {
    arrangement_coverage()
}

struct PublicationScope;

impl Schema for PublicationScope {
    const DOMAIN: u8 = 0x77;
    const TYPE: u16 = 0x101;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

struct PublicationVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence_digest: [u8; 32],
}

impl ProducerObservationVerifier for PublicationVerifier {
    type Error = CoverageAdmissionError;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        if observation.producer_identity() != self.producer
            || observation.scope_root() != self.scope
            || observation.context() != self.context
            || *blake3::hash(observation.evidence()).as_bytes() != self.evidence_digest
        {
            return Err(CoverageAdmissionError::ScopeMismatch {
                declared: self.scope,
                observed: observation.scope_root(),
            });
        }
        Ok(ProducerObservationClaims::new(
            self.producer,
            self.scope,
            self.context,
            self.evidence_digest,
        ))
    }
}

fn publication_coverage() -> CoverageWitness {
    let version = ObjectVersion::<PublicationScope>::from_value(b"flow-publication-fixture");
    let claim = AuthorityScopeClaim::from_object_version(version);
    let observation = UntrustedProducerObservation::new(
        [0x77; 32],
        claim.scope_root(),
        [3; 32],
        claim.scope_root().as_bytes().to_vec(),
    );
    let verifier = PublicationVerifier {
        producer: [0x77; 32],
        scope: claim.scope_root(),
        context: [3; 32],
        evidence_digest: *blake3::hash(claim.scope_root().as_bytes()).as_bytes(),
    };
    let admitted =
        admit_producer_observation(observation, &verifier).expect("publication observation");
    CoverageWitness::Complete(admit_complete_scope(claim, admitted).expect("matching scope"))
}

fn row(key: u64, value: i64, epoch: u64, diff: i64) -> Delta<i64> {
    Delta::checked(
        RowKey {
            relation: relation(),
            object: object(),
            key,
        },
        value,
        time(epoch),
        diff,
    )
    .expect("non-zero test weight")
}

fn row_oracle(rows: impl IntoIterator<Item = Delta<i64>>) -> BTreeMap<(RowKey, i64, Time), i64> {
    let mut out = BTreeMap::new();
    for row in rows {
        let map_key = (row.key, row.value, row.time);
        let next = out.get(&map_key).copied().unwrap_or(0) + row.diff.value();
        if next == 0 {
            out.remove(&map_key);
        } else {
            out.insert(map_key, next);
        }
    }
    out
}

fn row_oracle_vec(rows: impl IntoIterator<Item = Delta<i64>>) -> Vec<Delta<i64>> {
    row_oracle(rows)
        .into_iter()
        .map(|((key, value, time), diff)| {
            Delta::checked(key, value, time, diff).expect("oracle weight")
        })
        .collect()
}

#[test]
fn bounded_lending_cursor_stops_at_its_work_scope() {
    let batch =
        Microbatch::seal(&[row(1, 1, 1, 1), row(2, 2, 1, 1), row(3, 3, 1, 1)]).expect("batch");
    let mut cursor = batch.lending_cursor_bounded(WorkScope::new(2, usize::MAX));
    assert_eq!(cursor.remaining(), 2);
    assert!(cursor.next().is_some());
    assert!(cursor.next().is_some());
    assert!(cursor.next().is_none());
    assert!(cursor.is_exhausted());
    assert_eq!(cursor.work_scope().rows(), 2);
}

#[test]
fn bounded_reads_and_compaction_stop_before_unbounded_materialization() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(
            Microbatch::seal(&[row(1, 1, 1, 1), row(2, 2, 1, 1), row(3, 3, 1, 1)]).expect("batch"),
        )
        .expect("append");
    let upper = arrangement.upper().upper();
    let root_before = arrangement.root();
    let mut snapshot_scope = WorkScope::new(1, usize::MAX);
    assert_eq!(
        arrangement.snapshot_at_checked_budgeted(upper, &mut snapshot_scope),
        Err(FlowError::RecursionWorkLimit)
    );
    let mut range_scope = WorkScope::with_probes(1, usize::MAX, 1);
    assert_eq!(
        arrangement.range_seek_budgeted(row(1, 0, 0, 1).key, row(3, 0, 0, 1).key, &mut range_scope),
        Err(FlowError::RecursionWorkLimit)
    );
    let mut compact_scope = WorkScope::new(1, usize::MAX);
    assert_eq!(
        arrangement.consolidate_frontier_budgeted(Frontier::new(upper), &mut compact_scope,),
        Err(FlowError::RecursionWorkLimit)
    );
    let mut reset_scope = WorkScope::new(1, usize::MAX);
    let mut reset = arrangement
        .reset_subscription_budgeted(&mut reset_scope)
        .expect("descriptor has no row payload");
    let event = reset.poll();
    assert!(matches!(event, Some(SubscriptionEvent::Reset { .. })));
    let Some(SubscriptionEvent::Reset { snapshot }) = event else {
        return;
    };
    assert_eq!(snapshot.len(), 3);
    assert_eq!(reset_scope.rows(), 0);
    let mut page_scope = WorkScope::new(1, usize::MAX);
    assert_eq!(
        arrangement.reset_snapshot_page_budgeted(snapshot, 0, 2, &mut page_scope),
        Err(FlowError::RecursionWorkLimit)
    );
    assert_eq!(arrangement.root(), root_before);
}

#[test]
fn frontier_consolidation_resumes_after_a_bounded_page() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(
            Microbatch::seal_owned(
                (0..8_u64)
                    .map(|key| row(key, key.cast_signed(), 1, 1))
                    .collect(),
            )
            .expect("batch"),
        )
        .expect("append");
    let upper = arrangement.upper().upper();
    let before = arrangement.root();
    let mut first = WorkScope::new(2, usize::MAX);
    assert_eq!(
        arrangement.consolidate_frontier_budgeted(Frontier::new(upper), &mut first),
        Err(FlowError::RecursionWorkLimit)
    );
    assert_eq!(arrangement.root(), before);
    let mut rest = WorkScope::new(16, usize::MAX);
    arrangement
        .consolidate_frontier_budgeted(Frontier::new(upper), &mut rest)
        .expect("resume frontier merge");
    assert_eq!(arrangement.visible_rows_iter().count(), 8);
}

#[test]
fn frontier_merge_segments_stay_bounded_across_repeated_pauses() {
    let mut arrangement = Arrangement::new();
    let rows = (0..5_000_u64)
        .map(|key| row(key, key.cast_signed(), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal_owned(rows).expect("batch"))
        .expect("append");
    let root_before = arrangement.root();
    let upper = arrangement.upper().upper();
    let mut completed = false;
    for _ in 0..20 {
        let mut scope = WorkScope::new(512, usize::MAX);
        match arrangement.consolidate_frontier_budgeted(Frontier::new(upper), &mut scope) {
            Ok(()) => {
                completed = true;
                break;
            }
            Err(FlowError::RecursionWorkLimit) => {
                assert!(arrangement.retained_bytes() <= 64 * 1024 * 1024);
            }
            Err(error) => assert_eq!(error, FlowError::RecursionWorkLimit),
        }
    }
    assert!(completed);
    assert_eq!(arrangement.root(), root_before);
    assert_eq!(arrangement.visible_rows_iter().count(), 5_000);
}

#[test]
fn microbatch_is_consolidated_and_uses_shared_time_header() {
    let batch = Microbatch::seal(&[row(2, 8, 1, 1), row(1, 3, 1, 1), row(2, 8, 1, -1)])
        .expect("valid batch");
    assert_eq!(batch.len(), 1);
    assert_eq!(batch.input_len(), 3);
    assert!(batch.has_shared_time());
    assert_eq!(batch.cursor().next().map(|row| row.key.key), Some(1));
}

#[test]
fn owned_batch_and_run_share_payload_without_value_clones() {
    #[derive(Debug)]
    struct CloneProbe {
        value: i64,
        clones: Arc<AtomicUsize>,
    }
    impl PartialEq for CloneProbe {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }
    impl Eq for CloneProbe {}
    impl Clone for CloneProbe {
        fn clone(&self) -> Self {
            self.clones.fetch_add(1, Ordering::Relaxed);
            Self {
                value: self.value,
                clones: Arc::clone(&self.clones),
            }
        }
    }
    impl Ord for CloneProbe {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.value.cmp(&other.value)
        }
    }
    impl PartialOrd for CloneProbe {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl CanonicalValue for CloneProbe {
        fn encode_canonical(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&self.value.to_be_bytes());
        }
        fn encode_ordered(&self, out: &mut Vec<u8>) {
            out.extend_from_slice(&(self.value.cast_unsigned() ^ (1 << 63)).to_be_bytes());
        }
    }

    let clones = Arc::new(AtomicUsize::new(0));
    let rows = (0..8_u64)
        .map(|key| {
            Delta::checked(
                RowKey {
                    relation: relation(),
                    object: object(),
                    key,
                },
                CloneProbe {
                    value: key.cast_signed(),
                    clones: Arc::clone(&clones),
                },
                time(1),
                1,
            )
            .expect("row")
        })
        .collect();
    let batch = Microbatch::seal_owned(rows).expect("owned batch");
    assert_eq!(clones.load(Ordering::Relaxed), 0);
    let run = Run::from_batch(&batch).expect("run");
    assert_eq!(run.len(), batch.len());
    assert_eq!(clones.load(Ordering::Relaxed), 0);
}

#[test]
fn frontiers_retain_incomparable_recursive_lanes() {
    let frontier = Frontier::from_antichain([
        Time::new(Epoch(1), 1),
        Time::new(Epoch(2), 0),
        Time::new(Epoch(1), 1),
    ])
    .expect("non-empty frontier");
    assert_eq!(
        frontier.elements(),
        &[Time::new(Epoch(1), 1), Time::new(Epoch(2), 0)]
    );
    assert!(frontier.covers(Time::new(Epoch(1), 0)));
    assert!(!frontier.covers(Time::new(Epoch(1), 2)));

    let mut trace = TraceSpine::new();
    trace
        .advance_upper(Frontier::new(Time::new(Epoch(1), 1)))
        .expect("initial frontier");
    trace
        .advance_upper(frontier.clone())
        .expect("add recursive lane");
    assert_eq!(trace.upper(), frontier);
    assert_eq!(
        trace.advance_upper(Frontier::new(Time::new(Epoch(1), 2))),
        Err(FlowError::FrontierRegressed)
    );
}

#[test]
fn frontier_normalization_keeps_only_incomparable_lanes_and_has_a_bound() {
    let frontier = Frontier::from_antichain([
        Time::new(Epoch(1), 5),
        Time::new(Epoch(2), 1),
        Time::new(Epoch(3), 4),
        Time::new(Epoch(4), 0),
        Time::new(Epoch(4), 0),
    ])
    .expect("bounded antichain");
    assert_eq!(
        frontier.elements(),
        &[
            Time::new(Epoch(1), 5),
            Time::new(Epoch(2), 1),
            Time::new(Epoch(4), 0),
        ]
    );

    let too_many = (0..=Frontier::MAX_ELEMENTS)
        .map(|epoch| Time::new(Epoch(u64::try_from(epoch).expect("test epoch")), 0));
    assert_eq!(
        Frontier::from_antichain(too_many),
        Err(FlowError::RecursionWorkLimit)
    );
}

#[test]
fn frontier_normalization_charges_input_coordinates_before_sorting() {
    let mut scope = WorkScope::with_probes(3, usize::MAX, usize::MAX);
    let result =
        Frontier::from_antichain_budgeted([time(1), time(2), time(3), time(4)], &mut scope);
    assert_eq!(result, Err(FlowError::RecursionWorkLimit));
    assert_eq!(scope.rows(), 3);
}

#[test]
fn time_successor_preserves_product_order_at_iteration_boundary() {
    let terminal = Time::new(Epoch(7), u16::MAX);
    let successor = terminal.successor().expect("epoch successor");
    assert_eq!(successor, Time::new(Epoch(8), u16::MAX));
    assert!(terminal.strictly_less(successor));
}

#[test]
fn row_handles_are_rejected_by_another_batch_brand() {
    let first = Microbatch::seal(&[row(1, 1, 1, 1)]).expect("first batch");
    let second = Microbatch::seal(&[row(2, 2, 1, 1)]).expect("second batch");
    let handle = first.handle_at(0).expect("first row handle");
    assert!(first.row(handle).is_some());
    assert!(second.row(handle).is_none());
}

#[test]
fn map_and_filter_match_independent_full_recompute_for_mutations() {
    let input = vec![
        row(1, 1, 1, 1),
        row(1, 2, 1, -1),
        row(2, 3, 2, 2),
        row(2, 3, 2, -1),
        row(3, 4, 3, 1),
    ];
    let mapped = map_checked(input.clone(), &mut |_, value| value.rem_euclid(2)).expect("map");
    let expected_map = row_oracle_vec(input.iter().map(|row| Delta {
        key: row.key,
        value: row.value.rem_euclid(2),
        time: row.time,
        diff: row.diff,
    }));
    assert_eq!(mapped, expected_map);

    let filtered = filter_checked(input.clone(), &mut |_, value| *value >= 2).expect("filter");
    let expected_filter = row_oracle_vec(input.into_iter().filter(|row| row.value >= 2));
    assert_eq!(filtered, expected_filter);

    let canceling = vec![row(9, 8, 4, 1), row(9, 8, 4, -1)];
    assert!(
        map_checked(canceling.clone(), &mut |_, _| 0_i64)
            .expect("canceling map")
            .is_empty()
    );
    assert!(
        filter_checked(canceling, &mut |_, _| true)
            .expect("canceling filter")
            .is_empty()
    );
}

#[test]
fn simultaneous_join_consolidates_the_cross_term_once() {
    let old_left = vec![row(1, 10, 1, 1)];
    let old_right = vec![row(9, 10, 1, 1)];
    let delta_left = vec![row(1, 10, 2, 1), row(1, 10, 2, -1)];
    let delta_right = vec![row(9, 10, 2, 1), row(9, 10, 2, -1)];
    let out = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        |value| value.unsigned_abs(),
        |value| value.unsigned_abs(),
        |left, right| left + right,
    )
    .expect("join arithmetic");
    assert!(out.is_empty());
}

#[test]
fn incremental_join_matches_full_recompute_with_cross_and_cancellation() {
    let old_left = vec![row(1, 2, 1, 1), row(2, 4, 1, 1)];
    let old_right = vec![row(9, 2, 1, 1), row(8, 4, 1, 1)];
    let delta_left = vec![row(1, 2, 2, -1), row(1, 6, 2, 1), row(3, 4, 2, 1)];
    let delta_right = vec![row(9, 2, 2, -1), row(9, 8, 2, 1), row(7, 4, 2, 1)];
    let left_key = |value: &i64| value.rem_euclid(2).cast_unsigned();
    let right_key = |value: &i64| value.rem_euclid(2).cast_unsigned();
    let combine = |left: &i64, right: &i64| left * 10 + right;

    let mut next_left = old_left.clone();
    next_left.extend(delta_left.iter().cloned());
    let mut next_right = old_right.clone();
    next_right.extend(delta_right.iter().cloned());
    let before = join_checked(&old_left, &old_right, left_key, right_key, combine).expect("before");
    let after = join_checked(&next_left, &next_right, left_key, right_key, combine).expect("after");
    let expected = row_oracle_vec(after.into_iter().chain(before.into_iter().map(|mut row| {
        row.diff = row.diff.checked_neg().expect("inverse");
        row
    })));
    let actual = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        left_key,
        right_key,
        combine,
    )
    .expect("incremental");
    assert_eq!(actual, expected);
    assert!(actual.iter().any(|delta| delta.diff.value() > 0));
    assert!(actual.iter().any(|delta| delta.diff.value() < 0));
}

#[test]
fn counted_incremental_join_reports_consolidated_output() {
    let old_left = vec![row(1, 2, 1, 1), row(2, 4, 1, 1)];
    let old_right = vec![row(9, 2, 1, 1), row(8, 4, 1, 1)];
    let delta_left = vec![row(1, 2, 2, -1), row(1, 6, 2, 1)];
    let delta_right = vec![row(9, 2, 2, -1), row(9, 8, 2, 1)];
    let mut counters = WorkCounters::default();
    let output = incremental_join_counted(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        |value| value.rem_euclid(2).cast_unsigned(),
        |value| value.rem_euclid(2).cast_unsigned(),
        |left, right| left * 10 + right,
        &mut counters,
    )
    .expect("incremental join");
    assert!(!output.is_empty());
    assert_eq!(counters.join_rows, output.len() as u64);
    assert!(counters.join_probes > 0);
    assert!(counters.join_fanout >= output.len() as u64);
    assert!(counters.join_bytes > 0);
    assert_eq!(
        counters.join_index_rows,
        (old_right.len() + delta_right.len()) as u64
    );
}

#[test]
fn incremental_join_cursor_emits_high_fanout_one_segment_at_a_time() {
    let left = [row(7, 0, 1, 1)];
    let right = (0..100_000_u64)
        .map(|key| row(7, key.cast_signed(), 1, 1))
        .collect::<Vec<_>>();
    let mut cursor = incremental_join_cursor(
        &[],
        &left,
        &[],
        &right,
        |_| 7,
        |_| 7,
        |left, right| left + right,
    );
    let mut scope = WorkScope::new(128, 128 * size_of::<Delta<i64>>());
    let first = cursor
        .next_segment(128, &mut scope)
        .expect("first bounded segment")
        .expect("high fanout has output");
    assert_eq!(first.len(), 128);
    assert_eq!(scope.rows(), 128);
}

#[test]
fn arrangement_join_probes_large_retained_side_without_materializing_it() {
    let retained_rows = (0..100_000)
        .map(|key| row(key, key.cast_signed(), 1, 1))
        .collect::<Vec<_>>();
    let mut right = Arrangement::new();
    right
        .append(Microbatch::seal_owned(retained_rows).expect("retained batch"))
        .expect("retained arrangement");
    let left = Arrangement::<i64>::new();
    let delta_left = [row(77_777, 4, 2, 1)];
    let mut counters = WorkCounters::default();

    let output = incremental_arrangement_join_counted(
        &left,
        &delta_left,
        &right,
        &[],
        |left, right| left + right,
        &mut counters,
    )
    .expect("arrangement join");

    assert_eq!(output.len(), 1);
    assert_eq!(output[0].key.key, 77_777);
    assert_eq!(output[0].value, 77_781);
    assert_eq!(counters.join_rows, 1);
    assert_eq!(counters.join_probes, right.run_count() as u64);
    assert_eq!(counters.join_fanout, 1);
    assert!(counters.join_bytes > 0);
    assert_eq!(counters.join_index_rows, 0);
    assert_eq!(counters.seek_probes, right.run_count() as u64);
}

#[test]
fn arrangement_join_preserves_simultaneous_cancellation_law() {
    let old_left = vec![row(1, 2, 1, 1), row(2, 4, 1, 1)];
    let old_right = vec![row(1, 2, 1, 1), row(2, 4, 1, 1)];
    let delta_left = vec![row(1, 2, 2, -1), row(3, 6, 2, 1)];
    let delta_right = vec![row(1, 2, 2, -1), row(4, 8, 2, 1)];
    let mut left = Arrangement::new();
    left.append(Microbatch::seal(&old_left).expect("left batch"))
        .expect("left append");
    let mut right = Arrangement::new();
    right
        .append(Microbatch::seal(&old_right).expect("right batch"))
        .expect("right append");

    let expected = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        |value| value.cast_unsigned(),
        |value| value.cast_unsigned(),
        |left, right| left + right,
    )
    .expect("batch law");
    let actual = incremental_arrangement_join_by_key(
        &left,
        &delta_left,
        &right,
        &delta_right,
        |row| row.key,
        |row| row.key,
        |left, right| left + right,
    )
    .expect("arrangement law");
    assert_eq!(actual, expected);
}

#[test]
fn arrangement_has_levels_and_exact_snapshot() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 2, 1)]).expect("batch"))
        .expect("append");
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 2, -1)]).expect("batch"))
        .expect("append");
    assert_eq!(
        arrangement
            .snapshot_at_checked(time(2))
            .expect("snapshot")
            .len(),
        0
    );
    let report = arrangement
        .compact_budgeted(CompactionBudget {
            max_rows: 16,
            max_runs: 4,
        })
        .expect("bounded compaction");
    assert!(report.merged_runs <= 4);
    arrangement.consolidate(time(2)).expect("since compaction");
    assert_eq!(
        arrangement
            .snapshot_at_checked(time(2))
            .expect("snapshot")
            .len(),
        0
    );
    let checkpoint = arrangement.checkpoint(LayoutId::derive(b"test-layout"));
    assert_eq!(checkpoint.delta_runs.len(), arrangement.runs_at_level(0));
    assert_eq!(
        checkpoint.base_segments.len(),
        arrangement
            .run_count()
            .saturating_sub(arrangement.runs_at_level(0))
    );
}

#[test]
fn durable_checkpoint_reopens_shared_runs_and_tail_history() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-checkpoint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("store");
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 7, 1, 1)]).expect("first batch"))
        .expect("first append");
    arrangement
        .append(Microbatch::seal(&[row(2, 8, 2, 1)]).expect("second batch"))
        .expect("second append");
    let layout = LayoutId::derive(b"durable-checkpoint");
    let (checkpoint, first_write) = arrangement
        .write_checkpoint(&store, layout)
        .expect("write checkpoint");
    assert!(first_write.objects_written >= 3);
    assert!(first_write.bytes_written > 0);

    let decoder = |relation_bytes: &[u8; 32], object_bytes: &[u8; 32], key: u64| {
        if relation_bytes == relation().as_bytes() && object_bytes == object().as_bytes() {
            Ok(RowKey {
                relation: relation(),
                object: object(),
                key,
            })
        } else {
            Err(FlowError::InvalidRoot)
        }
    };
    let reopened = Arrangement::from_checkpoint(&store, &checkpoint, &decoder).expect("reopen");
    assert_eq!(reopened.root(), arrangement.root());
    assert_eq!(reopened.upper(), arrangement.upper());
    assert_eq!(reopened.visible(), arrangement.visible());
    assert_eq!(reopened.history_rows(), arrangement.history_rows());
    let lazy = checkpoint.open_lazy_root(&store).expect("lazy reopen");
    assert_eq!(
        lazy.lookup_arrangement(&(row(1, 7, 1, 1).key, 7_i64))
            .expect("lazy point lookup"),
        Some(1)
    );
    let (_, second_write) = arrangement
        .write_checkpoint(&store, layout)
        .expect("reuse checkpoint objects");
    assert_eq!(second_write.bytes_written, 0);

    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn lazy_checkpoint_reopens_hundred_thousand_rows_with_a_point_probe() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-lazy-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("store");
    let mut arrangement = Arrangement::new();
    let rows = (0..100_000_u64)
        .map(|key| row(key, i64::try_from(key).expect("value"), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal_owned(rows).expect("seal"))
        .expect("append");
    let (checkpoint, _) = arrangement
        .write_checkpoint(&store, LayoutId::derive(b"lazy-100k"))
        .expect("checkpoint");
    let lazy = checkpoint.open_lazy_root(&store).expect("open lazy root");
    let key = RowKey {
        relation: relation(),
        object: object(),
        key: 99_999,
    };
    assert_eq!(lazy.lookup_arrangement(&(key, 99_999_i64)), Ok(Some(1)));
    assert_eq!(
        lazy.root().as_bytes(),
        checkpoint.checkpoint.logical_root.as_bytes()
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn durable_checkpoint_rejects_missing_run_object() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-checkpoint-missing-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    let store = backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("store");
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 7, 1, 1)]).expect("batch"))
        .expect("append");
    let (checkpoint, _) = arrangement
        .write_checkpoint(&store, LayoutId::derive(b"missing-run"))
        .expect("write");
    let object_magic = b"LUNA_OBJECT_V1\0";
    let run_path = std::fs::read_dir(path.join("objects"))
        .expect("objects directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            let Ok(bytes) = std::fs::read(candidate) else {
                return false;
            };
            let type_at = object_magic.len() + 32 + 1;
            bytes.starts_with(object_magic)
                && bytes
                    .get(type_at..type_at + 2)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(u16::from_le_bytes)
                    == Some(RunSchema::TYPE)
        })
        .expect("run object");
    std::fs::remove_file(run_path).expect("remove");
    let decoder = |relation_bytes: &[u8; 32], object_bytes: &[u8; 32], key: u64| {
        if relation_bytes == relation().as_bytes() && object_bytes == object().as_bytes() {
            Ok(RowKey {
                relation: relation(),
                object: object(),
                key,
            })
        } else {
            Err(FlowError::InvalidRoot)
        }
    };
    assert!(Arrangement::from_checkpoint(&store, &checkpoint, &decoder).is_err());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn durable_checkpoint_rejects_a_missing_visible_descendant() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-checkpoint-descendant-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("store");
    let mut arrangement = Arrangement::new();
    let rows = (0..2_000)
        .map(|key| row(key + 1, i64::try_from(key % 31).expect("value") + 1, 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&rows).expect("batch"))
        .expect("append");
    let (checkpoint, _) = arrangement
        .write_checkpoint(&store, LayoutId::derive(b"missing-descendant"))
        .expect("write");
    let root_hex = checkpoint
        .visible_root_object
        .iter()
        .fold(String::new(), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        });
    let object_magic = b"LUNA_OBJECT_V1\0";
    let node_path = std::fs::read_dir(path.join("objects"))
        .expect("objects directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            let Some(name) = candidate.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            let Ok(bytes) = std::fs::read(candidate) else {
                return false;
            };
            let type_at = object_magic.len() + 32 + 1;
            bytes.starts_with(object_magic)
                && bytes
                    .get(type_at..type_at + 2)
                    .and_then(|bytes| bytes.try_into().ok())
                    .map(u16::from_le_bytes)
                    == Some(NodeSchema::TYPE)
                && !name.starts_with(&root_hex)
        })
        .expect("descendant node");
    std::fs::remove_file(node_path).expect("remove descendant");
    let decoder = |relation_bytes: &[u8; 32], object_bytes: &[u8; 32], key: u64| {
        if relation_bytes == relation().as_bytes() && object_bytes == object().as_bytes() {
            Ok(RowKey {
                relation: relation(),
                object: object(),
                key,
            })
        } else {
            Err(FlowError::InvalidRoot)
        }
    };
    assert!(Arrangement::from_checkpoint(&store, &checkpoint, &decoder).is_err());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn durable_checkpoint_one_row_append_writes_a_boundary_local_tree_delta() {
    let path = std::env::temp_dir().join(format!(
        "backend-flow-checkpoint-boundary-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let store = backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("store");
    let mut arrangement = Arrangement::new();
    let rows = (0..100_000)
        .map(|key| row(key + 1, i64::try_from(key % 101).expect("value"), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&rows).expect("large batch"))
        .expect("large append");
    let layout = LayoutId::derive(b"boundary-local");
    let (first, initial) = arrangement
        .write_checkpoint(&store, layout)
        .expect("first write");
    assert!(initial.rows_written >= 100_000);
    arrangement
        .append(Microbatch::seal(&[row(100_001, 7, 2, 1)]).expect("tail batch"))
        .expect("tail append");
    let (second, delta) = arrangement
        .write_checkpoint(&store, layout)
        .expect("tail write");
    assert!(delta.rows_written <= 1);
    assert!(
        delta.bytes_written < 200_000,
        "wrote {} bytes",
        delta.bytes_written
    );
    let closure = store
        .read_closure(second.closure_id())
        .expect("descriptor closure");
    assert_eq!(closure.objects().len(), 2);
    assert!(
        closure
            .objects()
            .iter()
            .any(|object| object.schema().ty() == CheckpointSchema::TYPE)
    );
    assert!(closure.objects().iter().any(|object| {
        object.schema()
            == backend_version::SchemaIdentity::of_relation::<backend_store::RawRelation>()
    }));
    let reference_index = closure
        .objects()
        .iter()
        .find(|object| {
            object.schema()
                == backend_version::SchemaIdentity::of_relation::<backend_store::RawRelation>()
        })
        .expect("reference index object");
    let edges = store
        .closure_edges(second.closure_id())
        .expect("reference index edges");
    assert!(edges.iter().any(|edge| {
        edge.from().as_bytes() == reference_index.id().as_bytes()
            && edge.to().as_bytes() == &second.visible_root_object
    }));
    assert_ne!(first.visible_root_object, second.visible_root_object);

    drop(arrangement);
    let reopened_store =
        backend_store::FileStore::open(&path, 8 * 1024 * 1024).expect("reopen store");
    let decoder = |relation_bytes: &[u8; 32], object_bytes: &[u8; 32], key: u64| {
        if relation_bytes == relation().as_bytes() && object_bytes == object().as_bytes() {
            Ok(RowKey {
                relation: relation(),
                object: object(),
                key,
            })
        } else {
            Err(FlowError::InvalidRoot)
        }
    };
    let reopened =
        Arrangement::from_checkpoint(&reopened_store, &second, &decoder).expect("hydrate");
    assert_eq!(reopened.root(), second.checkpoint.logical_root);
    assert_eq!(reopened.visible().len(), 100_001);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn since_compaction_preserves_the_since_snapshot() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 7, 1, 1)]).expect("batch"))
        .expect("append");
    arrangement
        .append(Microbatch::seal(&[row(2, 8, 3, 1)]).expect("later batch"))
        .expect("later append");
    assert_eq!(
        arrangement
            .snapshot_at_checked(time(1))
            .expect("snapshot before compaction")
            .get(&(row(1, 7, 1, 1).key, 7)),
        Some(&1)
    );
    arrangement.consolidate(time(2)).expect("consolidate");
    assert_eq!(
        arrangement
            .snapshot_at_checked(time(2))
            .expect("snapshot at since")
            .get(&(row(1, 7, 1, 1).key, 7)),
        Some(&1)
    );
}

#[test]
fn frontier_compaction_keeps_incomparable_since_lanes() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 7, 1, 1)]).expect("first batch"))
        .expect("first append");
    arrangement
        .append(Microbatch::seal(&[row(2, 8, 3, 1)]).expect("second batch"))
        .expect("second append");
    let since =
        Frontier::from_antichain([time(2), Time::new(Epoch(1), 1)]).expect("incomparable frontier");
    arrangement
        .consolidate_frontier(since.clone())
        .expect("frontier compaction");
    assert_eq!(arrangement.since_frontier(), since);
    assert_eq!(
        arrangement
            .snapshot_at_checked(Time::new(Epoch(3), 1))
            .expect("snapshot after all since lanes")
            .len(),
        2
    );
}

#[test]
fn arrangement_append_at_frontier_preserves_recursive_progress_lanes() {
    let mut arrangement = Arrangement::new();
    let upper =
        Frontier::from_antichain([time(2), Time::new(Epoch(1), 1)]).expect("incomparable frontier");
    arrangement
        .append_at_frontier(
            Microbatch::seal(&[row(1, 7, 1, 1)]).expect("batch"),
            upper.clone(),
        )
        .expect("frontier append");
    assert_eq!(arrangement.upper(), upper);
    assert_eq!(arrangement.visible().len(), 1);
}

#[test]
fn history_byte_budget_forces_a_reset_for_wide_deltas() {
    let mut arrangement = Arrangement::new();
    let initial = arrangement.root();
    arrangement.set_history_limit(64);
    arrangement.set_history_byte_limit(32);
    arrangement
        .append(Microbatch::seal(&[row(1, 7, 1, 1)]).expect("batch"))
        .expect("append");
    assert_eq!(arrangement.history_bytes(), 0);
    let subscription = arrangement
        .subscribe_from(initial, 0)
        .expect("subscription reset");
    assert!(matches!(
        subscription.events.front(),
        Some(SubscriptionEvent::Reset { .. })
    ));
}

#[test]
fn arrangement_path_copy_matches_independent_canonical_root_after_random_mutations() {
    let mut arrangement = Arrangement::new();
    let initial = (0..4_096_u64)
        .map(|key| row(key, i64::try_from(key % 31).unwrap(), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&initial).expect("initial batch"))
        .expect("initial append");

    let mut current = (0..4_096_u64)
        .map(|key| {
            (
                (
                    RowKey {
                        relation: relation(),
                        object: object(),
                        key,
                    },
                    i64::try_from(key % 31).unwrap(),
                ),
                1_i64,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut seed = 0x9e37_79b9_u64;
    for epoch in 2..80_u64 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let key = seed % 4_096;
        let old_value = current
            .iter()
            .find_map(|((row_key, value), support)| {
                (*support > 0 && row_key.key == key).then_some(*value)
            })
            .unwrap();
        let new_value = i64::try_from((seed >> 16) % 31 + 31).unwrap();
        let updates = vec![
            row(key, old_value, epoch, -1),
            row(key, new_value, epoch, 1),
        ];
        arrangement
            .append(Microbatch::seal(&updates).expect("mutation batch"))
            .expect("mutation append");
        let old_key = (updates[0].key, old_value);
        let next_old = current.remove(&old_key).unwrap();
        assert_eq!(next_old, 1);
        current.insert((updates[1].key, new_value), 1);

        let entries = current
            .iter()
            .map(|(key, support)| (*key, *support))
            .collect::<Vec<_>>();
        let expected_entries = entries
            .iter()
            .map(|(key, support)| (ArrangementKey::new(key.0, key.1), *support))
            .collect::<Vec<_>>();
        let expected =
            backend_version::canonical_root::<ArrangementRelation<i64>>(&expected_entries)
                .expect("independent canonical root")
                .commitment();
        assert_eq!(arrangement.root(), expected);
        assert_eq!(arrangement.visible(), current);
    }
    let work = arrangement.work_counters();
    assert!(work.root_rows < 4_096 * 80);
    assert!(work.root_nodes > 0);
}

#[test]
fn bounded_compaction_makes_progress_around_an_oversized_run() {
    let mut arrangement = Arrangement::new()
        .with_level_run_limit(2)
        .expect("valid level limit");
    let initial = (0..5_000_u64)
        .map(|key| row(key, i64::try_from(key).expect("payload"), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&initial).expect("large batch"))
        .expect("large append");

    // The first run is larger than the default merge budget. Newer small
    // runs must still compact with one another instead of pinning level zero
    // behind that old run forever.
    for epoch in 2..=10 {
        arrangement
            .append(
                Microbatch::seal(&[row(epoch, 10_000 + epoch.cast_signed(), epoch, 1)])
                    .expect("small batch"),
            )
            .expect("small append");
    }

    assert!(arrangement.runs_at_level(0) <= 2);
    assert!(arrangement.work_counters().compacted_rows > 0);
    assert_eq!(arrangement.visible().len(), 5_009);
    assert_eq!(
        arrangement.visible_iter().count(),
        arrangement.visible().len()
    );
}

#[test]
fn unmergeable_runs_are_promoted_without_unbounded_level_zero_growth() {
    let mut arrangement = Arrangement::new()
        .with_level_run_limit(2)
        .expect("valid level limit");
    for batch_epoch in 1..=8_u64 {
        let rows = (0..5_000_u64)
            .map(|key| {
                row(
                    key + batch_epoch * 10_000,
                    key.cast_signed(),
                    batch_epoch,
                    1,
                )
            })
            .collect::<Vec<_>>();
        arrangement
            .append(Microbatch::seal_owned(rows).expect("oversized batch"))
            .expect("append oversized batch");
        assert!(arrangement.runs_at_level(0) <= 2);
    }
    assert!(arrangement.level_count() > 1);
    assert!(arrangement.work_counters().promoted_runs > 0);
    assert_eq!(arrangement.visible_len(), 40_000);
}

#[test]
fn stalled_observer_backpressures_sustained_oversized_churn() {
    let mut arrangement = Arrangement::new()
        .with_level_run_limit(2)
        .expect("valid level limit");
    arrangement
        .set_retained_limits(12, 8 * 1024 * 1024, 8)
        .expect("retention limits");
    arrangement
        .set_promotion_debt_limit(4)
        .expect("promotion limit");
    let mut rejected = false;
    for epoch in 1..=20_u64 {
        let rows = (0..5_000_u64)
            .map(|key| row(key + epoch * 20_000, key.cast_signed(), epoch, 1))
            .collect::<Vec<_>>();
        if arrangement
            .append(Microbatch::seal_owned(rows).expect("oversized batch"))
            .is_err()
        {
            rejected = true;
            break;
        }
        assert!(arrangement.run_count() <= 12);
        assert!(arrangement.level_count() <= 8);
        assert!(arrangement.retained_bytes() <= 8 * 1024 * 1024);
    }
    assert!(
        rejected,
        "stalled retention must eventually apply backpressure"
    );
}

#[test]
fn borrowed_arrangement_iterators_preserve_range_and_visible_semantics() {
    let rows = (1..=4_u64)
        .map(|key| row(key, key.cast_signed() * 10, key, 1))
        .collect::<Vec<_>>();
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&rows).expect("batch"))
        .expect("append");

    let range = arrangement
        .iter_range(rows[1].key..=rows[2].key)
        .map(|entry| (entry.key.key, *entry.value, entry.diff.value()))
        .collect::<Vec<_>>();
    assert_eq!(range.len(), 2);
    assert!(range.iter().all(|(_, _, diff)| *diff == 1));

    let visible = arrangement
        .visible_rows_iter()
        .map(|entry| (entry.key.key, *entry.value))
        .collect::<Vec<_>>();
    assert_eq!(visible.len(), 4);
    assert_eq!(visible[0], (1, 10));
}

#[test]
fn history_row_budget_turns_large_gaps_into_explicit_resets() {
    let mut arrangement = Arrangement::new();
    arrangement.set_history_limit(64);
    arrangement.set_history_row_limit(2);
    let initial_root = arrangement.root();
    for epoch in 1..=3_u64 {
        arrangement
            .append(
                Microbatch::seal(&[
                    row(epoch, epoch.cast_signed(), epoch, 1),
                    row(epoch + 10, (epoch + 10).cast_signed(), epoch, 1),
                ])
                .expect("batch"),
            )
            .expect("append");
    }
    let mut subscription = arrangement
        .subscribe_from(initial_root, 0)
        .expect("history gap becomes reset");
    let event = subscription.poll().expect("reset event");
    assert!(matches!(event, SubscriptionEvent::Reset { .. }));
    assert_eq!(arrangement.visible_rows_iter().count(), 6);
}

#[test]
fn subscription_row_budget_prevents_an_unbounded_delta_replay() {
    let mut arrangement = Arrangement::new();
    arrangement.set_subscription_limit(8).expect("event limit");
    arrangement
        .set_subscription_row_limit(1)
        .expect("row limit");
    let root = arrangement.root();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1), row(2, 2, 1, 1)]).expect("batch"))
        .expect("append");
    let mut subscription = arrangement.subscribe_from(root, 0).expect("subscription");
    assert!(matches!(
        subscription.poll(),
        Some(SubscriptionEvent::Reset { .. })
    ));
}

#[test]
fn one_key_append_reuses_untouched_canonical_state() {
    let mut arrangement = Arrangement::new();
    let initial = (0..8_192_u64)
        .map(|key| row(key, i64::try_from(key % 31).expect("payload"), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&initial).expect("initial batch"))
        .expect("initial append");
    let retained = arrangement.clone();
    assert_eq!(retained.visible(), arrangement.visible());
    let before_root = arrangement.root();
    let before_work = arrangement.work_counters();

    arrangement
        .append(
            Microbatch::seal(&[row(4_096, i64::from(4_096 % 31), 2, 1)]).expect("one-key batch"),
        )
        .expect("one-key append");
    assert_ne!(retained.root(), arrangement.root());

    let after_work = arrangement.work_counters();
    assert_eq!(retained.root(), before_root);
    assert!(after_work.root_reused_nodes > before_work.root_reused_nodes);
    assert_eq!(
        retained.visible().get(&(
            RowKey {
                relation: relation(),
                object: object(),
                key: 4_096,
            },
            4_096_i64 % 31,
        )),
        Some(&1)
    );
    assert!(after_work.root_rows - before_work.root_rows <= 1_024);
    assert!(after_work.root_nodes - before_work.root_nodes <= 4);
    assert!(after_work.root_probes - before_work.root_probes <= 16);
    assert!(after_work.root_reused_nodes > before_work.root_reused_nodes);

    let root_after_update = arrangement.root();
    let sequence_after_update = arrangement.subscribe().sequence();
    let work_after_update = arrangement.work_counters();
    arrangement
        .append(
            Microbatch::seal(&[
                row(4_096, i64::from(4_096 % 31), 3, 1),
                row(4_096, i64::from(4_096 % 31), 4, -1),
            ])
            .expect("net no-op batch"),
        )
        .expect("net no-op append");
    let after_noop = arrangement.work_counters();
    assert_eq!(arrangement.root(), root_after_update);
    assert_eq!(arrangement.subscribe().sequence(), sequence_after_update);
    assert_eq!(after_noop.root_rows, work_after_update.root_rows);
    assert_eq!(after_noop.root_nodes, work_after_update.root_nodes);
    assert_eq!(after_noop.root_probes, work_after_update.root_probes);
    assert_eq!(
        after_noop.root_reused_nodes,
        work_after_update.root_reused_nodes
    );
}

#[test]
fn failed_nonempty_append_leaves_published_state_untouched() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 2, 1)]).expect("initial batch"))
        .expect("initial append");
    let before = arrangement.clone();
    let result = arrangement.append(Microbatch::seal(&[row(2, 2, 1, 1)]).expect("stale batch"));
    assert_eq!(result, Err(FlowError::FrontierRegressed));
    assert_eq!(arrangement.root(), before.root());
    assert_eq!(arrangement.upper(), before.upper());
    assert_eq!(arrangement.visible(), before.visible());
    assert_eq!(arrangement.work_counters(), before.work_counters());
}

#[test]
fn canonical_root_handles_leaf_split_collapse_and_empty_transition() {
    let mut arrangement = Arrangement::new();
    let initial = (0..1_026_u64)
        .map(|key| row(key, i64::try_from(key).unwrap(), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&initial).expect("initial batch"))
        .expect("initial append");

    let mut current = initial
        .iter()
        .map(|entry| ((entry.key, entry.value), 1_i64))
        .collect::<BTreeMap<_, _>>();
    let expected_root = |entries: &BTreeMap<(RowKey, i64), i64>| {
        backend_version::canonical_root::<ArrangementRelation<i64>>(
            &entries
                .iter()
                .map(|(key, support)| (ArrangementKey::new(key.0, key.1), *support))
                .collect::<Vec<_>>(),
        )
        .expect("canonical root")
        .commitment()
    };
    assert_eq!(arrangement.root(), expected_root(&current));

    let collapse = (8..1_026_u64)
        .map(|key| row(key, i64::try_from(key).unwrap(), 2, -1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&collapse).expect("collapse batch"))
        .expect("collapse append");
    for entry in collapse {
        current.remove(&(entry.key, entry.value));
    }
    assert_eq!(arrangement.root(), expected_root(&current));

    let split = (1_026..2_100_u64)
        .map(|key| row(key, i64::try_from(key).unwrap(), 3, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&split).expect("split batch"))
        .expect("split append");
    for entry in &split {
        current.insert((entry.key, entry.value), 1);
    }
    assert_eq!(arrangement.root(), expected_root(&current));

    let clear = current
        .iter()
        .map(|((key, value), _)| Delta::checked(*key, *value, time(4), -1).expect("deletion"))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&clear).expect("clear batch"))
        .expect("clear append");
    current.clear();
    assert_eq!(arrangement.root(), expected_root(&current));
    assert!(arrangement.visible().is_empty());
}

#[test]
fn frontier_subscriptions_compaction_and_pins_obey_fences() {
    let mut arrangement = Arrangement::new()
        .with_level_run_limit(2)
        .expect("valid level limit");
    arrangement.set_history_limit(1);
    let initial_root = arrangement.root();
    arrangement
        .append(Microbatch::seal(&[row(1, 10, 1, 1)]).expect("batch one"))
        .expect("append one");
    arrangement
        .append(Microbatch::seal(&[row(2, 20, 2, 1)]).expect("batch two"))
        .expect("append two");
    assert_eq!(
        arrangement.snapshot_at_checked(time(99)),
        Err(FlowError::ObservationBeyondUpper)
    );
    let pin = arrangement.pin_at(time(1)).expect("pin at retained time");
    assert_eq!(
        arrangement.consolidate(time(2)),
        Err(FlowError::PinnedFrontier)
    );
    assert!(arrangement.release_pin(pin));
    arrangement
        .consolidate(time(2))
        .expect("consolidate after release");
    assert_eq!(arrangement.since(), time(2));
    assert_eq!(
        arrangement.snapshot_at_checked(time(1)),
        Err(FlowError::ObservationOutsideRetention)
    );

    arrangement
        .append(Microbatch::seal(&[row(3, 30, 3, 1)]).expect("batch three"))
        .expect("append three");
    let mut reset = arrangement
        .subscribe_from(initial_root, 0)
        .expect("history gap gives reset");
    assert!(matches!(
        reset.poll(),
        Some(SubscriptionEvent::Reset { .. })
    ));
    let report = arrangement
        .compact_budgeted(CompactionBudget {
            max_rows: 64,
            max_runs: 2,
        })
        .expect("bounded merge");
    assert!(report.input_rows <= 64);
    assert!(arrangement.work_counters().compacted_rows >= report.input_rows);
}

#[test]
fn top_k_updates_only_support_boundary() {
    let values = vec![row(1, 4, 1, 1), row(2, 2, 1, 1)];
    let result = top_k(values, 1, |value| *value).expect("top-k");
    assert_eq!(result.first().map(|candidate| candidate.value), Some(4));
}

#[test]
fn subscriber_reset_and_gap_are_explicit() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1)]).expect("batch"))
        .expect("append");
    arrangement
        .append(Microbatch::seal(&[row(2, 2, 2, 1)]).expect("second batch"))
        .expect("second append");
    let mut other = Arrangement::new();
    other
        .append(Microbatch::seal(&[row(2, 2, 1, 1)]).expect("batch"))
        .expect("append");
    let mut reset = arrangement
        .subscribe_from(other.root(), 0)
        .expect("reset event");
    assert!(matches!(
        reset.poll(),
        Some(SubscriptionEvent::Reset { .. })
    ));
    assert!(matches!(
        arrangement.cursor_from(arrangement.root(), 3),
        Err(FlowError::Gap)
    ));
    assert!(matches!(
        arrangement.subscribe_from(arrangement.root(), 3),
        Err(FlowError::Gap)
    ));
}

#[test]
fn subscriptions_are_bounded_and_old_cursors_are_rejected() {
    let mut arrangement = Arrangement::new();
    arrangement.set_history_limit(8);
    arrangement
        .set_subscription_limit(1)
        .expect("positive subscription limit");
    let root = arrangement.root();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1)]).expect("insert"))
        .expect("append insert");
    arrangement
        .append(Microbatch::seal(&[row(2, 2, 2, 1)]).expect("insert"))
        .expect("append insert");
    let mut subscription = arrangement
        .subscribe_from(root, 0)
        .expect("bounded subscription");
    assert!(matches!(
        subscription.poll(),
        Some(SubscriptionEvent::Reset { .. })
    ));
    assert!(subscription.len() <= 1);
    assert!(matches!(
        arrangement.cursor_from(root, 0),
        Err(FlowError::ResetRequired)
    ));
}

#[test]
fn random_support_matches_btree_oracle() {
    let mut arrangement = Arrangement::new();
    let mut oracle = BTreeMap::<(RowKey, i64), i64>::new();
    let mut seed = 0x9e37_79b9_u64;
    for epoch in 1..100 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let key = seed % 7;
        let value = ((seed >> 8) % 5).cast_signed();
        let diff = if seed & 1 == 0 { 1 } else { -1 };
        let update = row(key, value, epoch, diff);
        let row_key = update.key;
        let map_key = (row_key, value);
        let next = oracle.get(&map_key).copied().unwrap_or(0) + diff;
        if next == 0 {
            oracle.remove(&map_key);
        } else {
            oracle.insert(map_key, next);
        }
        arrangement
            .append(Microbatch::seal(&[update]).expect("batch"))
            .expect("append");
        assert_eq!(arrangement.visible(), oracle);
    }
}

#[test]
fn randomized_simultaneous_join_matches_full_recompute() {
    fn aggregate(rows: &[Delta<i64>]) -> BTreeMap<(RowKey, i64), i64> {
        let mut out = BTreeMap::new();
        for row in rows {
            let key = (row.key, row.value);
            let next = out.get(&key).copied().unwrap_or(0) + row.diff.value();
            if next == 0 {
                out.remove(&key);
            } else {
                out.insert(key, next);
            }
        }
        out
    }
    fn full_join(
        rows_left: &[Delta<i64>],
        rows_right: &[Delta<i64>],
    ) -> BTreeMap<(RowKey, i64), i64> {
        aggregate(
            &join_checked(
                rows_left,
                rows_right,
                |value| value.rem_euclid(3).cast_unsigned(),
                |value| value.rem_euclid(3).cast_unsigned(),
                |left, right| left + right,
            )
            .expect("full join"),
        )
    }

    let mut seed = 17_u64;
    for epoch in 1..40 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let left_value = ((seed >> 4) % 7).cast_signed();
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let right_value = ((seed >> 4) % 7).cast_signed();
        let old_left = vec![row(1, left_value, epoch, 1)];
        let old_right = vec![row(9, right_value, epoch, 1)];
        let next_left_value = (left_value + 1) % 7;
        let next_right_value = (right_value + 2) % 7;
        let delta_left = vec![
            row(1, left_value, epoch + 1, -1),
            row(1, next_left_value, epoch + 1, 1),
        ];
        let delta_right = vec![
            row(9, right_value, epoch + 1, -1),
            row(9, next_right_value, epoch + 1, 1),
        ];
        let mut next_left = old_left.clone();
        next_left.extend(delta_left.clone());
        let mut next_right = old_right.clone();
        next_right.extend(delta_right.clone());
        let before = full_join(&old_left, &old_right);
        let after = full_join(&next_left, &next_right);
        let mut expected = after;
        for (key, diff) in before {
            let next = expected.get(&key).copied().unwrap_or(0) - diff;
            if next == 0 {
                expected.remove(&key);
            } else {
                expected.insert(key, next);
            }
        }
        let actual = aggregate(
            &incremental_join(
                &old_left,
                &delta_left,
                &old_right,
                &delta_right,
                |value| value.rem_euclid(3).cast_unsigned(),
                |value| value.rem_euclid(3).cast_unsigned(),
                |left, right| left + right,
            )
            .expect("incremental join"),
        );
        assert_eq!(actual, expected);
    }
}

#[test]
fn zero_weights_cannot_enter_a_sealed_batch() {
    assert_eq!(Weight::new(0), Err(FlowError::ZeroDiff));
    assert_eq!(
        Delta::checked(
            RowKey {
                relation: relation(),
                object: object(),
                key: 1
            },
            1_i64,
            time(1),
            0
        ),
        Err(FlowError::ZeroDiff)
    );
}

#[test]
fn pins_hold_since_until_released() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1)]).expect("batch"))
        .expect("append");
    let upper = arrangement.upper().upper();
    let pin = arrangement.pin_at(time(1)).expect("pin");
    assert_eq!(
        arrangement.consolidate(upper),
        Err(FlowError::PinnedFrontier)
    );
    assert!(arrangement.release_pin(pin));
    arrangement
        .consolidate(upper)
        .expect("release permits since");
    assert_eq!(arrangement.pin_count(), 0);
}

#[test]
fn pin_leases_are_bounded_and_expire_without_crossing_a_live_fence() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1)]).expect("batch"))
        .expect("append");
    arrangement.set_pin_limit(2).expect("pin limit");
    arrangement
        .set_pin_byte_limit(2 * size_of::<Time>())
        .expect("pin byte limit");
    let first = arrangement
        .pin_frontier_until(Frontier::new(time(1)), time(3))
        .expect("first lease");
    let second = arrangement.pin_at(time(1)).expect("second lease");
    assert_eq!(arrangement.pin_count(), 2);
    assert_eq!(arrangement.pin_bytes(), 2 * size_of::<Time>());
    assert_eq!(arrangement.pin_at(time(1)), Err(FlowError::PinCapacity));
    assert_eq!(arrangement.expire_pins(time(2)), 0);
    assert_eq!(arrangement.pin_count(), 2);
    assert_eq!(arrangement.expire_pins(time(3)), 1);
    assert_eq!(arrangement.pin_count(), 1);
    assert_eq!(arrangement.pin_bytes(), size_of::<Time>());
    assert!(arrangement.release_pin(second));
    assert!(!arrangement.release_pin(first));
    assert_eq!(arrangement.pin_count(), 0);
    assert_eq!(arrangement.pin_bytes(), 0);
}

#[test]
fn singleton_pin_minimum_is_recomputed_after_release() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 1, 1)]).expect("batch"))
        .expect("append");
    arrangement
        .append(Microbatch::seal(&[row(2, 2, 2, 1)]).expect("second batch"))
        .expect("second append");
    let old = arrangement.pin_at(time(1)).expect("old pin");
    let newer = arrangement.pin_at(time(2)).expect("newer pin");
    assert_eq!(
        arrangement.consolidate(time(2)),
        Err(FlowError::PinnedFrontier)
    );
    assert!(arrangement.release_pin(old));
    arrangement
        .consolidate(time(2))
        .expect("minimum fence advances after release");
    assert!(arrangement.release_pin(newer));
}

#[test]
fn snapshot_pages_keep_root_fence_and_bound_visible_payload() {
    let mut arrangement = Arrangement::new();
    for key in 0..32 {
        arrangement
            .append(
                Microbatch::seal(&[row(
                    key,
                    i64::try_from(key).expect("small test key"),
                    key.saturating_add(1),
                    1,
                )])
                .expect("single-row batch"),
            )
            .expect("append");
    }
    let root = arrangement.root();
    let sequence = arrangement.subscribe().sequence();
    let mut cursor = arrangement.snapshot_cursor();
    let mut scope = WorkScope::with_probes(8, 8 * size_of::<Delta<i64>>(), 8);
    let first = cursor.next_page(8, &mut scope).expect("first page");
    assert_eq!(first.root(), root);
    assert_eq!(first.sequence(), sequence);
    assert_eq!(first.offset(), 0);
    assert_eq!(first.rows().len(), 8);
    assert_eq!(first.next_offset(), Some(8));
    assert_eq!(scope.rows(), 8);
    assert_eq!(cursor.offset(), 8);

    let mut tiny = WorkScope::new(1, size_of::<Delta<i64>>());
    assert_eq!(
        cursor.next_page(8, &mut tiny),
        Err(FlowError::RecursionWorkLimit)
    );
    assert_eq!(cursor.offset(), 8);
}

#[test]
fn reset_descriptor_stays_bounded_for_a_large_visible_relation() {
    let mut arrangement = Arrangement::new();
    let rows = (0..100_000_u64)
        .map(|key| row(key, i64::try_from(key % 101).expect("value"), 1, 1))
        .collect::<Vec<_>>();
    arrangement
        .append(Microbatch::seal(&rows).expect("large batch"))
        .expect("large append");

    let snapshot = arrangement.reset_snapshot();
    assert_eq!(snapshot.len(), 100_000);
    let mut scope = WorkScope::with_probes(32, 32 * size_of::<Delta<i64>>(), 32);
    let mut cursor = arrangement
        .reset_snapshot_cursor(snapshot)
        .expect("reset cursor");
    let page = cursor.next_page(32, &mut scope).expect("first reset page");
    assert_eq!(page.rows().len(), 32);
    assert_eq!(page.next_offset(), Some(32));
    assert_eq!(scope.rows(), 32);
    assert_eq!(scope.probes(), 0);
}

#[test]
fn offset_snapshot_page_charges_only_prefix_probes_and_page_rows() {
    let mut arrangement = Arrangement::new();
    for key in 0..16 {
        arrangement
            .append(
                Microbatch::seal(&[row(
                    key,
                    i64::try_from(key).expect("small test key"),
                    key.saturating_add(1),
                    1,
                )])
                .expect("single-row batch"),
            )
            .expect("append");
    }
    let mut scope = WorkScope::with_probes(2, 2 * size_of::<Delta<i64>>(), 4);
    let page = arrangement
        .snapshot_page_budgeted(4, 2, &mut scope)
        .expect("offset page");
    assert_eq!(page.offset(), 4);
    assert_eq!(page.rows().len(), 2);
    assert_eq!(page.next_offset(), Some(6));
    assert_eq!(scope.rows(), 2);
    assert_eq!(scope.probes(), 4);
}

#[test]
fn canceled_batch_advances_frontier_without_publishing_a_sequence() {
    let mut arrangement = Arrangement::new();
    let root = arrangement.root();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 3, 1), row(1, 1, 3, -1)]).expect("canceling batch"))
        .expect("append");
    assert_eq!(arrangement.root(), root);
    assert_eq!(arrangement.work_counters().root_rows, 0);
    assert_eq!(
        arrangement.upper().upper(),
        time(3).successor().expect("upper")
    );
    let subscription = arrangement
        .subscribe_from(root, 0)
        .expect("same root and sequence");
    assert!(subscription.is_empty());
}

#[test]
fn empty_batch_cannot_regress_frontier() {
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[row(1, 1, 2, 1)]).expect("batch"))
        .expect("append");
    assert_eq!(
        arrangement.append(Microbatch::seal(&[]).expect("empty batch")),
        Err(FlowError::FrontierRegressed)
    );
}

#[test]
fn factor_inverse_and_cross_terms_are_exact() {
    let base = factor_root_from_parts(
        0,
        1,
        recipe(1),
        input(1),
        Frontier::new(time(1)),
        coverage(),
        &[],
    );
    let target = factor_root_from_parts(
        0,
        2,
        recipe(1),
        input(1),
        Frontier::new(time(2)),
        coverage(),
        &[],
    );
    let delta = FactorDelta::admit(
        base,
        target,
        input(1),
        Frontier::new(time(2)),
        vec![row(1, 9, 1, 1)],
        coverage(),
    )
    .expect("factor delta");
    let inverse = delta.inverse().expect("factor inverse");
    assert_eq!(inverse.base, target);
    assert_eq!(inverse.target, base);
    assert_eq!(inverse.changes[0].diff.value(), -1);
    let view = HigherOrderDeltaView::new(base, target, 2, vec![delta.clone()], vec![delta])
        .expect("higher order view");
    assert_eq!(view.expand().expect("expansion").len(), 1);
    let mut retained = TraceSpine::new();
    retained
        .advance_upper(Frontier::new(time(3)))
        .expect("advance upper");
    retained.advance_since(time(1)).expect("since");
    let retained_root =
        factor_root_from_trace(0, 1, recipe(1), input(1), &retained, coverage(), &[]);
    assert_ne!(retained_root, base);
}

#[test]
fn prepared_outputs_keep_typed_root_frontier_bindings() {
    let base = backend_version::canonical_empty::<ArrangementRelation<i64>>().commitment();
    let target = backend_version::canonical_empty::<ArrangementRelation<i64>>().commitment();
    let output = PreparedOutput::admit(
        base,
        target,
        Frontier::new(time(3)),
        publication_coverage(),
        Vec::<Delta<i64>>::new(),
    )
    .expect("complete output");
    assert_eq!(output.base_fence().root(), base);
    assert_eq!(output.target_fence().root(), target);
    assert_eq!(output.base_fence().frontier(), Frontier::new(time(3)));
}

#[test]
fn global_top_k_dependency_can_refresh_scores() {
    let mut state = TopKState::with_score_dependency(equivalence(4));
    state
        .apply(vec![row(1, 2, 1, 1), row(2, 3, 1, 1)], 1, |value| *value)
        .expect("top-k update");
    assert_eq!(state.score_dependency(), Some(equivalence(4)));
    state.refresh_scores(|value| -*value);
    assert_eq!(state.current(1)[0].value, 2);
}

#[test]
fn top_k_handles_ties_updates_deletes_and_global_reordering() {
    let mut state = TopKState::with_score_dependency(equivalence(9));
    let initial = state
        .apply(
            vec![row(1, 5, 1, 1), row(2, 5, 1, 1), row(3, 4, 1, 1)],
            2,
            |value| *value,
        )
        .expect("initial top-k");
    assert_eq!(
        initial
            .iter()
            .map(|candidate| candidate.key.key)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );

    let updated = state
        .apply(vec![row(3, 6, 2, 1), row(1, 5, 3, -1)], 2, |value| *value)
        .expect("update top-k");
    assert_eq!(
        updated
            .iter()
            .map(|candidate| candidate.value)
            .collect::<Vec<_>>(),
        vec![6, 5]
    );
    let before = initial;
    let changes = top_k_changes(&before, &updated, time(3)).expect("rank changes");
    assert_eq!(row_oracle(changes.clone()).len(), changes.len());
    assert!(changes.iter().any(|delta| delta.diff.value() < 0));
    assert!(changes.iter().any(|delta| delta.diff.value() > 0));

    state.set_score_dependency(equivalence(10));
    assert!(state.current(2).is_empty());
    state.refresh_scores(|value| -*value);
    let reordered = state.current(2);
    assert_eq!(reordered[0].value, 4);
    assert_eq!(reordered[1].value, 5);
}

#[test]
fn top_k_refreshes_stale_global_scores_before_next_delta() {
    let mut state = TopKState::with_score_dependency(equivalence(10));
    state
        .apply(vec![row(1, 2, 1, 1), row(2, 3, 1, 1)], 1, |value| *value)
        .expect("initial top-k");
    state.set_score_dependency(equivalence(11));
    assert!(state.current(1).is_empty());
    let current = state
        .apply(Vec::<Delta<i64>>::new(), 1, |value| -*value)
        .expect("refresh through next delta");
    assert_eq!(current.first().map(|candidate| candidate.value), Some(2));
}

#[test]
fn top_k_score_refresh_honors_a_bounded_support_scope() {
    let mut state = TopKState::with_score_dependency(equivalence(12));
    state
        .apply(
            vec![row(1, 2, 1, 1), row(2, 3, 1, 1), row(3, 4, 1, 1)],
            2,
            |value| *value,
        )
        .expect("initial top-k");
    state.set_score_dependency(equivalence(13));
    let mut budget = WorkScope::new(1, usize::MAX);
    assert_eq!(
        state.refresh_scores_budgeted(|value| -*value, &mut budget),
        Err(FlowError::RecursionWorkLimit)
    );
    state.refresh_scores(|value| -*value);
    assert_eq!(
        state.current(1).first().map(|candidate| candidate.value),
        Some(2)
    );
}

#[test]
fn top_k_changes_publish_score_and_rank_only_mutations() {
    let key = row(1, 10, 1, 1).key;
    let before = [Candidate {
        score: 4,
        key,
        value: 10_i64,
    }];
    let after = [Candidate {
        score: 5,
        key,
        value: 10_i64,
    }];
    let score_change = top_k_changes(&before, &after, time(2)).expect("score change");
    assert_eq!(score_change.len(), 2);
    assert_ne!(score_change[0].time, score_change[1].time);

    let first = Candidate {
        score: 3,
        key: row(1, 11, 1, 1).key,
        value: 11_i64,
    };
    let second = Candidate {
        score: 3,
        key: row(2, 12, 1, 1).key,
        value: 12_i64,
    };
    let rank_change = top_k_changes(&[first.clone(), second.clone()], &[second, first], time(2))
        .expect("rank change");
    assert_eq!(rank_change.len(), 4);
}

#[test]
fn distinct_consolidates_same_time_crossings() {
    let rows = vec![row(1, 3, 1, 1), row(1, 3, 1, -1)];
    assert!(distinct_checked(rows).expect("distinct batch").is_empty());
    let mut state = DistinctState::new();
    assert_eq!(state.apply(vec![row(1, 3, 1, 1)]).expect("insert").len(), 1);
    assert_eq!(
        state.apply(vec![row(1, 3, 2, -1)]).expect("delete").len(),
        1
    );
    assert!(state.members().is_empty());
}

#[test]
fn reduce_and_distinct_match_support_oracles_across_crossings() {
    let updates = vec![
        row(1, 11, 1, 2),
        row(1, 11, 2, -1),
        row(1, 11, 3, -1),
        row(2, 12, 1, 3),
        row(2, 12, 2, -2),
        row(2, 12, 3, 1),
    ];
    let reduced = reduce_rows(updates.clone()).expect("reduce");
    let mut expected = BTreeMap::new();
    expected.insert((updates[0].key, 11_i64), 0_i64);
    expected.remove(&(updates[0].key, 11_i64));
    expected.insert((updates[3].key, 12_i64), 2_i64);
    assert_eq!(reduced, expected);

    let mut state = DistinctState::new();
    assert_eq!(
        state
            .apply(vec![row(4, 1, 1, 2)])
            .expect("insert two")
            .len(),
        1
    );
    assert!(
        state
            .apply(vec![row(4, 1, 2, -1)])
            .expect("delete one")
            .is_empty()
    );
    let deletion = state.apply(vec![row(4, 1, 3, -1)]).expect("delete last");
    assert_eq!(deletion.len(), 1);
    assert_eq!(deletion[0].diff.value(), -1);
    assert!(state.members().is_empty());

    let net = vec![row(5, 2, 4, -2), row(5, 2, 4, 2)];
    assert!(
        distinct_checked(net)
            .expect("same batch crossing")
            .is_empty()
    );
}

#[test]
fn recursive_deletion_recomputes_an_scc_boundary() {
    let mut edges = BTreeMap::new();
    edges.insert(1_u64, BTreeSet::from([2_u64]));
    edges.insert(2_u64, BTreeSet::from([3_u64]));
    edges.insert(3_u64, BTreeSet::from([1_u64]));
    edges.get_mut(&3).expect("edge").remove(&1);
    let closure =
        recursive_recompute(&edges, &BTreeSet::from([3]), 64).expect("bounded rederivation");
    assert!(!closure.contains(&(3, 1)));
    assert!(closure.contains(&(1, 2)));
}

#[test]
fn recursive_barrier_matches_exact_closure_after_merge_split_and_delete() {
    let mut edges = BTreeMap::new();
    edges.insert(1_u64, BTreeSet::from([2_u64]));
    edges.insert(2_u64, BTreeSet::from([3_u64]));
    edges.insert(3_u64, BTreeSet::new());
    let merged = recursive_recompute(&edges, &BTreeSet::from([3]), 128).expect("merge closure");
    assert_eq!(merged, BTreeSet::from([(1, 2), (1, 3), (2, 3)]));

    edges.get_mut(&2).expect("middle node").clear();
    let split = recursive_recompute(&edges, &BTreeSet::from([2]), 128).expect("split closure");
    assert_eq!(split, BTreeSet::from([(1, 2)]));

    edges.get_mut(&1).expect("first node").clear();
    let deleted = recursive_recompute(&edges, &BTreeSet::from([1]), 128).expect("delete closure");
    assert!(deleted.is_empty());
}

#[test]
fn recursive_region_leaves_disjoint_components_out_of_work() {
    let mut edges = BTreeMap::new();
    edges.insert(1_u64, BTreeSet::from([2_u64]));
    edges.insert(2_u64, BTreeSet::new());
    edges.insert(9_u64, BTreeSet::from([10_u64]));
    edges.insert(10_u64, BTreeSet::new());
    let region =
        recursive_recompute_region(&edges, &BTreeSet::from([2]), 64).expect("region closure");
    assert_eq!(region, BTreeSet::from([(1, 2)]));
}

#[test]
fn recursive_budget_counts_reverse_scan_and_preserves_region_result() {
    let mut edges = BTreeMap::new();
    edges.insert(1_u64, BTreeSet::from([2_u64]));
    edges.insert(2_u64, BTreeSet::from([3_u64]));
    edges.insert(3_u64, BTreeSet::new());
    let mut budget = WorkScope::new(32, usize::MAX);
    let closure =
        recursive_recompute_region_budgeted(&edges, &BTreeSet::from([2_u64]), &mut budget)
            .expect("bounded recursive region");
    assert!(closure.contains(&(1, 3)));
    assert!(budget.rows() >= 3);

    let mut too_small = WorkScope::new(1, usize::MAX);
    assert_eq!(
        recursive_recompute_region_budgeted(&edges, &BTreeSet::from([2_u64]), &mut too_small),
        Err(FlowError::RecursionWorkLimit)
    );
}

#[test]
fn recursive_empty_delta_is_a_zero_work_transition() {
    let edges = BTreeMap::from([
        (1_u64, BTreeSet::from([2_u64])),
        (2_u64, BTreeSet::from([3_u64])),
        (3_u64, BTreeSet::new()),
    ]);
    assert_eq!(
        recursive_recompute(&edges, &BTreeSet::new(), 0).expect("empty closure"),
        BTreeSet::new()
    );
    assert_eq!(
        recursive_recompute_region(&edges, &BTreeSet::new(), 0).expect("empty region"),
        BTreeSet::new()
    );
}

#[test]
fn demand_noop_is_a_satisfied_dependency() {
    let mut graph = DemandGraph::default();
    let input_key = WorkKey::new(recipe(1), input(1), read(1), authority(1), equivalence(1));
    let output_key = WorkKey::new(recipe(2), input(2), read(2), authority(2), equivalence(2));
    graph.depends_on(output_key, input_key);
    graph.suppress_noop(input_key);
    assert_eq!(graph.ready(&BTreeSet::new()), Some(output_key));
}

#[test]
fn demand_graph_tracks_exact_identities_and_mutating_interest() {
    let mut graph = DemandGraph::default();
    let input_key = WorkKey::new(recipe(1), input(1), read(1), authority(1), equivalence(1));
    let output_key = WorkKey::new(recipe(2), input(2), read(2), authority(2), equivalence(2));
    graph.depends_on(output_key, input_key);
    graph.add_demand(Demand {
        consumer: 7,
        work: output_key,
        range: Some(4..=9),
        freshness: Frontier::new(time(5)),
        priority: 3,
    });
    assert!(graph.is_demanded(output_key));
    graph.mark_dirty(input_key);
    assert!(graph.is_dirty(input_key));
    assert_eq!(graph.ready(&BTreeSet::new()), Some(input_key));
    graph.suppress_noop(input_key);
    assert!(!graph.is_dirty(input_key));
    assert_eq!(graph.ready(&BTreeSet::new()), Some(output_key));
    assert!(graph.remove_demand(7).is_some());
    assert!(!graph.is_demanded(output_key));
    assert_eq!(graph.dependency_count(output_key), 1);
    assert_eq!(
        graph.try_depends_on(input_key, output_key),
        Err(FlowError::DependencyCycle)
    );
}

#[test]
fn variable_payload_range_is_rejected_before_clone_exceeds_budget() {
    #[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
    struct WidePayload(u8);
    impl CanonicalValue for WidePayload {
        fn encode_canonical(&self, out: &mut Vec<u8>) {
            out.push(self.0);
        }
        fn encode_ordered(&self, out: &mut Vec<u8>) {
            out.push(self.0);
        }
        fn canonical_len(&self) -> usize {
            1024 * 1024
        }
    }
    let delta = Delta::checked(
        RowKey {
            relation: relation(),
            object: object(),
            key: 1,
        },
        WidePayload(7),
        time(1),
        1,
    )
    .expect("delta");
    let mut arrangement = Arrangement::new();
    arrangement
        .append(Microbatch::seal(&[delta]).expect("batch"))
        .expect("append");
    let mut scope = WorkScope::new(1, 256);
    assert_eq!(
        arrangement.range_seek_budgeted(
            RowKey {
                relation: relation(),
                object: object(),
                key: 1,
            },
            RowKey {
                relation: relation(),
                object: object(),
                key: 1,
            },
            &mut scope,
        ),
        Err(FlowError::RecursionWorkLimit)
    );
}

#[test]
fn disjoint_hundred_thousand_cross_join_is_linear_and_charged() {
    let left = (0_u64..100_000)
        .map(|key| row(key, 1, 1, 1))
        .collect::<Vec<_>>();
    let right = (100_000_u64..200_000)
        .map(|key| row(key, 2, 1, 1))
        .collect::<Vec<_>>();
    let mut counters = WorkCounters::default();
    let output = incremental_join_counted(
        &[],
        &left,
        &[],
        &right,
        |value| value.unsigned_abs(),
        |value| value.unsigned_abs(),
        |left, right| left + right,
        &mut counters,
    )
    .expect("disjoint join");
    assert!(output.is_empty());
    assert_eq!(counters.join_fanout, 0);
    assert!(counters.join_probes <= 200_000 + 1);
}
