#![cfg(feature = "local")]
//! Fan-out merge and hot-set planning tests: deterministic cross-shard
//! ordering with payload provenance, pure merge tie-breaks, and the
//! admission-diff install/evict plan.

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::*;
use heart::PackageId;
use registry::vector::{
	AdmissionBudget, JinaCodeV2, NAMESPACE_NUDOX, Payload, PayloadValue, SearchHit, SourceTag,
	VectorPoint, VectorStore,
};
use registry::vector::local::{HotSetManager, LocalShardStore, WorkingSet, merge_hits};

fn pkg(name: &str) -> PackageId {
	PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
}

/// Two real shards (mutable project ∪ read-only baked dep) fan out and
/// merge: exact global score order, deterministic tie-break by PointId,
/// package payload preserved on every hit.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fanout_merge_is_deterministic_and_keeps_package_payload() {
	// Project shard: graded points 0..5 (scores just below 1.0) plus an
	// exact-match tie point.
	let project_dir = tempfile::tempdir().unwrap();
	let project = open_mutable_f32(project_dir.path()).await;
	project.upsert(corpus(5, "proj")).await.unwrap();
	project
		.upsert(vec![VectorPoint {
			id: pid(501),
			vector: basis(0),
			payload: payload("rust", "proj"),
		}])
		.await
		.unwrap();

	// Dep shard, built then reopened read-only (the baked lifecycle):
	// weaker graded points 10..15 plus the *same* exact-match vector under
	// a smaller id — a cross-shard score tie.
	let dep_dir = tempfile::tempdir().unwrap();
	{
		let builder = open_mutable_f32(dep_dir.path()).await;
		let mut points: Vec<VectorPoint<JinaCodeV2>> = (0..5)
			.map(|i| VectorPoint {
				id: pid(100 + i as u128),
				vector: graded(10 + i),
				payload: payload("rust", "dep"),
			})
			.collect();
		points.push(VectorPoint {
			id: pid(500),
			vector: basis(0),
			payload: payload("rust", "dep"),
		});
		builder.upsert(points).await.unwrap();
		builder.close().await.unwrap();
	}
	settle().await;
	let dep = LocalShardStore::open_read_only(dep_dir.path(), f32_schema()).await.unwrap();

	let mut working_set = WorkingSet::new(Arc::new(project));
	assert!(working_set.insert_dep(pkg("dep"), Arc::new(dep)).is_none());
	assert_eq!(working_set.dep_count(), 1);
	assert_eq!(working_set.resident_packages(), BTreeSet::from([pkg("dep")]));

	// Exact expected global order:
	// - the two score-1.0 ties first, id ascending: 500 (dep), 501 (proj);
	// - project graded 0..5 (w = 0.05..0.25);
	// - dep graded 10..15 (w = 0.55..0.75).
	let expected: Vec<_> = [500u128, 501]
		.into_iter()
		.chain(0..5)
		.chain((100..105).map(|i| i as u128))
		.map(pid)
		.collect();

	let first = working_set.search_all(request(basis(0), 20)).await.unwrap();
	let second = working_set.search_all(request(basis(0), 20)).await.unwrap();
	assert_eq!(first, second, "fan-out must be deterministic run to run");

	let got: Vec<_> = first.iter().map(|hit| hit.id).collect();
	assert_eq!(got, expected, "merged order must be exact score order with id tie-break");

	// Cross-shard tie really is a tie, broken by id, not by shard luck.
	assert_eq!(first[0].score, first[1].score, "identical vectors must score identically");

	// Provenance payload survives the merge on every hit.
	for hit in &first {
		assert!(hit.payload.contains_key("package"), "hit {} lost its package facet", hit.id);
	}
	assert_eq!(first[0].payload.get("package"), Some(&PayloadValue::Str("dep".into())));
	assert_eq!(first[1].payload.get("package"), Some(&PayloadValue::Str("proj".into())));

	// Truncation honors the global order, not per-shard order.
	let top3 = working_set.search_all(request(basis(0), 3)).await.unwrap();
	let got: Vec<_> = top3.iter().map(|hit| hit.id).collect();
	assert_eq!(got, vec![pid(500), pid(501), pid(0)]);
}

