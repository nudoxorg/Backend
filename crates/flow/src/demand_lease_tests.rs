#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use backend_version::ObjectVersion;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

fn work(seed: u8) -> WorkKey {
    WorkKey::new(
        ObjectVersion::from_value(&[seed; 32]),
        ObjectVersion::from_value(&[seed; 32]),
        ObjectVersion::from_value(&[seed; 32]),
        ObjectVersion::from_value(&[seed; 32]),
        ObjectVersion::from_value(&[seed; 32]),
    )
}

#[test]
fn dropped_demand_lease_prunes_dependency_closure() {
    let store = DemandStore::new();
    let input = work(1);
    let output = work(2);
    store.with_mut(|graph| {
        graph
            .try_depends_on(output, input)
            .expect("acyclic dependency");
    });
    let lease = store
        .lease(Demand {
            consumer: 7,
            work: output,
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("lease admission");
    assert_eq!(store.with(DemandGraph::retained_work_count), 2);
    drop(lease);
    assert_eq!(store.with(DemandGraph::retained_work_count), 0);
    assert_eq!(store.with(|graph| graph.dependency_count(output)), 0);
}

#[test]
fn unchecked_dependency_convenience_cannot_retain_a_cycle() {
    let key = work(9);
    let mut graph = DemandGraph::default();
    graph.depends_on(key, key);
    assert_eq!(graph.dependency_count(key), 0);
}

#[test]
fn replacing_a_consumer_releases_the_old_dependency_closure() {
    let mut graph = DemandGraph::default();
    let old_input = work(10);
    let old_output = work(11);
    let new_output = work(12);
    graph
        .try_depends_on(old_output, old_input)
        .expect("old dependency");
    graph
        .add_demand(Demand {
            consumer: 8,
            work: old_output,
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("old demand admission");
    assert_eq!(graph.retained_work_count(), 2);

    graph
        .add_demand(Demand {
            consumer: 8,
            work: new_output,
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("replacement demand admission");
    assert_eq!(graph.retained_work_count(), 1);
    assert_eq!(graph.dependency_count(old_output), 0);
}

#[test]
fn an_old_lease_cannot_cancel_a_replacement_for_the_same_consumer() {
    let store = DemandStore::new();
    let old = store
        .lease(Demand {
            consumer: 9,
            work: work(13),
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("old lease admission");
    let replacement = store
        .lease(Demand {
            consumer: 9,
            work: work(14),
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("replacement lease admission");

    assert!(!old.release());
    assert!(store.with(|graph| graph.is_demanded(work(14))));
    assert!(replacement.release());
    assert_eq!(store.with(DemandGraph::retained_work_count), 0);
}

#[test]
fn lease_fence_rejects_stale_owner_and_expiry_reaps_roots() {
    let store = DemandStore::new();
    let first = store
        .lease_for(
            Demand {
                consumer: 15,
                work: work(15),
                range: None,
                freshness: Frontier::new(Time::default()),
                priority: 1,
            },
            Duration::ZERO,
        )
        .expect("bounded lease admission");
    let stale = first.fence();
    let replacement = store
        .lease(Demand {
            consumer: 15,
            work: work(16),
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("replacement lease admission");
    assert!(!store.release_fence(stale));
    assert!(store.with(|graph| graph.is_demanded(work(16))));
    assert_eq!(store.reap_expired(Instant::now()), 0);
    drop(first);
    assert!(replacement.is_active());
    assert!(replacement.release());
}

#[test]
fn expired_lease_is_reaped_when_its_generation_is_current() {
    let store = DemandStore::new();
    let mut lease = store
        .lease_for(
            Demand {
                consumer: 17,
                work: work(17),
                range: None,
                freshness: Frontier::new(Time::default()),
                priority: 1,
            },
            Duration::ZERO,
        )
        .expect("renewable lease admission");
    assert!(lease.is_expired(Instant::now()));
    assert_eq!(store.reap_expired(Instant::now()), 1);
    assert!(!lease.renew(Duration::from_secs(1)));
    assert!(!store.with(|graph| graph.is_demanded(work(17))));
}

#[test]
fn demand_gc_uses_live_roots_after_manual_owner_release() {
    let mut graph = DemandGraph::default();
    let input = work(18);
    let output = work(19);
    graph.try_depends_on(output, input).expect("dependency");
    graph
        .add_demand(Demand {
            consumer: 19,
            work: output,
            range: None,
            freshness: Frontier::new(Time::default()),
            priority: 1,
        })
        .expect("demand admission");
    assert_eq!(graph.gc_roots(), BTreeSet::from([output]));
    assert_eq!(graph.collect_unreachable(), 0);
    let _ = graph.remove_demand(19);
    assert_eq!(graph.collect_unreachable(), 2);
    assert!(graph.gc_roots().is_empty());
}
