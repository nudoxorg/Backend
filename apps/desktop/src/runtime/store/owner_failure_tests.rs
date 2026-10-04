//! Real worker and owner-watcher schedules; no native presentation claim.
#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{Activity, ResourceTerminal, VersionedRoot};
use crate::model::ServiceMode;
use crate::model::pages::{PageValue, SeedEntry};
use crate::runtime::owner::OwnerState;
use crate::runtime::reads::{PageReader, ReadContext};
use crate::runtime::{
    DesktopRuntime, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest, UiEntityGraph,
};
use crate::shell::tests::{Fixture, page, symbol};
use gpui::{Subscription, TestAppContext};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Condvar, Mutex, PoisonError, mpsc};

fn root(tag: &str) -> VersionedRoot {
    VersionedRoot::synthetic(
        backend_library::view_state_root(&[("failure-schedule".into(), tag.into())]),
        13,
    )
}

type CompletedSuccess = (bool, Result<PageValue, ReadFailure>);

struct BlockingSuccess {
    block: bool,
    began: mpsc::Sender<CancellationToken>,
    completed: mpsc::Sender<CompletedSuccess>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl PageReader for BlockingSuccess {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        // Construct the complete, useful model before any completion witness.
        // Mapping failure cannot be caught by the pool and satisfy the oracle.
        let success = Ok(Fixture.read(request, context)?);
        if self.block && matches!(request, ReadRequest::Symbol(_)) {
            self.block = false;
            let gate = Arc::clone(&self.gate);
            let wake = context.cancel.on_cancel(move || {
                let (lock, ready) = &*gate;
                *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
                ready.notify_all();
            });
            self.began
                .send(context.cancel.clone())
                .expect("actual worker token");
            let (lock, ready) = &*self.gate;
            let (released, timeout) = ready
                .wait_timeout_while(
                    lock.lock().expect("gate"),
                    crate::runtime::wait::HUNG,
                    |released| !*released,
                )
                .expect("bounded cancellation wait");
            let cancelled = *released && !timeout.timed_out() && context.cancel.is_cancelled();
            drop((released, wake));
            self.completed
                .send((cancelled, success.clone()))
                .expect("completed success witness");
        }
        // A deliberately non-cooperating successful return after cancellation.
        success
    }
}

fn blocking_pool() -> (
    ReadPool,
    mpsc::Receiver<CancellationToken>,
    mpsc::Receiver<CompletedSuccess>,
) {
    let (began, entered) = mpsc::channel();
    let (completed, witness) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let pool = ReadPool::start(1, move |_| BlockingSuccess {
        block: true,
        began: began.clone(),
        completed: completed.clone(),
        gate: Arc::clone(&gate),
    })
    .expect("real worker");
    (pool, entered, witness)
}

fn until(
    store: &Entity<DataStore>,
    cx: &mut TestAppContext,
    why: &str,
    done: impl Fn(&DataStore) -> bool,
) {
    crate::runtime::wait::until(why, || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| done(store))
    });
}

