//! Regression cases for the v2 cutover review.
//!
//! These cases intentionally live outside the implementation crates.  Their
//! expected values are built with small standard-library models and the tests
//! consume only public APIs.  A green result therefore covers the composition
//! boundary, rather than a private helper having asserted its own counters.

#![forbid(unsafe_code)]

use backend_flow::{
    Arrangement, ArrangementKey, ArrangementRelation, CompactionBudget, Delta, Epoch, FlowError,
    Microbatch, RowKey, SubscriptionEvent, Time, WorkCounters, WorkScope, incremental_join,
    incremental_join_counted,
};
use backend_store::{OrderedMap, StoredValue, WorkBudget};
use backend_version::canonical_root;
use std::collections::BTreeMap;
use std::error::Error;
use std::mem::size_of;

fn row(key: u64, value: i64, epoch: u64, diff: i64) -> Result<Delta<i64>, FlowError> {
    Delta::checked(
        RowKey {
            relation: backend_flow::RelationIdentity::from_value(&[0x31; 32]),
            object: backend_flow::ObjectIdentity::from_value(&[0x32; 32]),
            key,
        },
        value,
        Time::new(Epoch(epoch), 0),
        diff,
    )
}

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed
}

#[test]
fn randomized_multi_leaf_orderings_match_full_rebuild_oracle() -> Result<(), Box<dyn Error>> {
    let mut rows = (0..4_096_u64)
        .map(|key| Ok(row(key, i64::try_from(key % 97)?, 1, 1)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let expected = rows
        .iter()
        .map(|item| ((item.key, item.value), item.diff.value()))
        .collect::<BTreeMap<_, _>>();
    let expected_entries = expected
        .iter()
        .map(|(key, support)| (ArrangementKey::new(key.0, key.1), *support))
        .collect::<Vec<_>>();
    let expected_root = canonical_root::<ArrangementRelation<i64>>(&expected_entries)?.commitment();

    // Every permutation is fed as an arbitrary batch, and then as shuffled
    // multi-batch input.  The expected state is a fresh BTreeMap model.
    let mut seed = 0x51f1_aa77_93c2_18d1;
    for round in 0..8_u64 {
        for index in (1..rows.len()).rev() {
            let swap = usize::try_from(lcg(&mut seed))? % (index + 1);
            rows.swap(index, swap);
        }
        let mut arrangement = Arrangement::new();
        if round % 2 == 0 {
            arrangement.append(Microbatch::seal(&rows)?)?;
        } else {
            for (chunk, part) in rows.chunks(37).enumerate() {
                // A batch's frontier must advance.  The logical root does not
                // include time, so these distinct epochs still describe the
                // same full state model.
                let epoch = u64::try_from(chunk + 1)?;
                let timed = part
                    .iter()
                    .map(|item| Ok(row(item.key.key, item.value, epoch, item.diff.value())?))
                    .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
                arrangement.append(Microbatch::seal(&timed)?)?;
            }
        }
        assert_eq!(
            arrangement.visible(),
            expected,
            "round {round} visible state"
        );
        assert_eq!(
            arrangement.root().to_bytes(),
            expected_root.to_bytes(),
            "round {round} root"
        );
    }
    Ok(())
}

#[test]
fn large_payload_is_rejected_before_a_budgeted_page_clones_it() -> Result<(), Box<dyn Error>> {
    let payload = "payload".repeat(4 * 1024);
    let key = RowKey {
        relation: backend_flow::RelationIdentity::from_value(&[0x41; 32]),
        object: backend_flow::ObjectIdentity::from_value(&[0x42; 32]),
        key: 7,
    };
    let change = Delta::checked(key, payload.clone(), Time::new(Epoch(1), 0), 1)?;
    let batch = Microbatch::seal(&[change])?;
    let mut arrangement = Arrangement::new();
    arrangement.append(batch)?;

    let mut tiny = WorkScope::new(1, 256);
    assert_eq!(
        arrangement.snapshot_page_budgeted(0, 1, &mut tiny),
        Err(FlowError::RecursionWorkLimit)
    );
    assert_eq!(tiny.rows(), 0, "failed admission must not consume a row");
    assert_eq!(tiny.bytes(), 0, "failed admission must not charge a clone");

    let mut enough = WorkScope::new(1, size_of::<Delta<String>>() + payload.len());
    let page = arrangement.snapshot_page_budgeted(0, 1, &mut enough)?;
    assert_eq!(page.rows().len(), 1);
    assert_eq!(page.rows()[0].value, payload);
    assert_eq!(enough.rows(), 1);
    assert!(enough.bytes() >= u64::try_from(payload.len())?);
    Ok(())
}

#[test]
fn one_row_delta_delivery_does_not_hydrate_the_growing_view() -> Result<(), Box<dyn Error>> {
    let mut arrangement = Arrangement::new();
    let base = (0..100_000_u64)
        .map(|key| Ok(row(key, i64::try_from(key % 11)?, 1, 1)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    arrangement.append(Microbatch::seal_owned(base)?)?;
    let base_root = arrangement.root();
    let base_sequence = arrangement.subscribe().sequence();
    arrangement.append(Microbatch::seal(&[row(100_000, 99, 2, 1)?])?)?;

    let mut subscription = arrangement.subscribe_from(base_root, base_sequence)?;
    let event = subscription
        .poll()
        .ok_or("one-row change was not delivered")?;
    let SubscriptionEvent::Delta { deltas, .. } = event else {
        return Err("one-row change must be delivered as a delta event".into());
    };
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].key.key, 100_000);
    assert!(subscription.poll().is_none());
    Ok(())
}

#[test]
fn disjoint_hundred_thousand_row_join_is_bounded_and_matches_full_oracle()
-> Result<(), Box<dyn Error>> {
    let left = (0..100_000_u64)
        .map(|key| Ok(row(key, i64::try_from(key)?, 1, 1)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let right = (100_000..200_000_u64)
        .map(|key| Ok(row(key, i64::try_from(key)?, 1, 1)?))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
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
    )?;
    assert!(output.is_empty());
    assert_eq!(counters.join_fanout, 0);
    assert!(counters.join_probes <= 200_001);
    assert!(counters.join_index_rows <= 100_000);
    Ok(())
}

#[test]
fn stalled_observer_gets_bounded_compaction_debt_then_releases_it() -> Result<(), Box<dyn Error>> {
    let mut arrangement = Arrangement::new().with_level_run_limit(2)?;
    arrangement.append(Microbatch::seal(&[row(0, 0, 1, 1)?])?)?;
    let stalled = arrangement.pin_at(Time::new(Epoch(1), 0))?;

    for epoch in 2..=65_u64 {
        arrangement.append(Microbatch::seal(&[row(
            epoch,
            i64::try_from(epoch)?,
            epoch,
            1,
        )?])?)?;
        let report = arrangement.compact_budgeted(CompactionBudget {
            max_rows: 64,
            max_runs: 2,
        })?;
        assert!(report.input_rows <= 64);
        assert!(arrangement.runs_at_level(0) <= 2);
    }
    assert_eq!(
        arrangement.consolidate(Time::new(Epoch(65), 0)),
        Err(FlowError::PinnedFrontier)
    );
    assert!(arrangement.release_pin(stalled));
    arrangement.consolidate(Time::new(Epoch(65), 0))?;
    assert_eq!(arrangement.visible_len(), 65);
    assert!(arrangement.run_count() <= arrangement.level_count() * 2);
    Ok(())
}

#[test]
fn full_recompute_shadow_and_delta_transition_have_the_same_state() -> Result<(), Box<dyn Error>> {
    let old_left = vec![row(1, 2, 1, 1)?, row(2, 3, 1, 1)?, row(3, 8, 1, 1)?];
    let old_right = vec![row(7, 2, 1, 1)?, row(8, 5, 1, 1)?, row(9, 8, 1, 1)?];
    let delta_left = vec![row(1, 2, 2, -1)?, row(1, 4, 2, 1)?, row(4, 5, 2, 1)?];
    let delta_right = vec![row(7, 2, 2, -1)?, row(7, 4, 2, 1)?, row(10, 5, 2, 1)?];
    let key = |value: &i64| value.unsigned_abs();
    let combine = |left: &i64, right: &i64| left + right;

    let mut full_left = old_left.clone();
    full_left.extend(delta_left.iter().cloned());
    let mut full_right = old_right.clone();
    full_right.extend(delta_right.iter().cloned());
    let before = backend_flow::join_checked(&old_left, &old_right, key, key, combine)?;
    let after = backend_flow::join_checked(&full_left, &full_right, key, key, combine)?;
    let mut expected = after;
    expected.extend(
        before
            .into_iter()
            .map(|mut item| {
                item.diff.checked_neg().map(|diff| {
                    item.diff = diff;
                    item
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    let actual = incremental_join(
        &old_left,
        &delta_left,
        &old_right,
        &delta_right,
        key,
        key,
        combine,
    )?;
    let consolidate = |items: Vec<Delta<i64>>| {
        let mut model = BTreeMap::<(RowKey, i64, Time), i64>::new();
        for item in items {
            let coordinate = (item.key, item.value, item.time);
            let next = model.get(&coordinate).copied().unwrap_or(0) + item.diff.value();
            if next == 0 {
                model.remove(&coordinate);
            } else {
                model.insert(coordinate, next);
            }
        }
        model
    };
    assert_eq!(consolidate(actual), consolidate(expected));
    Ok(())
}

#[test]
fn large_payload_map_budget_fails_without_claiming_a_completed_update() -> Result<(), Box<dyn Error>>
{
    let payload = StoredValue::new(vec![0x7f; 32 * 1024], 1, Vec::new());
    let map = OrderedMap::try_from_iter(vec![(b"payload".to_vec(), payload.clone())])
        .map_err(|error| format!("large map: {error:?}"))?;
    let result = map.apply_with_budget(
        &[backend_store::Change {
            key: b"payload".to_vec(),
            before: Some(payload.clone()),
            after: Some(StoredValue::new(vec![0x80; 32 * 1024], 1, Vec::new())),
        }],
        WorkBudget::new(0),
    );
    assert!(result.is_err());
    assert_eq!(map.get(b"payload"), Some(&payload));
    Ok(())
}
