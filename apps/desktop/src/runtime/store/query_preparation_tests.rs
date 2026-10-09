//! Model/worker scheduling laws use GPUI's unit-test clock. They establish
//! no native pixels, real-owner readiness, or live GUI acceptance.

// Explicit pattern guards fail an invalid authored-law setup immediately.
#![allow(clippy::panic)]

use super::*;
use crate::core::{Activity, QueryPreparation, VersionedRoot};
use crate::model::browse::BrowseKey;
use crate::model::pages::PageValue;
use crate::navigation::Overlay;
use crate::runtime::reads::{PageReader, ReadContext};
use gpui::{BackgroundExecutor, TestAppContext};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
enum ReplyMode { ReadyAt(Instant), RealFailure, BlockUntilCancelled }

struct PreparingReader {
    clock: BackgroundExecutor,
    basis: backend_library::ViewRevision,
    reads: Arc<AtomicUsize>,
    mode: ReplyMode,
    attempts: std::collections::BTreeMap<PageKey, usize>,
}

fn ready(request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
    let ReadRequest::Browse(BrowseKey::Find(query)) = request else {
        return crate::shell::tests::Fixture.read(request, context);
    };
    // The existing shared fixture serves Search, and deliberately serves no
    // Browse pages. Compose this law's Find presentation from its actual
    // Search facts through the production view preparation boundary.
    let PageValue::Search(search) = crate::shell::tests::Fixture.read(&ReadRequest::Search(query.clone()), context)? else {
        panic!("the authored Search fixture must return Search facts");
    };
    let answers = crate::model::pages::Known::Known(search);
    let package_coverage = crate::model::pages::Known::Known(());
    let prepared = Arc::new(crate::runtime::browse_views::prepare_find(query.text.as_ref(), &answers, &[], &package_coverage));
    Ok(PageValue::Browse(crate::model::browse::BrowseValue::Find(Arc::new(crate::model::browse::FindModel {
        answers, packages: Arc::from([]), package_coverage, prepared,
    }))))
}

impl PageReader for PreparingReader {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if !matches!(request, ReadRequest::Search(_) | ReadRequest::Browse(BrowseKey::Find(_))) {
            return crate::shell::tests::Fixture.read(request, context);
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let key = match request {
            ReadRequest::Search(query) => PageKey::Search(query.clone()),
            ReadRequest::Browse(BrowseKey::Find(query)) => PageKey::Browse(BrowseKey::Find(query.clone())),
            _ => panic!("the authored preparation reader only owns query requests"),
        };
        let count = self.attempts.entry(key).or_default();
        let attempt = *count;
        *count += 1;
        if attempt > 0 {
            match self.mode {
                ReplyMode::ReadyAt(at) if self.clock.now() >= at => return ready(request, context),
                ReplyMode::RealFailure => return Err(ReadFailure::Fault(ErrorValue::new(FaultCode::Protocol, "actual preparation failed"))),
                ReplyMode::BlockUntilCancelled if attempt % 2 == 1 => {
                    crate::runtime::wait::until("the owned read was cancelled", || context.cancel.is_cancelled());
                    return Err(ReadFailure::Cancelled);
                }
                _ => {}
            }
        }
        Err(ReadFailure::QueryPreparation(QueryPreparation {
            basis: self.basis,
            state: if attempt == 0 { backend_library::QueryPreparationState::Retiring }
                else { backend_library::QueryPreparationState::Preparing },
        }))
    }
}

fn root() -> VersionedRoot {
    VersionedRoot::synthetic(backend_library::view_state_root(&[("query".into(), "unit law".into())]), 1)
}

fn install(cx: &mut TestAppContext, snapshot: AppSnapshot, mode: ReplyMode) -> (Entity<DataStore>, Arc<AtomicUsize>) {
    cx.executor().allow_parking();
    let reads = Arc::new(AtomicUsize::new(0));
    let worker_reads = reads.clone();
    let clock = cx.executor().clone();
    let Ok(pool) = ReadPool::start(1, move |_| PreparingReader { clock: clock.clone(), basis: root().root().into(), reads: worker_reads.clone(), mode, attempts: std::collections::BTreeMap::new() }) else { panic!("the authored worker pool must start"); };
    let store = cx.update(|cx| DataStore::install(cx, Arc::new(snapshot), Some(pool)));
    settle(cx, &store);
    (store, reads)
}