#[gpui::test]
fn exhaustion_cancels_captured_read_ids_but_a_reentrant_same_key_admission_survives(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (pool, entered, completed) = blocking_pool();
    let submit = pool.test_submitter();
    let store = cx.update(|cx| {
        DataStore::install(
            cx,
            Arc::new(AppSnapshot::empty(root("same-key"))),
            Some(pool),
        )
    });
    until(&store, cx, "initial reads settle", |store| {
        store.health().is_loaded() && store.pool_activity().is_idle()
    });
    let reference = symbol("ReentrantIdentity");
    let key = PageKey::Symbol(reference.clone());
    store.update(cx, |store, cx| {
        store
            .pages
            .test_advance_generation_to(Generation::new(u64::MAX).expect("last"));
        store.ensure(key.clone(), cx);
    });
    let old = entered
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("running last worker");
    let old_ids = store.read_with(cx, |store, _| {
        store.pool.as_ref().expect("pool").test_ids(&key)
    });
    assert_eq!(old_ids.len(), 1);
    let later = CancellationToken::new();
    let later_generation = Generation::new(7).expect("distinct pool-only probe generation");
    let job = ReadJob {
        key: key.clone(),
        request: ReadRequest::Symbol(reference.clone()),
        generation: later_generation,
        priority: Priority::Normal,
        cancel: later.clone(),
        affinity: None,
    };
    let (sent, received) = mpsc::channel();
    let registration = old.on_cancel(move || {
        // No assertion can be swallowed by callback isolation.
        let answer = submit(job.clone());
        let _ = sent.send(answer);
    });
    store.update(cx, |store, cx| {
        store.retry(key.clone(), cx);
        assert!(old.is_cancelled());
        assert_eq!(store.pages.inflight(&key), None);
        assert_eq!(
            store.symbol(&reference).terminal(),
            &ResourceTerminal::Fault(crate::model::pages::store::GenerationExhausted.fault())
        );
    });
    assert!(
        received
            .recv_timeout(crate::runtime::wait::HUNG)
            .expect("callback ran")
            .is_ok(),
        "reentrant admission used the real scheduler"
    );
    let (cancelled, useful) = completed
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("completed useful return");
    assert!(cancelled);
    assert_eq!(useful, Ok(PageValue::Symbol(page("ReentrantIdentity"))));
    // Withhold the foreground executor, so the pool-only probe cannot be
    // discarded by the store's deliberately exhausted generation fence.
    crate::runtime::wait::until("both real workers finished", || {
        store.read_with(cx, |store, _| {
            let load = store.pool.as_ref().expect("pool").load();
            load.running == 0 && load.queued == 0 && load.undelivered > 0
        })
    });
    let outcomes = store.read_with(cx, |store, _| store.pool.as_ref().expect("pool").drain());
    let mut later_ids = BTreeSet::new();
    for outcome in &outcomes {
        let id = ReadPool::test_outcome_id(outcome);
        if old_ids.contains(&id) {
            assert_eq!(
                outcome.delivery,
                Delivery::Terminal(Err(ReadFailure::Cancelled))
            );
        } else {
            assert_eq!(outcome.key, key);
            assert_eq!(outcome.generation, later_generation);
            assert_eq!(
                outcome.delivery,
                Delivery::Terminal(Ok(PageValue::Symbol(page("ReentrantIdentity"))))
            );
            later_ids.insert(id);
        }
    }
    assert_eq!(
        later_ids.len(),
        1,
        "the later exact ReadId must survive withdrawal"
    );
    assert!(old_ids.is_disjoint(&later_ids));
    assert!(
        later_ids.first().expect("later identity") > old_ids.last().expect("old identity"),
        "the callback mints an actual later ReadId"
    );
    assert!(
        !later.is_cancelled(),
        "a captured old-key cancellation cannot target the newer token"
    );
    drop((outcomes, registration));
    store.read_with(cx, |store, _| {
        assert!(store.pool_activity().is_idle());
        assert_eq!(
            store.pool.as_ref().expect("pool").load().held,
            crate::runtime::reads::Held::default()
        );
        assert_eq!(
            store.symbol(&reference).terminal(),
            &ResourceTerminal::Fault(crate::model::pages::store::GenerationExhausted.fault())
        );
    });
}

