#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers turn invariant failures into focused diagnostics"
)]

use super::*;
use crate::ids::Identity;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn must<T>(result: Result<T, ControlError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("control test failed: {error}"),
    }
}

fn plane() -> VersionedControlPlane {
    must(VersionedControlPlane::new(SchedulerLimits {
        max_parallel: 2,
        max_attempts: 2,
        max_waiters: 2,
        lease_ttl_ns: 100,
    }))
}

#[test]
fn control_coverage_fixture_is_bound_to_the_admitted_evaluator() {
    let coverage = must(ledger::control_coverage());
    let backend_version::CoverageWitness::Complete(complete) = coverage else {
        panic!("control coverage fixture must be complete");
    };
    assert_eq!(
        complete.producer_identity(),
        *blake3::hash(b"backend-control/evaluator/v1").as_bytes()
    );
}

#[test]
fn invalid_scheduler_policy_is_rejected_without_defaulting() {
    let result = VersionedControlPlane::new(SchedulerLimits {
        max_parallel: 0,
        max_attempts: 1,
        max_waiters: 1,
        lease_ttl_ns: 1,
    });
    assert!(matches!(result, Err(ControlError::Bounds)));
}

fn spec(cell: &str, input: &[u8]) -> WorkSpec {
    must(WorkSpec::from_labels(
        cell,
        "implement",
        "local",
        Effort::High,
        b"toolchain",
        input,
        [],
    ))
}

fn owner(name: &str) -> Identity<ids::OwnerSchema> {
    must(Identity::from_label(name))
}

fn admit_planned(plane: &mut VersionedControlPlane, work: &WorkSpec) -> Lease<Held> {
    let (admission, commit) = must(plane.admit(work.key(), owner("worker"), 1));
    if let Some(commit) = commit {
        must(plane.apply(commit));
    }
    match admission {
        Admission::Owned(lease) => lease,
        other => panic!("expected owned admission, got {other:?}"),
    }
}

fn plan(plane: &mut VersionedControlPlane, work: WorkSpec) {
    let result = must(plane.plan(work));
    if let PlanResult::Admitted(commit) = result {
        must(plane.apply(commit));
    }
}

fn complete_candidate(
    plane: &mut VersionedControlPlane,
    frozen: &Lease<Frozen>,
    label: &str,
    now: u64,
) {
    let evaluation = must(plane.evaluate(
        frozen,
        owner(&format!("{label}-evaluator")),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(
            format!("{label}-evaluation").into_bytes(),
        )),
        now,
    ));
    let evaluated = match evaluation {
        StageResult::Advanced { candidate, commit } => {
            must(plane.apply(commit));
            candidate
        }
        StageResult::Terminal { status, .. } => {
            panic!("evaluation unexpectedly ended in {status:?}")
        }
    };
    let review = must(plane.review(
        &evaluated,
        owner(&format!("{label}-reviewer")),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(format!("{label}-review").into_bytes())),
    ));
    let reviewed = match review {
        StageResult::Advanced { candidate, commit } => {
            must(plane.apply(commit));
            candidate
        }
        StageResult::Terminal { status, .. } => {
            panic!("review unexpectedly ended in {status:?}")
        }
    };
    let decision = must(plane.decide(
        &reviewed,
        owner(&format!("{label}-sol")),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(
            format!("{label}-decision").into_bytes(),
        )),
    ));
    assert_eq!(decision.status, WorkStatus::Completed);
    must(plane.apply(decision.commit));
}

#[test]
fn work_key_is_canonical_and_order_independent() {
    let first = spec("cell", b"input");
    let second = spec("cell", b"input");
    assert_eq!(first.key(), second.key());
    assert_eq!(first.canonical_bytes(), second.canonical_bytes());
    assert_ne!(first.key(), spec("cell", b"changed").key());
}

#[test]
fn duplicate_plan_reuses_only_completed_receipt() {
    let mut plane = plane();
    let work = spec("cell", b"input");
    plan(&mut plane, work.clone());
    assert!(matches!(
        must(plane.plan(work.clone())),
        PlanResult::Existing { .. }
    ));
    let lease = admit_planned(&mut plane, &work);
    let output = must(Identity::<ids::OutputSchema>::from_bytes(
        b"candidate".to_vec(),
    ));
    let evidence = must(Identity::<ids::EvidenceSchema>::from_bytes(
        b"evidence".to_vec(),
    ));
    let (frozen, commit) = must(plane.freeze(&lease, output, evidence, 2));
    must(plane.apply(commit));
    complete_candidate(&mut plane, &frozen, "reuse", 3);
    assert!(matches!(
        must(plane.plan(work.clone())),
        PlanResult::Reused(_)
    ));

    let completed = plane.get(work.key()).expect("completed row");
    let canonical = must(completed.canonical_bytes());
    assert_eq!(must(WorkRecord::decode(&canonical)), completed.clone());
    let mut hostile = canonical.clone();
    let middle = hostile.len() / 2;
    hostile[middle] ^= 0x80;
    assert!(matches!(
        WorkRecord::decode(&hostile),
        Err(ControlError::Corrupt | ControlError::Bounds)
    ));
}