fn settle(cx: &mut TestAppContext, store: &Entity<DataStore>) {
    crate::runtime::wait::until("real worker outcomes landed", || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| store.pool_activity().is_idle())
    });
}

#[gpui::test]
fn active_find_ask_and_graph_queries_advance_near_the_original_deadline_without_manual_retry(cx: &mut TestAppContext) {
    for lane in ["find", "ask", "graph"] {
        let started = cx.executor().now();
        let mut snapshot = AppSnapshot::empty(root());
        if lane == "ask" {
            let mut session = snapshot.session().clone();
            session.open_overlay(Overlay::CommandPalette);
            snapshot = snapshot.with_session(session);
        }
        let route = snapshot.route().clone();
        let (store, reads) = install(cx, snapshot, ReplyMode::ReadyAt(started + Duration::from_millis(29_500)));
        let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
        let key = if lane == "find" { PageKey::Browse(BrowseKey::Find(query.clone())) } else { PageKey::Search(query.clone()) };
        store.update(cx, |store, cx| match lane {
            "find" => store.focus(vec![key.clone()], cx),
            "ask" => store.observe_ask_query(Some(query.clone()), cx),
            _ => { store.observe_graph_query(query.clone(), cx); }
        });
        settle(cx, &store);
        store.read_with(cx, |store, _| {
            assert!(store.preparation_task.is_some());
            assert!(store.preparation_token(&key).is_some());
            assert_eq!(store.pool.as_ref().map(|pool| pool.load().held.total()), Some(0), "awaiting holds no RPC permit");
        });
        for _ in 0..119 {
            cx.executor().advance_clock(Duration::from_millis(250));
            settle(cx, &store);
        }
        store.read_with(cx, |store, _| {
            assert_eq!(store.pages.activity(&key), Activity::Rest, "{lane} advances itself at 29.75s");
            assert!(store.pages.query_preparation(&key).is_none());
            assert!(store.preparation_token(&key).is_none());
            assert!(store.preparation_task.is_none());
            assert_eq!(store.snapshot.route(), &route, "read readiness does not jump the selected view");
        });
        assert!(reads.load(Ordering::SeqCst) > 8, "the old short cap cannot satisfy this law");
        store.update(cx, |store, _| { store.commit_close(); });
    }
}

#[gpui::test]
fn the_original_deadline_interrupts_a_running_retry_and_retains_manual_awaiting(cx: &mut TestAppContext) {
    let (store, reads) = install(cx, AppSnapshot::empty(root()), ReplyMode::BlockUntilCancelled);
    let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
    let key = PageKey::Search(query.clone());
    store.update(cx, |store, cx| { store.observe_graph_query(query, cx); });
    settle(cx, &store);
    cx.executor().advance_clock(Duration::from_millis(250));
    crate::runtime::wait::until("the retry entered its real worker", || { cx.run_until_parked(); reads.load(Ordering::SeqCst) == 2 });
    cx.executor().advance_clock(Duration::from_millis(29_750));
    settle(cx, &store);
    store.read_with(cx, |store, _| {
        assert!(store.preparation_token(&key).is_some());
        assert!(store.preparation_task.is_none());
        assert_eq!(store.pages.activity(&key), Activity::Waiting);
        assert!(store.pages.query_preparation(&key).is_some());
        assert!(store.pages.inflight(&key).is_none());
    });
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), 2, "expiry cannot renew the deadline or spin");
}

/// The submitter will block in an evicted job's cancellation callback after
/// its replacement is enqueued. The occupied worker can then consume that
/// replacement without waiting for submit's final ReadyNotify.
struct CallbackHeldReader {
    refused: bool,
    occupied: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    occupied_entered: std::sync::mpsc::Sender<()>,
    retry_entered: std::sync::mpsc::Sender<CancellationToken>,
    interrupted: std::sync::mpsc::Sender<()>,
}