#[gpui::test]
fn owner_failure_revokes_the_final_running_generation_and_retains_exact_old_root_bytes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (pool, entered, completed) = blocking_pool();
    let store = cx.update(|cx| {
        DataStore::install(
            cx,
            Arc::new(AppSnapshot::empty(root("current"))),
            Some(pool),
        )
    });
    until(&store, cx, "initial reads settle", |store| {
        store.health().is_loaded() && store.pool_activity().is_idle()
    });
    let reference = symbol("FinalOwnerFailure");
    let key = PageKey::Symbol(reference.clone());
    let old_root = root("retained");
    let bytes = store.update(cx, |store, cx| {
        assert!(store.pages.seed(
            SeedEntry::Symbol(reference.clone(), Arc::new(page("FinalOwnerFailure"))),
            old_root
        ));
        let bytes = store
            .symbol(&reference)
            .loaded_arc()
            .expect("actual installed predecessor")
            .clone();
        store
            .pages
            .test_advance_generation_to(Generation::new(u64::MAX).expect("last"));
        store.focus(vec![key.clone()], cx);
        bytes
    });
    let actual = entered
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("final read runs");
    let fault = OwnerFault::Lost("actual fixture owner failed".into());
    store.update(cx, |store, cx| {
        assert_eq!(store.pages.inflight(&key), Generation::new(u64::MAX));
        store.owner_failed(&fault, cx);
        assert!(
            actual.is_cancelled(),
            "revocation precedes fault visibility"
        );
        assert_eq!(store.pages.inflight(&key), None);
        assert_eq!(
            store.symbol(&reference).terminal(),
            &ResourceTerminal::Fault(crate::model::pages::store::GenerationExhausted.fault())
        );
        assert!(Arc::ptr_eq(
            store.symbol(&reference).loaded_arc().expect("old bytes"),
            &bytes
        ));
        assert_eq!(store.symbol(&reference).value_root(), Some(old_root));
    });
    let (cancelled, useful) = completed
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("actual successful return after interruption");
    assert!(cancelled);
    assert_eq!(useful, Ok(PageValue::Symbol(page("FinalOwnerFailure"))));
    let stamp = store.read_with(cx, |store, _| store.stamp(&key));
    until(&store, cx, "cancelled terminal lands inertly", |store| {
        store.pool_activity().is_idle()
    });
    store.update(cx, |store, cx| {
        store.owner_failed(&fault, cx);
        assert_eq!(
            store.stamp(&key),
            stamp,
            "identical exhausted failure cannot jump through Waiting"
        );
        assert!(Arc::ptr_eq(
            store.symbol(&reference).loaded_arc().expect("old bytes"),
            &bytes
        ));
        assert_eq!(store.symbol(&reference).value_root(), Some(old_root));
        assert_eq!(
            store.pool.as_ref().expect("pool").load().held,
            crate::runtime::reads::Held::default()
        );
    });
}

struct RootOnly;
impl EngineClient for RootOnly {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { request, basis, .. } => Ok(EngineDto::Root {
                request: *request,
                basis: *basis,
                key: *basis,
                revision: basis.revision(),
                delta: None,
                project: None,
                catalog: None,
            }),
            _ => Err(EngineFault::Cancelled),
        }
    }
}