#[test]
fn coalescing_stops_at_the_admitted_waiter_bound() {
    let mut plane = plane();
    let work = spec("coalesced", b"input");
    plan(&mut plane, work.clone());
    let owner_lease = admit_planned(&mut plane, &work);

    for owner_name in ["waiter-1", "waiter-2"] {
        let (admission, commit) = must(plane.admit(work.key(), owner(owner_name), 2));
        assert!(matches!(admission, Admission::Coalesced { .. }));
        must(plane.apply(commit.expect("coalescing transition")));
    }
    assert_eq!(plane.get(work.key()).map(WorkRecord::waiter_count), Some(2));
    assert_eq!(
        plane.admit(work.key(), owner("waiter-3"), 2),
        Err(ControlError::WaiterLimit)
    );
    assert_eq!(owner_lease.fence().epoch(), 1);
}

#[test]
fn stale_fence_and_stale_root_are_rejected() {
    let mut plane = plane();
    let work = spec("cell", b"input");
    plan(&mut plane, work.clone());
    let lease = admit_planned(&mut plane, &work);
    let stale = plane.snapshot();
    let failed = must(plane.fail(&lease));
    let root_before_failure = stale.root();
    must(plane.apply(failed));
    assert_eq!(stale.root(), root_before_failure);
    assert!(matches!(
        plane.fail(&lease),
        Err(ControlError::InvalidTransition)
    ));
    let (admission, commit) = must(plane.admit(work.key(), owner("replacement"), 2));
    let _replacement = match admission {
        Admission::Owned(lease) => lease,
        other => panic!("expected replacement ownership, got {other:?}"),
    };
    must(plane.apply(commit.expect("replacement transition")));
    assert!(matches!(plane.fail(&lease), Err(ControlError::StaleFence)));
}

#[test]
fn dependency_waits_then_invalidates_through_reverse_index() {
    let mut plane = plane();
    let dependency = spec("dependency", b"input");
    let child = must(WorkSpec::from_labels(
        "child",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&dependency))],
    ));
    plan(&mut plane, dependency.clone());
    plan(&mut plane, child.clone());
    let (admission, commit) = must(plane.admit(child.key(), owner("child"), 1));
    assert!(matches!(admission, Admission::Waiting));
    if let Some(commit) = commit {
        must(plane.apply(commit));
    }
    let dependency_lease = admit_planned(&mut plane, &dependency);
    let (frozen, commit) = must(plane.freeze(
        &dependency_lease,
        must(Identity::from_bytes(b"dep-out".to_vec())),
        must(Identity::from_bytes(b"dep-evidence".to_vec())),
        2,
    ));
    must(plane.apply(commit));
    complete_candidate(&mut plane, &frozen, "dependency-ready", 3);
    let (admission, commit) = must(plane.admit(child.key(), owner("child"), 3));
    assert!(matches!(admission, Admission::Owned(_)));
    if let Some(commit) = commit {
        must(plane.apply(commit));
    }
    assert!(plane.invalidate_dependency(dependency.key()).is_ok());
}