impl PageReader for CallbackHeldReader {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Search(query) = request else { return ready(request, context); };
        if query.text.as_ref() == "occupier" {
            let gate = self.occupied.clone();
            let wake_gate = gate.clone();
            let _wake = context.cancel.on_cancel(move || {
                *wake_gate.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                wake_gate.1.notify_all();
            });
            let _ = self.occupied_entered.send(());
            let mut released = gate.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*released && !context.cancel.is_cancelled() {
                released = gate.1.wait(released).unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            return ready(request, context);
        }
        if query.text.as_ref() != "retry" { return ready(request, context); }
        if !self.refused {
            self.refused = true;
            return Err(ReadFailure::QueryPreparation(QueryPreparation {
                basis: root().root().into(), state: backend_library::QueryPreparationState::Preparing,
            }));
        }
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let wake_gate = gate.clone();
        let _wake = context.cancel.on_cancel(move || {
            *wake_gate.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            wake_gate.1.notify_all();
        });
        let _ = self.retry_entered.send(context.cancel.clone());
        let mut cancelled = gate.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*cancelled {
            cancelled = gate.1.wait(cancelled).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        assert!(context.cancel.is_cancelled(), "the real worker must observe its actual cancellation token");
        let _ = self.interrupted.send(());
        Err(ReadFailure::Cancelled)
    }
}

#[gpui::test]
fn retry_deadline_interrupts_the_worker_while_pool_submit_is_held_in_a_cancellation_callback(cx: &mut TestAppContext) {
    use std::sync::{Condvar, Mutex, mpsc};
    use std::sync::atomic::AtomicBool;
    cx.executor().allow_parking();
    let occupied = Arc::new((Mutex::new(false), Condvar::new()));
    let (occupied_entered, occupied_rx) = mpsc::channel();
    let (retry_entered, retry_rx) = mpsc::channel();
    let (interrupted, interrupted_rx) = mpsc::channel();
    let Some(two) = std::num::NonZeroUsize::new(2) else { panic!("two admissions"); };
    let reader_gate = occupied.clone();
    let Ok(pool) = ReadPool::start_with(1, crate::runtime::reads::ReadLimits::new(two, 1), move |_| CallbackHeldReader {
        refused: false, occupied: reader_gate.clone(), occupied_entered: occupied_entered.clone(),
        retry_entered: retry_entered.clone(), interrupted: interrupted.clone(),
    }) else { panic!("the callback law's real worker must start"); };
    let store = cx.update(|cx| DataStore::install(cx, Arc::new(AppSnapshot::empty(root())), Some(pool)));
    settle(cx, &store);
    let Some(query) = SearchQuery::new("retry", 50) else { panic!("retry query"); };
    let key = PageKey::Search(query.clone());
    let started = cx.executor().now();
    store.update(cx, |store, cx| { store.observe_graph_query(query, cx); });
    settle(cx, &store);
    let Some(token) = store.read_with(cx, |store, _| store.preparation_token(&key)) else { panic!("original refusal token"); };
    // Disable foreground delivery before the forced handoff. The watcher
    // drives only background executor tasks, never App/Entity update/drain.
    store.update(cx, |store, _| { store.wake_task = None; });
    let Some(occupier) = SearchQuery::new("occupier", 50) else { panic!("occupier query"); };
    let Some(victim) = SearchQuery::new("victim", 50) else { panic!("victim query"); };
    let Some(generation) = Generation::new(1) else { panic!("authored generation"); };
    let victim_cancel = CancellationToken::new();
    store.read_with(cx, |store, _| {
        let Some(pool) = &store.pool else { panic!("installed pool"); };
        assert!(pool.submit(ReadJob { key: PageKey::Search(occupier.clone()), request: ReadRequest::Search(occupier),
            generation, priority: Priority::Normal, cancel: CancellationToken::new(), affinity: None }).is_ok());
    });
    assert!(occupied_rx.recv_timeout(Duration::from_secs(5)).is_ok());
    store.read_with(cx, |store, _| {
        let Some(pool) = &store.pool else { panic!("installed pool"); };
        assert!(pool.submit(ReadJob { key: PageKey::Search(victim.clone()), request: ReadRequest::Search(victim),
            generation, priority: Priority::Prefetch, cancel: victim_cancel.clone(), affinity: None }).is_ok());
    });
    let (callback_entered, callback_rx) = mpsc::channel();
    let (release_callback, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let callback_active = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_active.clone();
    let _registration = victim_cancel.on_cancel(move || {
        callback_flag.store(true, Ordering::Release);
        let _ = callback_entered.send(());
        let _ = release_rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner).recv_timeout(Duration::from_secs(10));
        callback_flag.store(false, Ordering::Release);
    });
    let submitted = Arc::new(AtomicBool::new(false));
    let submit_flag = submitted.clone();
    let clock = cx.executor().clone();
    let dispatcher = cx.dispatcher.clone();
    cx.executor().advance_clock(Duration::from_millis(250));
    let proof = std::thread::scope(|scope| {
        let watcher = scope.spawn(move || {
            let callback_held = callback_rx.recv_timeout(Duration::from_secs(5)).is_ok();
            *occupied.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            occupied.1.notify_all();
            let actual_token = retry_rx.recv_timeout(Duration::from_secs(5)).ok();
            dispatcher.advance_clock((started + query_preparation::READ_BUDGET).saturating_duration_since(clock.now()));
            let bound = Instant::now() + Duration::from_secs(5);
            let worker_interrupted = loop {
                match interrupted_rx.try_recv() {
                    Ok(()) => break true,
                    Err(mpsc::TryRecvError::Disconnected) => break false,
                    Err(mpsc::TryRecvError::Empty) if Instant::now() >= bound => break false,
                    Err(mpsc::TryRecvError::Empty) => { dispatcher.tick(true); std::thread::yield_now(); }
                }
            };
            let proof = (callback_held, worker_interrupted,
                actual_token.as_ref().is_some_and(CancellationToken::is_cancelled),
                callback_active.load(Ordering::Acquire), !submit_flag.load(Ordering::Acquire));
            // Release even on a failed law, so no real worker or submitter
            // is stranded while the assertions below report the failure.
            let _ = release_callback.send(());
            proof
        });
        store.update(cx, |store, cx| store.retry_preparation(key.clone(), &token, false, cx));
        submitted.store(true, Ordering::Release);
        let Ok(proof) = watcher.join() else { panic!("the owned callback watcher must retire"); };
        proof
    });
    assert_eq!(proof, (true, true, true, true, true),
        "the actual retry worker must be interrupted before pool.submit returns, with no foreground drain");
    crate::runtime::wait::until("both real read outcomes are posted without foreground landing", || {
        store.read_with(cx, |store, _| store.pool.as_ref().is_some_and(|pool| {
            let load = pool.load();
            load.running == 0 && load.queued == 0 && load.undelivered == 2
        }))
    });
    store.update(cx, DataStore::drain);
    store.read_with(cx, |store, _| {
        assert_eq!(store.pages.activity(&key), Activity::Waiting);
        assert!(store.pages.inflight(&key).is_none());
        assert!(store.preparation_token(&key).is_some());
    });
}

#[gpui::test]
fn a_close_pause_keeps_the_original_deadline_and_resuming_does_not_renew_it(cx: &mut TestAppContext) {
    let (store, reads) = install(cx, AppSnapshot::empty(root()), ReplyMode::BlockUntilCancelled);
    let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
    let key = PageKey::Search(query.clone());
    store.update(cx, |store, cx| { store.observe_graph_query(query, cx); });
    settle(cx, &store);
    cx.executor().advance_clock(Duration::from_millis(250));
    crate::runtime::wait::until("the retry entered its real worker", || { cx.run_until_parked(); reads.load(Ordering::SeqCst) == 2 });
    store.update(cx, |store, cx| store.set_close_paused(true, cx));
    cx.executor().advance_clock(Duration::from_millis(29_750));
    crate::runtime::wait::until("the deadline interrupted the worker while close was paused", || {
        cx.run_until_parked();
        store.read_with(cx, |store, _| store.pool.as_ref().is_some_and(|pool| pool.load().running == 0))
    });
    assert!(store.read_with(cx, |store, _| store.pages.inflight(&key).is_some()), "close pause still defers ordinary terminal admission");
    store.update(cx, |store, cx| store.set_close_paused(false, cx));
    settle(cx, &store);
    store.read_with(cx, |store, _| {
        assert_eq!(store.pages.activity(&key), Activity::Waiting);
        assert!(store.preparation_token(&key).is_some());
        assert!(store.preparation_task.is_none());
        assert!(store.pages.inflight(&key).is_none());
    });
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), 2, "resuming a close dialogue cannot restart or extend preparation");
}