#[gpui::test]
fn actual_owner_watcher_repeats_refresh_retry_without_republishing_the_fault_resource(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let at = root("watcher");
    let gate = OwnerGate::ready(at, ServiceMode::Attached);
    let actor = EngineActor::start(RootOnly, 8).expect("actor");
    let pool = ReadPool::start(1, |_| Fixture).expect("pool");
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(AppSnapshot::empty(at), actor),
            None,
            Some(pool),
            Some(gate.clone()),
            None,
        )
    });
    let store = &graph.store;
    until(store, cx, "initial actual worker reads", |store| {
        store.health().is_loaded() && store.pool_activity().is_idle()
    });
    let retained = store.read_with(cx, |store, _| {
        store.health().loaded_arc().expect("health").clone()
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    let capabilities = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&events);
    let captured = Rc::clone(&capabilities);
    let subscription: Subscription = cx.update(|cx| {
        let mut watch = Watch::new(store.read(cx), [PageKey::Health], &[Branch::GraphFocus]);
        cx.subscribe(store, move |store, event: &StoreEvent, cx| {
            if watch.changed(store.read(cx), event) {
                sink.borrow_mut().push(event.clone());
                if event.is_branch(Branch::GraphFocus) {
                    captured
                        .borrow_mut()
                        .push(store.read(cx).current_owner_retry());
                }
            }
        })
    });
    let first = OwnerFault::Lost("first failure".into());
    gate.publish(OwnerState::Failed(first.clone()));
    until(store, cx, "watcher faults real resources", |store| {
        store.health().terminal() == &ResourceTerminal::Fault(owner_failure_value(&first))
    });
    cx.run_until_parked();
    let previous_retry = store.read_with(cx, |store, _| {
        store.current_owner_retry().expect("retry capability")
    });
    let stamp = store.read_with(cx, |store, _| store.stamp(&PageKey::Health));
    events.borrow_mut().clear();
    let before = capabilities.borrow().len();
    gate.publish(OwnerState::Failed(first.clone()));
    crate::runtime::wait::until("repeat reaches capability subscriber", || {
        cx.run_until_parked();
        capabilities.borrow().len() > before
    });
    assert_eq!(
        events.borrow().as_slice(),
        &[StoreEvent::Snapshot(Branch::GraphFocus)],
        "capability repaint is separate from resource churn"
    );
    store.update(cx, |store, cx| {
        assert_eq!(store.stamp(&PageKey::Health), stamp);
        assert_eq!(
            store.health().terminal(),
            &ResourceTerminal::Fault(owner_failure_value(&first))
        );
        assert!(Arc::ptr_eq(
            store.health().loaded_arc().expect("retained"),
            &retained
        ));
        assert_eq!(store.health().value_root(), Some(at));
        let current = store.current_owner_retry().expect("new retry capability");
        assert_ne!(
            current, previous_retry,
            "identical publications still invalidate gate epochs"
        );
        assert_eq!(capabilities.borrow().last(), Some(&Some(current)));
        assert!(
            !store.retry_owner_at(&previous_retry, PageKey::Health, cx),
            "old rendered callback stays inert"
        );
    });
    let changed = OwnerFault::Lost("changed failure".into());
    gate.publish(OwnerState::Failed(changed.clone()));
    until(store, cx, "changed failure visibly lands", |store| {
        store.health().terminal() == &ResourceTerminal::Fault(owner_failure_value(&changed))
    });
    assert_ne!(
        store.read_with(cx, |store, _| store.stamp(&PageKey::Health)),
        stamp
    );
    assert!(
        events
            .borrow()
            .iter()
            .any(|event| event.touches(&PageKey::Health))
    );
    gate.publish(OwnerState::Starting);
    until(store, cx, "starting remains visible", |store| {
        matches!(store.owner.phase(), OwnerPhase::Starting)
            && store.health().activity() == Activity::Waiting
    });
    store.read_with(cx, |store, _| {
        assert!(Arc::ptr_eq(
            store.health().loaded_arc().expect("retained waiting bytes"),
            &retained
        ));
        assert_eq!(store.health().value_root(), Some(at));
        assert_eq!(store.current_owner_retry(), None);
    });
    // Same failure after Starting is a new terminal transition, not a repeat.
    gate.publish(OwnerState::Failed(changed.clone()));
    until(store, cx, "starting then failure still lands", |store| {
        store.health().terminal() == &ResourceTerminal::Fault(owner_failure_value(&changed))
    });
    let next = root("new-authority");
    gate.publish(OwnerState::Ready {
        key: next,
        mode: ServiceMode::Attached,
    });
    until(
        store,
        cx,
        "new serving authority reads actual values",
        |store| {
            store.owner_serving()
                && store.health().is_loaded()
                && store.health().value_root() == Some(next)
                && store.pool_activity().is_idle()
        },
    );
    gate.publish(OwnerState::Failed(changed.clone()));
    until(
        store,
        cx,
        "serving then same failure remains visible",
        |store| {
            store.health().terminal() == &ResourceTerminal::Fault(owner_failure_value(&changed))
        },
    );
    assert_eq!(
        store.read_with(cx, |store, _| store.health().value_root()),
        Some(next)
    );
    assert!(
        events
            .borrow()
            .iter()
            .filter(|event| event.touches(&PageKey::Health))
            .count()
            >= 4
    );
    drop(subscription);
}