#[test]
fn dependency_invalidation_revokes_completed_reuse() {
    let mut plane = plane();
    let dependency = spec("dependency-completed", b"input");
    let child = must(WorkSpec::from_labels(
        "child-completed",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&dependency))],
    ));
    plan(&mut plane, dependency.clone());
    plan(&mut plane, child.clone());

    let dependency_lease = admit_planned(&mut plane, &dependency);
    let (frozen, commit) = must(plane.freeze(
        &dependency_lease,
        must(Identity::from_bytes(b"dependency-output".to_vec())),
        must(Identity::from_bytes(b"dependency-evidence".to_vec())),
        2,
    ));
    must(plane.apply(commit));
    complete_candidate(&mut plane, &frozen, "dependency", 3);

    let child_lease = admit_planned(&mut plane, &child);
    let (frozen, commit) = must(plane.freeze(
        &child_lease,
        must(Identity::from_bytes(b"child-output".to_vec())),
        must(Identity::from_bytes(b"child-evidence".to_vec())),
        3,
    ));
    must(plane.apply(commit));
    complete_candidate(&mut plane, &frozen, "child", 3);
    assert!(matches!(
        must(plane.plan(child.clone())),
        PlanResult::Reused(_)
    ));

    let invalidation = must(plane.invalidate_dependency(dependency.key()));
    must(plane.apply(invalidation.expect("completed dependent transition")));
    assert_eq!(
        plane.get(child.key()).map(WorkRecord::status),
        Some(WorkStatus::Invalidated)
    );
    assert!(
        plane
            .reusable(&child)
            .is_ok_and(|receipt| receipt.is_none())
    );
}

#[test]
fn every_custody_authority_is_independent() {
    let mut plane = plane();
    let work = spec("cell", b"input");
    plan(&mut plane, work.clone());
    let lease = admit_planned(&mut plane, &work);
    let (frozen, commit) = must(plane.freeze(
        &lease,
        must(Identity::from_bytes(b"out".to_vec())),
        must(Identity::from_bytes(b"ev".to_vec())),
        2,
    ));
    must(plane.apply(commit));
    let same_owner = plane.evaluate(
        &frozen,
        owner("worker"),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(b"review".to_vec())),
        3,
    );
    assert_eq!(same_owner, Err(ControlError::AuthorityNotIndependent));

    let evaluated = match must(plane.evaluate(
        &frozen,
        owner("evaluator"),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(b"evaluation".to_vec())),
        3,
    )) {
        StageResult::Advanced { candidate, commit } => {
            must(plane.apply(commit));
            candidate
        }
        StageResult::Terminal { status, .. } => panic!("unexpected {status:?}"),
    };
    assert_eq!(
        plane.review(
            &evaluated,
            owner("evaluator"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"review".to_vec())),
        ),
        Err(ControlError::AuthorityNotIndependent)
    );
    let reviewed = match must(plane.review(
        &evaluated,
        owner("reviewer"),
        CustodyVerdict::Accepted,
        must(Identity::from_bytes(b"independent-review".to_vec())),
    )) {
        StageResult::Advanced { candidate, commit } => {
            must(plane.apply(commit));
            candidate
        }
        StageResult::Terminal { status, .. } => panic!("unexpected {status:?}"),
    };
    assert_eq!(
        plane.decide(
            &reviewed,
            owner("reviewer"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"decision".to_vec())),
        ),
        Err(ControlError::AuthorityNotIndependent)
    );
}

#[test]
fn context_delta_is_exact_base_and_redacts_holdout_evidence() {
    let base = must(Identity::<ids::ContextSchema>::from_bytes(
        b"context-0".to_vec(),
    ));
    let public = must(Identity::<ids::EvidenceSchema>::from_bytes(
        b"public".to_vec(),
    ));
    let benchmark = must(Identity::<ids::EvidenceSchema>::from_bytes(
        b"benchmark".to_vec(),
    ));
    let holdout = must(Identity::<ids::EvaluationSchema>::from_bytes(
        b"holdout".to_vec(),
    ));
    let delta = must(ContextDelta::new(
        base.id(),
        [
            ContextItem::new(ContextAudience::Implementer, public.id()),
            ContextItem::new(ContextAudience::Evaluator, benchmark.id())
                .with_evaluation(holdout.id()),
        ],
    ));
    assert_eq!(must(delta.apply_to(base.id())), delta.target());
    assert_eq!(
        delta.apply_to(
            must(Identity::<ids::ContextSchema>::from_bytes(
                b"other".to_vec()
            ))
            .id()
        ),
        Err(ControlError::StaleRoot)
    );
    let visible = delta
        .visible_to(ContextAudience::Implementer)
        .collect::<Vec<_>>();
    assert_eq!(
        visible,
        vec![ContextItem::new(ContextAudience::Implementer, public.id())]
    );
    assert!(
        delta
            .visible_to(ContextAudience::Implementer)
            .all(|item| item.evaluation().is_none())
    );

    // Evaluation provenance must stay private even when a coordinator entry
    // carries it as an optional binding.  Filtering only evaluator-audience
    // entries would leak this root through a controller entry.
    let controller_holdout =
        ContextItem::new(ContextAudience::Controller, public.id()).with_evaluation(holdout.id());
    let guarded = must(ContextDelta::new(base.id(), [controller_holdout]));
    assert_eq!(
        guarded
            .visible_to(ContextAudience::Implementer)
            .next()
            .and_then(ContextItem::evaluation),
        None
    );
}