/// Pure merge semantics: score descending, `PointId` ascending on ties,
/// truncation, and indifference to per-shard list order.
#[test]
fn merge_hits_tie_break_and_truncation_are_deterministic() {
	let hit = |id: u128, score: f32| SearchHit {
		id: pid(id),
		score,
		payload: Payload::default(),
		source: SourceTag::Local,
	};

	let shard_a = vec![hit(7, 0.9), hit(3, 0.5)];
	let shard_b = vec![hit(1, 0.9), hit(2, 0.7)];

	let merged = merge_hits(vec![shard_a.clone(), shard_b.clone()], 10);
	let ids: Vec<_> = merged.iter().map(|h| h.id).collect();
	assert_eq!(ids, vec![pid(1), pid(7), pid(2), pid(3)], "0.9 tie breaks to smaller id");

	// Swapping shard arrival order changes nothing.
	let swapped = merge_hits(vec![shard_b, shard_a], 10);
	assert_eq!(merged, swapped);

	// Truncation keeps the global best, not per-list survivors.
	let top2 = merge_hits(
		vec![vec![hit(7, 0.9), hit(3, 0.5)], vec![hit(1, 0.9), hit(2, 0.7)]],
		2,
	);
	let ids: Vec<_> = top2.iter().map(|h| h.id).collect();
	assert_eq!(ids, vec![pid(1), pid(7)]);

	assert!(merge_hits(vec![], 5).is_empty());
	assert!(merge_hits(vec![vec![]], 0).is_empty());
}

/// The plan is the admission/installed diff: newly admitted packages
/// install (priority order), no-longer-admitted ones evict, residents in
/// good standing are untouched.
#[test]
fn hotset_diff_plan_install_and_evict_are_correct() {
	let state = tempfile::tempdir().unwrap();
	let budget = AdmissionBudget { budget_bytes: 1_000, project_ram: 0 };
	let mut manager = HotSetManager::open(state.path().join("hotset.json"), budget);

	let now = 1_000_000;
	// a, b: direct deps, 400 B each — both fit (800 ≤ 1000).
	manager.upsert_package(pkg("a"), 400, true, 0.5, now);
	manager.upsert_package(pkg("b"), 400, true, 0.5, now);
	// c: transitive, zero signals — score 0, rejected.
	manager.upsert_package(pkg("c"), 400, false, 0.0, now);

	// Installed: c (stale) and a (still good).
	let installed = BTreeSet::from([pkg("a"), pkg("c")]);
	let plan = manager.plan(now, &installed);

	assert_eq!(plan.install, vec![pkg("b")], "only the missing admitted package installs");
	assert_eq!(plan.evict, vec![pkg("c")], "only the no-longer-admitted package evicts");
	assert!(!plan.over_budget);
	assert!(!plan.is_empty());

	// Deterministic: identical inputs, identical plan.
	assert_eq!(plan, manager.plan(now, &installed));

	// Already converged working set → empty plan.
	let converged = BTreeSet::from([pkg("a"), pkg("b")]);
	assert!(manager.plan(now, &converged).is_empty());
}