#[gpui::test]
fn repeated_failed_phase_still_revokes_a_real_inflight_read(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (pool, entered, completed) = blocking_pool();
    let store = cx.update(|cx| {
        DataStore::install(
            cx,
            Arc::new(AppSnapshot::empty(root("inflight-repeat"))),
            Some(pool),
        )
    });
    until(&store, cx, "initial reads settle", |store| {
        store.health().is_loaded() && store.pool_activity().is_idle()
    });
    let fault = OwnerFault::Lost("unchanged failure with a new active read".into());
    let reference = symbol("RunningDuringFailedPhase");
    let key = PageKey::Symbol(reference.clone());
    store.update(cx, |store, cx| {
        store.owner_failed(&fault, cx);
        // Exercise the real revocation boundary even when the phase is
        // already Failed: a running page must never be classified as idle.
        let generation = store
            .pages
            .begin_forced(&key, store.snapshot.key())
            .expect("mint")
            .expect("fetch");
        store.submit(
            key.clone(),
            ReadRequest::Symbol(reference.clone()),
            generation,
            Priority::Normal,
            None,
            cx,
        );
        store.focused.insert(key.clone());
    });
    let actual = entered
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("read is actually running");
    store.update(cx, |store, cx| {
        assert!(store.pages.inflight(&key).is_some());
        store.owner_failed(&fault, cx);
        assert!(actual.is_cancelled());
        assert_eq!(store.pages.inflight(&key), None);
        assert_eq!(
            store.symbol(&reference).terminal(),
            &ResourceTerminal::Fault(owner_failure_value(&fault))
        );
    });
    let (cancelled, useful) = completed
        .recv_timeout(crate::runtime::wait::HUNG)
        .expect("constructed successful worker return");
    assert!(cancelled);
    assert_eq!(
        useful,
        Ok(PageValue::Symbol(page("RunningDuringFailedPhase")))
    );
    let stamp = store.read_with(cx, |store, _| store.stamp(&key));
    until(&store, cx, "old terminal is discarded", |store| {
        store.pool_activity().is_idle()
    });
    store.read_with(cx, |store, _| {
        assert_eq!(store.stamp(&key), stamp);
        assert_eq!(
            store.symbol(&reference).terminal(),
            &ResourceTerminal::Fault(owner_failure_value(&fault))
        );
        assert!(store.symbol(&reference).loaded_value().is_none());
    });
}

struct QuietValidationFault;
impl PageReader for QuietValidationFault {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Symbol(_)) {
            Err(ReadFailure::Fault(quiet_validation_fault()))
        } else {
            Fixture.read(request, context)
        }
    }
}

fn quiet_validation_fault() -> ErrorValue {
    ErrorValue::new(
        FaultCode::Transport,
        "the actual quiet-validation worker failed",
    )
}