#[test]
fn dependency_invalidation_reaches_transitive_dependents() {
    let mut plane = plane();
    let root = spec("invalidation-root", b"input");
    let middle = must(WorkSpec::from_labels(
        "invalidation-middle",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&root))],
    ));
    let leaf = must(WorkSpec::from_labels(
        "invalidation-leaf",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&middle))],
    ));
    plan(&mut plane, root.clone());
    plan(&mut plane, middle.clone());
    plan(&mut plane, leaf.clone());
    let invalidation = must(plane.invalidate_dependency(root.key()));
    must(plane.apply(invalidation.expect("transitive invalidation")));
    assert_eq!(
        plane.get(middle.key()).map(WorkRecord::status),
        Some(WorkStatus::Invalidated)
    );
    assert_eq!(
        plane.get(leaf.key()).map(WorkRecord::status),
        Some(WorkStatus::Invalidated)
    );
}

#[test]
fn invalidation_deduplicates_rows_reached_through_a_diamond() {
    let mut plane = plane();
    let root = spec("diamond-root", b"input");
    let left = must(WorkSpec::from_labels(
        "diamond-left",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&root))],
    ));
    let right = must(WorkSpec::from_labels(
        "diamond-right",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [must(WorkDependency::from_spec(&root))],
    ));
    let merge = must(WorkSpec::from_labels(
        "diamond-merge",
        "implement",
        "local",
        Effort::Medium,
        b"toolchain",
        b"input",
        [
            must(WorkDependency::from_spec(&left)),
            must(WorkDependency::from_spec(&right)),
        ],
    ));
    for item in [root.clone(), left, right, merge.clone()] {
        plan(&mut plane, item);
    }
    let invalidation = must(plane.invalidate_dependency(root.key()));
    must(plane.apply(invalidation.expect("diamond invalidation")));
    assert_eq!(
        plane.get(merge.key()).map(WorkRecord::status),
        Some(WorkStatus::Invalidated)
    );
}

#[test]
fn failed_and_expired_attempts_retain_monotonic_fences_for_retry() {
    let mut plane = plane();
    let work = spec("retry", b"input");
    plan(&mut plane, work.clone());
    let first = admit_planned(&mut plane, &work);
    let first_fence = first.fence();
    let failed = must(plane.fail(&first));
    must(plane.apply(failed));
    let (admission, commit) = must(plane.admit(work.key(), owner("replacement"), 2));
    let second = match admission {
        Admission::Owned(lease) => lease,
        other => panic!("expected retry ownership, got {other:?}"),
    };
    assert_ne!(first_fence.epoch(), second.fence().epoch());
    must(plane.apply(commit.expect("retry transition")));
    assert!(matches!(plane.fail(&first), Err(ControlError::StaleFence)));

    let expiring = spec("expiry", b"input");
    plan(&mut plane, expiring.clone());
    let lease = admit_planned(&mut plane, &expiring);
    let old_epoch = lease.fence().epoch();
    let (expired, commit) = must(plane.recover(101));
    assert_eq!(expired, vec![expiring.key()]);
    must(plane.apply(commit.expect("expiry transition")));
    let (admission, commit) = must(plane.admit(expiring.key(), owner("replacement"), 103));
    let retry = match admission {
        Admission::Owned(lease) => lease,
        other => panic!("expected expired retry ownership, got {other:?}"),
    };
    assert!(retry.fence().epoch() > old_epoch);
    if let Some(commit) = commit {
        must(plane.apply(commit));
    }
}

#[test]
fn frozen_candidate_releases_execution_capacity_and_expires_to_quarantine() {
    let mut plane = plane();
    let work = spec("frozen-recovery", b"input");
    plan(&mut plane, work.clone());
    let lease = admit_planned(&mut plane, &work);
    let (frozen, commit) = must(plane.freeze(
        &lease,
        must(Identity::from_bytes(b"frozen-output".to_vec())),
        must(Identity::from_bytes(b"frozen-evidence".to_vec())),
        2,
    ));
    must(plane.apply(commit));
    let (expired, commit) = must(plane.recover(102));
    assert_eq!(expired, vec![work.key()]);
    must(plane.apply(commit.expect("review expiry transition")));
    assert_eq!(
        plane.get(work.key()).map(WorkRecord::status),
        Some(WorkStatus::Quarantined)
    );
    assert_eq!(must(plane.summary()).active(), 0);
    assert_eq!(
        plane.evaluate(
            &frozen,
            owner("independent-reviewer"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"review-evidence".to_vec())),
            102,
        ),
        Err(ControlError::InvalidTransition)
    );
}