#[gpui::test]
fn ask_draft_replacement_and_clear_cancel_the_old_worker_and_readmit_a_fresh_visit(cx: &mut TestAppContext) {
    let mut snapshot = AppSnapshot::empty(root());
    let mut session = snapshot.session().clone();
    session.open_overlay(Overlay::CommandPalette);
    snapshot = snapshot.with_session(session);
    let route = snapshot.route().clone();
    let (store, reads) = install(cx, snapshot, ReplyMode::BlockUntilCancelled);
    let Some(first) = SearchQuery::new("first", 50) else { panic!("first query"); };
    let Some(second) = SearchQuery::new("second", 50) else { panic!("second query"); };
    let Some(unrelated) = SearchQuery::new("unrelated", 50) else { panic!("unrelated query"); };
    let first_key = PageKey::Search(first.clone());
    let second_key = PageKey::Search(second.clone());
    let unrelated_key = PageKey::Search(unrelated);
    store.update(cx, |store, cx| {
        store.observe_ask_query(Some(first), cx);
        store.ensure(unrelated_key.clone(), cx);
    });
    settle(cx, &store);
    let unrelated_stamp = store.read_with(cx, |store, _| store.pages.stamp(&unrelated_key));
    assert!(store.read_with(cx, |store, _| store.pages.query_preparation(&unrelated_key).is_some()));
    cx.executor().advance_clock(Duration::from_millis(250));
    crate::runtime::wait::until("first Ask retry is blocked in its worker", || { cx.run_until_parked(); reads.load(Ordering::SeqCst) == 3 });
    store.update(cx, |store, cx| store.observe_ask_query(Some(second.clone()), cx));
    settle(cx, &store);
    store.read_with(cx, |store, _| {
        assert!(store.pages.inflight(&first_key).is_none());
        assert!(store.preparation_token(&first_key).is_none());
        assert_eq!(store.pages.stamp(&unrelated_key), unrelated_stamp);
        assert!(store.pages.query_preparation(&unrelated_key).is_some());
    });
    let Some(before_clear) = store.read_with(cx, |store, _| store.preparation_token(&second_key)) else { panic!("replacement Ask token"); };
    store.update(cx, |store, cx| store.check_preparation(second_key.clone(), &before_clear, cx));
    crate::runtime::wait::until("replacement Ask retry is blocked in its worker", || { cx.run_until_parked(); reads.load(Ordering::SeqCst) == 5 });
    let Some(revoked_generation) = store.read_with(cx, |store, _| store.pages.inflight(&second_key)) else { panic!("the actual blocked Ask retry must own its generation"); };
    // Actual blank/invalid editor transitions unobserve before debounce;
    // a later settled valid draft must acquire new generation authority.
    store.update(cx, |store, cx| store.observe_ask_query(None, cx));
    settle(cx, &store);
    assert!(store.read_with(cx, |store, _| store.ask_query.is_none()
        && store.preparation_token(&second_key).is_none()
        && store.pages.inflight(&second_key).is_none()));
    store.update(cx, |store, cx| store.observe_ask_query(Some(second), cx));
    settle(cx, &store);
    let Some(after_clear) = store.read_with(cx, |store, _| store.preparation_token(&second_key)) else { panic!("fresh valid Ask token"); };
    assert_ne!(before_clear.generation, after_clear.generation);
    assert_ne!(revoked_generation, after_clear.generation, "invalid→valid cannot reuse the actual cancelled worker generation");
    store.read_with(cx, |store, _| {
        assert_eq!(after_clear.root, store.snapshot.key());
        assert_eq!(Some(after_clear.owner.clone()), store.current_owner_attachment());
        assert_eq!(store.snapshot.route(), &route);
        assert_eq!(store.pages.stamp(&unrelated_key), unrelated_stamp);
        assert!(store.pages.query_preparation(&unrelated_key).is_some());
    });
    store.update(cx, |store, cx| store.check_preparation(second_key.clone(), &before_clear, cx));
    assert_eq!(store.read_with(cx, |store, _| store.preparation_token(&second_key)), Some(after_clear));
}