#[gpui::test]
fn actual_watcher_replaces_changed_seeded_quiet_faults_and_keeps_identical_failures_stable(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let current = root("seeded-current");
    let saved_at = root("seeded-predecessor");
    let gate = OwnerGate::ready(current, ServiceMode::Attached);
    let actor = EngineActor::start(RootOnly, 8).expect("actor");
    let pool = ReadPool::start(1, |_| QuietValidationFault).expect("actual quiet reader");
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_owner(
            cx,
            DesktopRuntime::new(AppSnapshot::empty(current), actor),
            None,
            Some(pool),
            Some(gate.clone()),
            None,
        )
    });
    let store = &graph.store;
    until(store, cx, "initial route loaded", |store| {
        store.health().is_loaded() && store.pool_activity().is_idle()
    });
    let reference = symbol("SeededOwnerFailure");
    let key = PageKey::Symbol(reference.clone());
    let retained = store.update(cx, |store, cx| {
        assert!(store.pages.seed(
            SeedEntry::Symbol(reference.clone(), Arc::new(page("SeededOwnerFailure"))),
            saved_at
        ));
        let retained = store
            .symbol(&reference)
            .loaded_arc()
            .expect("actual installed seed")
            .clone();
        store.focused.insert(key.clone());
        store.ensure(key.clone(), cx);
        retained
    });
    until(store, cx, "actual quiet revalidation failed", |store| {
        store.symbol(&reference).terminal() == &ResourceTerminal::Fault(quiet_validation_fault())
            && store.pool_activity().is_idle()
    });
    assert!(store.read_with(cx, |store, _| store.pages.is_seeded(&key)));
    let events = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&events);
    let observed_key = key.clone();
    let subscription: Subscription = cx.update(|cx| {
        let mut watch = Watch::new(store.read(cx), [observed_key], &[Branch::GraphFocus]);
        cx.subscribe(store, move |store, event: &StoreEvent, cx| {
            if watch.changed(store.read(cx), event) {
                sink.borrow_mut().push(event.clone());
            }
        })
    });
    let first = OwnerFault::Lost("first failure on retained seeded bytes".into());
    gate.publish(OwnerState::Failed(first.clone()));
    until(
        store,
        cx,
        "first owner fault replaces quiet validation fault",
        |store| {
            store.symbol(&reference).terminal()
                == &ResourceTerminal::Fault(owner_failure_value(&first))
        },
    );
    cx.run_until_parked();
    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|event| event.touches(&key))
            .count(),
        1,
        "only the changed fault publishes, with no Waiting publication"
    );
    let stamp = store.read_with(cx, |store, _| store.stamp(&key));
    let old_retry = store.read_with(cx, |store, _| store.current_owner_retry().expect("retry"));
    events.borrow_mut().clear();
    gate.publish(OwnerState::Failed(first.clone()));
    crate::runtime::wait::until("same seeded fault refreshes retry capability", || {
        cx.run_until_parked();
        events
            .borrow()
            .iter()
            .any(|event| event.is_branch(Branch::GraphFocus))
    });
    assert!(!events.borrow().iter().any(|event| event.touches(&key)));
    assert_eq!(store.read_with(cx, |store, _| store.stamp(&key)), stamp);
    assert_ne!(
        store.read_with(cx, |store, _| store.current_owner_retry()),
        Some(old_retry)
    );
    events.borrow_mut().clear();
    let changed = OwnerFault::Lost("changed failure on retained seeded bytes".into());
    gate.publish(OwnerState::Failed(changed.clone()));
    until(
        store,
        cx,
        "changed owner failure replaces current seeded fault",
        |store| {
            store.symbol(&reference).terminal()
                == &ResourceTerminal::Fault(owner_failure_value(&changed))
        },
    );
    cx.run_until_parked();
    assert_eq!(
        events
            .borrow()
            .iter()
            .filter(|event| event.touches(&key))
            .count(),
        1
    );
    store.read_with(cx, |store, _| {
        assert_ne!(store.stamp(&key), stamp);
        assert!(store.pages.is_seeded(&key));
        assert_eq!(store.symbol(&reference).activity(), Activity::Stopped);
        assert!(Arc::ptr_eq(
            store
                .symbol(&reference)
                .loaded_arc()
                .expect("exact retained bytes"),
            &retained
        ));
        assert_eq!(store.symbol(&reference).value_root(), Some(saved_at));
        assert_eq!(store.pages.inflight(&key), None);
    });
    // Ready at another authority must still attempt a genuine quiet worker
    // read rather than rebasing the saved bytes into current evidence.
    let next = root("seeded-next-authority");
    gate.publish(OwnerState::Ready {
        key: next,
        mode: ServiceMode::Attached,
    });
    until(
        store,
        cx,
        "another authority genuinely revalidates the seed",
        |store| {
            store.owner_serving()
                && store.snapshot.key().same_authority(next)
                && store.symbol(&reference).terminal()
                    == &ResourceTerminal::Fault(quiet_validation_fault())
                && store.pool_activity().is_idle()
        },
    );
    gate.publish(OwnerState::Failed(changed.clone()));
    until(
        store,
        cx,
        "same owner fault at a new authority remains visible",
        |store| {
            store.symbol(&reference).terminal()
                == &ResourceTerminal::Fault(owner_failure_value(&changed))
        },
    );
    store.read_with(cx, |store, _| {
        assert!(Arc::ptr_eq(
            store
                .symbol(&reference)
                .loaded_arc()
                .expect("old bytes after new authority failed"),
            &retained
        ));
        assert_eq!(store.symbol(&reference).value_root(), Some(saved_at));
        assert!(
            store
                .pages
                .idle_fault_at(&key, next, &owner_failure_value(&changed))
        );
    });
    drop(subscription);
}