/// Pins are admitted unconditionally; a pin overshoot is surfaced, never
/// silent.
#[test]
fn hotset_pin_wins_and_overshoot_is_flagged() {
	let state = tempfile::tempdir().unwrap();
	let budget = AdmissionBudget { budget_bytes: 500, project_ram: 0 };
	let mut manager = HotSetManager::open(state.path().join("hotset.json"), budget);

	let now = 1_000_000;
	// A zero-signal package would never be admitted on merit at this size…
	manager.upsert_package(pkg("pinned"), 400, false, 0.0, now);
	manager.upsert_package(pkg("worthy"), 400, true, 1.0, now);

	let none_installed = BTreeSet::new();
	let unpinned_plan = manager.plan(now, &none_installed);
	assert_eq!(unpinned_plan.install, vec![pkg("worthy")], "no pin → merit only");

	// …but the pin admits it first, squeezing the worthy package out.
	manager.set_pinned(pkg("pinned"), true);
	let pinned_plan = manager.plan(now, &none_installed);
	assert_eq!(pinned_plan.install, vec![pkg("pinned")]);
	assert!(!pinned_plan.over_budget, "400 ≤ 500 is not an overshoot");

	// Two pins beyond the budget: both admitted, overshoot flagged.
	manager.upsert_package(pkg("pinned2"), 400, false, 0.0, now);
	manager.set_pinned(pkg("pinned2"), true);
	let overshoot_plan = manager.plan(now, &none_installed);
	let installed_pins: BTreeSet<_> = overshoot_plan.install.iter().copied().collect();
	assert!(installed_pins.contains(&pkg("pinned")) && installed_pins.contains(&pkg("pinned2")));
	assert!(overshoot_plan.over_budget, "pin overshoot must be surfaced");
}

/// Stats survive a restart byte-for-byte via the JSON state file; a
/// corrupted state file degrades to empty stats instead of failing open.
#[test]
fn hotset_state_persists_and_survives_corruption() {
	let dir = tempfile::tempdir().unwrap();
	let state_path = dir.path().join("hotset.json");
	let budget = AdmissionBudget { budget_bytes: 1_000, project_ram: 100 };

	let now = 2_000_000;
	let mut manager = HotSetManager::open(state_path.clone(), budget);
	manager.upsert_package(pkg("a"), 400, true, 0.25, now);
	manager.upsert_package(pkg("b"), 300, false, 0.75, now);
	manager.set_pinned(pkg("b"), true);
	manager.record_query_hit(pkg("a"), now + 60);
	manager.persist().unwrap();

	let reloaded = HotSetManager::open(state_path.clone(), budget);
	assert_eq!(reloaded.state(), manager.state(), "state must roundtrip exactly");
	// And the reloaded stats produce the same plan.
	let installed = BTreeSet::new();
	assert_eq!(reloaded.plan(now + 120, &installed), manager.plan(now + 120, &installed));

	// Unknown packages never invent stats.
	let mut reloaded = reloaded;
	reloaded.record_query_hit(pkg("ghost"), now);
	assert!(!reloaded.state().stats.contains_key(&pkg("ghost")));

	// Corrupt file → warn + empty, not a crash and not garbage stats.
	std::fs::write(&state_path, b"{ not json").unwrap();
	let fresh = HotSetManager::open(state_path, budget);
	assert!(fresh.state().stats.is_empty(), "corrupt state must degrade to empty");
}

/// Packages that leave the DepSet drop out of the stats (and thus of every
/// future plan).
#[test]
fn hotset_retain_drops_departed_packages() {
	let dir = tempfile::tempdir().unwrap();
	let budget = AdmissionBudget { budget_bytes: 10_000, project_ram: 0 };
	let mut manager = HotSetManager::open(dir.path().join("hotset.json"), budget);

	let now = 3_000_000;
	manager.upsert_package(pkg("stays"), 100, true, 0.5, now);
	manager.upsert_package(pkg("leaves"), 100, true, 0.5, now);

	manager.retain_packages(&BTreeSet::from([pkg("stays")]));
	assert!(manager.state().stats.contains_key(&pkg("stays")));
	assert!(!manager.state().stats.contains_key(&pkg("leaves")));

	let plan = manager.plan(now, &BTreeSet::from([pkg("leaves")]));
	assert_eq!(plan.install, vec![pkg("stays")]);
	assert_eq!(plan.evict, vec![pkg("leaves")], "departed resident must evict");
}