#[gpui::test]
fn a_real_failure_terminates_preparation_and_old_graph_release_cannot_cancel_a_successor(cx: &mut TestAppContext) {
    let (store, reads) = install(cx, AppSnapshot::empty(root()), ReplyMode::RealFailure);
    let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
    let key = PageKey::Search(query.clone());
    let Some(old) = store.update(cx, |store, cx| store.observe_graph_query(query.clone(), cx)) else { panic!("the first graph open must be admitted"); };
    settle(cx, &store);
    let Some(current) = store.update(cx, |store, cx| store.observe_graph_query(query, cx)) else { panic!("the same-query successor must be admitted"); };
    store.update(cx, |store, cx| store.release_graph_query(&old, cx));
    assert!(store.read_with(cx, |store, _| store.graph_query.as_ref().is_some_and(|interest| Arc::ptr_eq(&interest.identity, &current.identity))));
    settle(cx, &store);
    store.read_with(cx, |store, _| {
        assert!(matches!(store.search(&current.query).terminal(), ResourceTerminal::Fault(error) if error.message() == "actual preparation failed"));
        assert!(store.preparation_token(&key).is_none());
        assert!(store.preparation_task.is_none());
    });
    let count = reads.load(Ordering::SeqCst);
    cx.executor().advance_clock(Duration::from_secs(30));
    cx.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), count);
}