#[test]
fn durable_restart_rehydrates_the_same_selected_relation() {
    let path = unique_ledger_path("restart");
    let work = spec("durable", b"input");
    let root = {
        let mut durable = must(DurableControlPlane::open(
            &path,
            DEFAULT_MAX_PACK_BYTES,
            SchedulerLimits::default(),
        ));
        let result = must(durable.plan(work.clone()));
        assert!(matches!(result, PlanResult::Admitted(_)));
        let (admission, commit) = must(durable.admit(work.key(), owner("worker"), 1));
        assert!(commit.is_some());
        let lease = match admission {
            Admission::Owned(lease) => lease,
            other => panic!("expected owned admission, got {other:?}"),
        };
        let (frozen, freeze_commit) = must(durable.freeze(
            &lease,
            must(Identity::from_bytes(b"durable-output".to_vec())),
            must(Identity::from_bytes(b"durable-evidence".to_vec())),
            2,
        ));
        assert!(!freeze_commit.delta.canonical_bytes().is_empty());
        let evaluation = must(durable.evaluate(
            &frozen,
            owner("evaluator"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"durable-evaluation".to_vec())),
            3,
        ));
        let evaluated = match evaluation {
            StageResult::Advanced { candidate, .. } => candidate,
            StageResult::Terminal { status, .. } => panic!("unexpected {status:?}"),
        };
        let review = must(durable.review(
            &evaluated,
            owner("reviewer"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"durable-review".to_vec())),
        ));
        let reviewed = match review {
            StageResult::Advanced { candidate, .. } => candidate,
            StageResult::Terminal { status, .. } => panic!("unexpected {status:?}"),
        };
        let decision = must(durable.decide(
            &reviewed,
            owner("sol"),
            CustodyVerdict::Accepted,
            must(Identity::from_bytes(b"durable-decision".to_vec())),
        ));
        assert_eq!(decision.status, WorkStatus::Completed);
        durable.root()
    };
    let mut reopened = must(DurableControlPlane::open(
        &path,
        DEFAULT_MAX_PACK_BYTES,
        SchedulerLimits::default(),
    ));
    assert_eq!(reopened.root(), root);
    assert_eq!(
        must(reopened.get(work.key())).map(|record| record.status()),
        Some(WorkStatus::Completed)
    );
    assert!(matches!(must(reopened.plan(work)), PlanResult::Reused(_)));
    assert!(std::fs::remove_dir_all(path).is_ok());
}

#[test]
fn cloned_durable_handles_reject_an_obsolete_relation_base() {
    let path = unique_ledger_path("stale-clone");
    let mut primary = must(DurableControlPlane::open(
        &path,
        DEFAULT_MAX_PACK_BYTES,
        SchedulerLimits::default(),
    ));
    let mut stale = primary.clone();
    let first = spec("primary", b"input");
    must(primary.plan(first));

    let second = spec("stale", b"input");
    assert_eq!(stale.plan(second.clone()), Err(ControlError::StaleRoot));
    assert!(must(primary.get(second.key())).is_none());
    assert!(std::fs::remove_dir_all(path).is_ok());
}

#[test]
fn relation_encoding_rejects_hostile_mutations_and_never_collapses_to_empty() {
    let mut plane = plane();
    let work = spec("encoding", b"input");
    plan(&mut plane, work.clone());
    let record = plane.get(work.key()).expect("planned row");
    let canonical = must(record.canonical_bytes());
    assert!(!canonical.is_empty());
    assert_eq!(must(WorkRecord::decode(&canonical)), record.clone());
    let mut hostile = canonical.clone();
    let last = hostile.len() - 1;
    hostile[last] ^= 0xff;
    assert!(matches!(
        WorkRecord::decode(&hostile),
        Err(ControlError::Corrupt)
    ));

    let root_before = plane.root();
    let lease = admit_planned(&mut plane, &work);
    assert_ne!(plane.root(), root_before);
    assert!(
        lease
            .fence()
            .fence()
            .as_bytes()
            .iter()
            .any(|byte| *byte != 0)
    );
}

fn unique_ledger_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "backend-control-{label}-{}-{nanos}",
        std::process::id()
    ))
}