#[gpui::test]
fn navigation_and_owner_change_revoke_pending_tokens_even_when_a_query_key_returns(cx: &mut TestAppContext) {
    let started = cx.executor().now();
    let (store, _) = install(cx, AppSnapshot::empty(root()), ReplyMode::ReadyAt(started + Duration::from_secs(60)));
    let Some(query) = SearchQuery::new("RelationLabel", 50) else { panic!("the authored query must parse"); };
    let key = PageKey::Browse(BrowseKey::Find(query.clone()));
    let Some(other_query) = SearchQuery::new("unrelated", 50) else { panic!("the unrelated query must parse"); };
    let other_key = PageKey::Browse(BrowseKey::Find(other_query));
    store.update(cx, |store, cx| store.focus(vec![key.clone(), other_key.clone()], cx));
    settle(cx, &store);
    let Some(old) = store.read_with(cx, |store, _| store.preparation_token(&key)) else { panic!("the old visit must own a preparation token"); };
    let Some(unrelated) = store.read_with(cx, |store, _| store.preparation_token(&other_key)) else { panic!("the unrelated pending query must own its token"); };
    store.update(cx, |store, cx| store.focus(vec![other_key.clone()], cx));
    store.read_with(cx, |store, _| {
        assert!(store.preparation_token(&key).is_none());
        assert_eq!(store.preparation_token(&other_key), Some(unrelated.clone()), "dropping one query leaves the unrelated pending owner unchanged");
    });
    store.update(cx, |store, cx| store.focus(vec![key.clone(), other_key.clone()], cx));
    settle(cx, &store);
    let Some(current) = store.read_with(cx, |store, _| store.preparation_token(&key)) else { panic!("the new visit must own a preparation token"); };
    assert_ne!(old.generation, current.generation);
    store.update(cx, |store, cx| store.check_preparation(key.clone(), &old, cx));
    assert_eq!(store.read_with(cx, |store, _| store.preparation_token(&key)), Some(current));
    assert_eq!(store.read_with(cx, |store, _| store.preparation_token(&other_key)), Some(unrelated));
    store.update(cx, DataStore::owner_starting);
    assert!(store.read_with(cx, |store, _| store.preparation_task.is_none()
        && store.preparation_token(&key).is_none() && store.preparation_token(&other_key).is_none()));

    // Ask remains visible across a preview route change. Its event-owned
    // draft observation must explicitly admit the replacement generation.
    store.update(cx, |store, cx| {
        store.owner = OwnerLink::serving();
        let mut session = store.snapshot.session().clone();
        session.open_overlay(Overlay::CommandPalette);
        let snapshot = Arc::new(store.snapshot.with_session(session));
        store.admit_snapshot(snapshot, cx);
        store.observe_ask_query(Some(query.clone()), cx);
    });
    settle(cx, &store);
    let search_key = PageKey::Search(query.clone());
    let Some(old) = store.read_with(cx, |store, _| store.preparation_token(&search_key)) else { panic!("the old Ask visit must own a preparation token"); };
    store.update(cx, |store, cx| {
        let mut session = store.snapshot.session().clone();
        session.route = Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::FindHome));
        let snapshot = Arc::new(store.snapshot.with_session(session));
        store.admit_snapshot(snapshot, cx);
        assert!(store.preparation_token(&search_key).is_none());
        store.observe_ask_query(Some(query.clone()), cx); // the actual Ask Route subscription does this
    });
    settle(cx, &store);
    let Some(current) = store.read_with(cx, |store, _| store.preparation_token(&search_key)) else { panic!("the replacement Ask visit must own a preparation token"); };
    assert_ne!(old.generation, current.generation);
    store.update(cx, |store, cx| store.check_preparation(search_key.clone(), &old, cx));
    assert_eq!(store.read_with(cx, |store, _| store.preparation_token(&search_key)), Some(current));
}
