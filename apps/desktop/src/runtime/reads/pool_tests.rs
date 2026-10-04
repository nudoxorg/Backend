//! The pool's admission, outbox, and shutdown under adverse schedules, on
//! real worker threads. Each test names the schedule it forces and the
//! bound it proves; the interleaving-exhaustive model of the same rules is
//! in `schedule_tests`.

use super::*;
use crate::model::pages::{HealthModel, IngestModel, SymbolRef};
use crate::runtime::reads::{ReadContext, ReadRequest};
use crate::runtime::wait;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

pub(super) fn health(rows: u64) -> PageValue {
    PageValue::Health(HealthModel {
        lanes: backend_present::CoverageLine::new(&[], Some(rows)),
        rows,
        ingest: IngestModel {
            files_discovered: 0,
            files_indexed: 0,
            files_unavailable: 0,
            declarations: 0,
            languages: Arc::from([]),
            faults: Arc::from([]),
        },
        ready_capabilities: Arc::from([]),
        not_ready_capabilities: Arc::from([]),
    })
}

pub(super) fn rows(delivery: &Delivery) -> Option<u64> {
    match delivery {
        Delivery::Partial(PageValue::Health(model))
        | Delivery::Terminal(Ok(PageValue::Health(model))) => Some(model.rows),
        _ => None,
    }
}

pub(super) fn key(name: &str) -> PageKey {
    PageKey::Symbol(SymbolRef::new(name).expect("symbol"))
}

pub(super) fn job(name: &str, generation: u64, priority: Priority) -> ReadJob {
    let symbol = SymbolRef::new(name).expect("symbol");
    ReadJob {
        key: PageKey::Symbol(symbol.clone()),
        request: ReadRequest::Symbol(symbol),
        generation: Generation::new(generation),
        priority,
        cancel: CancellationToken::new(),
        affinity: None,
    }
}

pub(super) fn limits(reads: usize, prefetch: usize) -> ReadLimits {
    ReadLimits::new(NonZeroUsize::new(reads).expect("nonzero"), prefetch)
}

const ALL: NonZeroUsize = NonZeroUsize::MAX;

fn name_of(request: &ReadRequest) -> String {
    match request {
        ReadRequest::Symbol(symbol) => symbol.as_str().to_owned(),
        other => format!("{other:?}"),
    }
}

/// Answers at once, except names starting with `slow`, which wait until the
/// test opens `gate` (or the read is cancelled).
#[derive(Clone)]
struct Gated {
    gate: Arc<AtomicBool>,
}

impl PageReader for Gated {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if name_of(request).starts_with("slow") {
            wait::until("the test opened the gate or cancelled the read", || {
                self.gate.load(Ordering::Acquire) || context.cancel.is_cancelled()
            });
            if context.cancel.is_cancelled() {
                return Err(ReadFailure::Cancelled);
            }
        }
        Ok(health(1))
    }
}

fn gated(workers: usize, limits: ReadLimits) -> (ReadPool, Arc<AtomicBool>) {
    let gate = Arc::new(AtomicBool::new(false));
    let reader = Gated {
        gate: Arc::clone(&gate),
    };
    let pool = ReadPool::start_with(workers, limits, move |_| reader.clone()).expect("pool");
    (pool, gate)
}

/// Schedule: workers finish faster than the UI takes anything. The pool
/// admits exactly its limit and then refuses, and a taken outcome keeps
/// its admission until the UI drops it, not until its worker returned.
#[test]
fn completed_outcomes_hold_their_admission_until_the_ui_drops_them() {
    let (pool, _gate) = gated(2, limits(4, 2));
    for round in 0..4 {
        assert_eq!(
            pool.submit(job(&format!("fast-{round}"), round, Priority::Normal)),
            Ok(Admitted { evicted: None })
        );
    }
    wait::until("every read finished on its worker", || {
        pool.load().undelivered == 4
    });
    let load = pool.load();
    assert_eq!(
        (load.queued, load.running),
        (0, 0),
        "no worker holds anything"
    );
    assert_eq!(
        load.held.total(),
        4,
        "undelivered payloads still hold their reads"
    );
    assert_eq!(
        pool.submit(job("fast-late", 9, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads)),
        "a slow UI cannot let completed payloads pile up past the limit"
    );

    let taken = pool.take(NonZeroUsize::new(3).expect("three"));
    assert_eq!((taken.outcomes.len(), taken.remaining), (3, 1));
    assert_eq!(
        pool.load().held.total(),
        4,
        "outcomes in the UI's hands are still held"
    );
    assert_eq!(
        pool.submit(job("fast-late", 9, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
    let clone = taken.outcomes[0].clone();
    drop(taken);
    assert_eq!(
        pool.load().held.total(),
        2,
        "a clone shares its outcome's admission"
    );
    drop(clone);
    assert_eq!(
        pool.load().held.total(),
        1,
        "landing and dropping returns the admissions"
    );
    for round in 10..13 {
        assert!(
            pool.submit(job(&format!("fast-{round}"), round, Priority::Normal))
                .is_ok()
        );
    }
    assert_eq!(
        pool.submit(job("fast-over", 20, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
}

/// Schedule: one read posts ten thousand partial pages while the UI takes
/// nothing. Only the newest stays undelivered; the flood never takes a
/// second admission; the terminal outcome replaces the partial in place.
#[test]
fn a_partial_flood_keeps_one_undelivered_page_per_read() {
    struct Flood {
        flooded: mpsc::Sender<()>,
        release: Arc<AtomicBool>,
    }
    impl PageReader for Flood {
        fn read(
            &mut self,
            _: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            for page in 1..=10_000 {
                context.publish(health(page));
            }
            self.flooded.send(()).expect("flood signal");
            wait::until("the test released the read", || {
                self.release.load(Ordering::Acquire)
            });
            Ok(health(20_000))
        }
    }
    let (flooded, done) = mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let reader_release = Arc::clone(&release);
    let pool = ReadPool::start_with(1, limits(4, 2), move |_| Flood {
        flooded: flooded.clone(),
        release: Arc::clone(&reader_release),
    })
    .expect("pool");
    assert!(pool.submit(job("flood", 1, Priority::Normal)).is_ok());
    done.recv_timeout(wait::HUNG).expect("flood finished");
    let load = pool.load();
    assert_eq!((load.undelivered, load.held.total()), (1, 1));
    let newest = pool.take(ALL);
    assert_eq!(
        newest
            .outcomes
            .iter()
            .map(|outcome| rows(&outcome.delivery))
            .collect::<Vec<_>>(),
        [Some(10_000)]
    );
    drop(newest);

    release.store(true, Ordering::Release);
    wait::until("the read finished", || pool.running() == 0);
    let terminal = pool.take(ALL);
    assert_eq!(terminal.outcomes.len(), 1);
    assert!(matches!(
        terminal.outcomes[0].delivery,
        Delivery::Terminal(Ok(_))
    ));
    drop(terminal);
    assert_eq!(pool.load().held, Held::default());
}

/// Schedule: a partial page is still undelivered when its read finishes.
/// The terminal outcome takes the partial's place; the UI never lands the
/// stale partial after the final page.
#[test]
fn a_terminal_outcome_replaces_its_undelivered_partial() {
    struct Staged;
    impl PageReader for Staged {
        fn read(
            &mut self,
            _: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            context.publish(health(1));
            Ok(health(2))
        }
    }
    let pool = ReadPool::start_with(1, limits(4, 2), |_| Staged).expect("pool");
    assert!(pool.submit(job("staged", 1, Priority::Normal)).is_ok());
    wait::until("the read finished", || {
        pool.running() == 0 && pool.load().undelivered == 1
    });
    let batch = pool.take(ALL);
    assert_eq!(batch.outcomes.len(), 1);
    assert!(matches!(batch.outcomes[0].delivery, Delivery::Terminal(_)));
    assert_eq!(rows(&batch.outcomes[0].delivery), Some(2));
}

/// Schedule: the reader passed its own cancellation check, the UI cancels
/// the key, and only then does the reader post a second partial page. The
/// outbox refuses it under its lock; the first partial was withdrawn; the
/// read's admission returns when its terminal outcome is dropped.
#[test]
fn a_partial_posted_after_cancel_never_reaches_the_ui() {
    struct Racing {
        posted: mpsc::Sender<u64>,
        step: Arc<AtomicUsize>,
    }
    impl PageReader for Racing {
        fn read(
            &mut self,
            _: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            let progress = context.progress.expect("the pool publishes progress");
            progress(health(1));
            self.posted.send(1).expect("posted signal");
            wait::until("the test cancelled the read", || {
                self.step.load(Ordering::Acquire) >= 1
            });
            // As if the reader checked `cancel` just before the UI set it.
            progress(health(2));
            self.posted.send(2).expect("posted signal");
            wait::until("the test looked at the outbox", || {
                self.step.load(Ordering::Acquire) >= 2
            });
            Ok(health(3))
        }
    }
    let (posted, attempts) = mpsc::channel();
    let step = Arc::new(AtomicUsize::new(0));
    let reader_step = Arc::clone(&step);
    let pool = ReadPool::start_with(1, limits(4, 2), move |_| Racing {
        posted: posted.clone(),
        step: Arc::clone(&reader_step),
    })
    .expect("pool");
    assert!(pool.submit(job("racing", 1, Priority::Normal)).is_ok());
    assert_eq!(attempts.recv_timeout(wait::HUNG), Ok(1));
    assert_eq!(pool.load().undelivered, 1);
    assert!(pool.cancel(&key("racing")));
    assert_eq!(
        pool.load().undelivered,
        0,
        "cancel withdrew the queued partial"
    );
    assert_eq!(
        pool.load().held.total(),
        1,
        "the running read still holds its admission"
    );
    step.store(1, Ordering::Release);
    assert_eq!(attempts.recv_timeout(wait::HUNG), Ok(2));
    assert_eq!(
        pool.load().undelivered,
        0,
        "the late partial was refused under the outbox lock"
    );
    step.store(2, Ordering::Release);
    wait::until("the cancelled read finished", || pool.running() == 0);
    let batch = pool.take(ALL);
    assert_eq!(
        batch.outcomes.len(),
        1,
        "only the terminal outcome is delivered"
    );
    assert!(matches!(
        batch.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    ));
    drop(batch);
    assert_eq!(pool.load().held, Held::default());
}

/// Schedule: an older generation's outcome is undelivered when the store
/// asks for the key again. It is withdrawn at once and its admission
/// returns, so a late generation can neither land nor hold capacity.
#[test]
fn asking_for_a_key_again_withdraws_its_late_generations() {
    let (pool, _gate) = gated(1, limits(4, 2));
    assert!(pool.submit(job("fast-key", 1, Priority::Normal)).is_ok());
    wait::until("generation 1 finished", || pool.load().undelivered == 1);
    assert!(pool.submit(job("fast-key", 2, Priority::Normal)).is_ok());
    wait::until("generation 2 finished", || {
        pool.running() == 0 && pool.queued() == 0
    });
    let batch = pool.take(ALL);
    assert_eq!(
        batch
            .outcomes
            .iter()
            .map(|outcome| outcome.generation)
            .collect::<Vec<_>>(),
        [Generation::new(2)]
    );
    drop(batch);
    assert_eq!(pool.load().held, Held::default());
}

/// Schedule: a running read, queued reads, an undelivered outcome, and an
/// outcome the UI still holds when the pool shuts down. Shutdown does not
/// wait for the UI; every admission returns once its last holder drops.
#[test]
fn shutdown_returns_every_admission_without_waiting_for_held_outcomes() {
    let (pool, _gate) = gated(1, limits(8, 4));
    let ledger = Arc::clone(&pool.shared.ledger);
    assert!(pool.submit(job("fast-held", 1, Priority::Normal)).is_ok());
    assert!(
        pool.submit(job("fast-undelivered", 2, Priority::Normal))
            .is_ok()
    );
    wait::until("both finished", || pool.load().undelivered == 2);
    let held = pool.take(NonZeroUsize::MIN);
    assert!(
        pool.submit(job("slow-running", 3, Priority::Normal))
            .is_ok()
    );
    wait::until("the slow read is running", || pool.running() == 1);
    assert!(
        pool.submit(job("fast-queued", 4, Priority::Prefetch))
            .is_ok()
    );
    assert_eq!(ledger.held().total(), 4);

    let (closed, finished) = mpsc::channel();
    thread::spawn(move || {
        drop(pool);
        closed.send(()).expect("pool dropped");
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown waited on a cancelled read or the UI");
    assert_eq!(
        ledger.held().total(),
        1,
        "only the outcome the UI still holds keeps its admission"
    );
    assert_eq!(held.outcomes.len(), 1);
    drop(held);
    assert_eq!(ledger.held(), Held::default());
}

/// Schedule: one worker is busy; prefetches fill their share; reads a view
/// waits on use the reserve and then evict queued prefetches, oldest first.
/// A running prefetch is never evicted, and an evicted one never runs.
#[test]
fn reads_a_view_waits_on_evict_only_queued_prefetches() {
    let (pool, gate) = gated(1, limits(4, 2));
    assert!(pool.submit(job("slow-busy", 1, Priority::Normal)).is_ok());
    wait::until("the worker is busy", || pool.running() == 1);
    assert!(
        pool.submit(job("fast-hover-1", 2, Priority::Prefetch))
            .is_ok()
    );
    assert!(
        pool.submit(job("fast-hover-2", 3, Priority::Prefetch))
            .is_ok()
    );
    assert_eq!(
        pool.submit(job("fast-hover-3", 4, Priority::Prefetch)),
        Err(Refused::Busy(Saturation::PrefetchShare))
    );
    assert_eq!(
        pool.submit(job("fast-click-1", 5, Priority::Normal)),
        Ok(Admitted { evicted: None })
    );
    assert_eq!(
        pool.submit(job("fast-click-2", 6, Priority::Normal)),
        Ok(Admitted {
            evicted: Some(Evicted {
                key: key("fast-hover-1"),
                generation: Generation::new(2)
            })
        })
    );
    assert_eq!(
        pool.load().held,
        Held {
            normal: 3,
            prefetch: 1
        },
        "the evicted admission moved to the view's read"
    );
    assert_eq!(
        pool.submit(job("fast-click-3", 7, Priority::Normal)),
        Ok(Admitted {
            evicted: Some(Evicted {
                key: key("fast-hover-2"),
                generation: Generation::new(3)
            })
        })
    );
    assert_eq!(
        pool.submit(job("fast-click-4", 8, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
    gate.store(true, Ordering::Release);
    wait::until("every admitted read finished", || {
        pool.load().undelivered == 4
    });
    let landed = pool.take(ALL);
    let mut names = landed
        .outcomes
        .iter()
        .map(|outcome| outcome.key.to_string())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        [
            "symbol fast-click-1",
            "symbol fast-click-2",
            "symbol fast-click-3",
            "symbol slow-busy"
        ]
    );
}

/// Schedule: a prefetch finishes before a read a view waits on. The UI's
/// next turn lands the view's read first.
#[test]
fn a_landing_turn_takes_reads_a_view_waits_on_before_prefetches() {
    let (pool, _gate) = gated(1, limits(4, 2));
    assert!(
        pool.submit(job("fast-hover", 1, Priority::Prefetch))
            .is_ok()
    );
    wait::until("the prefetch finished", || pool.load().undelivered == 1);
    assert!(pool.submit(job("fast-click", 2, Priority::Normal)).is_ok());
    wait::until("the click finished", || pool.load().undelivered == 2);
    let first = pool.take(NonZeroUsize::MIN);
    assert_eq!(first.outcomes[0].key, key("fast-click"));
    assert_eq!(first.remaining, 1);
}

/// A panicking reader becomes one typed fault, and its admission returns.
#[test]
fn a_panicking_reader_is_one_fault_and_returns_its_admission() {
    struct Panics(Arc<AtomicUsize>);
    impl PageReader for Panics {
        fn read(&mut self, _: &ReadRequest, _: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
            self.0.fetch_add(1, Ordering::SeqCst);
            panic!("mapping bug");
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let reader_calls = Arc::clone(&calls);
    let pool = ReadPool::start_with(1, limits(2, 1), move |_| Panics(Arc::clone(&reader_calls)))
        .expect("pool");
    assert!(pool.submit(job("boom", 1, Priority::Normal)).is_ok());
    wait::until("the read faulted", || pool.load().undelivered == 1);
    let batch = pool.take(ALL);
    assert!(matches!(
        &batch.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Fault(_)))
    ));
    drop(batch);
    assert_eq!(pool.load().held, Held::default());
    assert!(
        pool.submit(job("boom", 2, Priority::Normal)).is_ok(),
        "the worker survived"
    );
    wait::until("the second read ran", || calls.load(Ordering::SeqCst) == 2);
}

/// Use the actual core to finish workers while the UI is withheld. Outcomes
/// have genuine typed admissions; no direct synthetic outbox insertion.
fn completed(shared: &Shared, name: &str, generation: u64, priority: Priority) {
    assert!(shared.submit(job(name, generation, priority)).is_ok());
    let read = shared.try_start(0).expect("worker took the admission");
    shared.finish(read, Ok(health(generation)));
}

#[test]
fn mixed_landing_batches_keep_foreground_fifo_and_eighth_slot_prefetch_progress() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(64, 32), wake);
    for class in [Priority::Prefetch, Priority::Normal] {
        for round in 0..14 {
            completed(&shared, &format!("{class:?}-{round}"), round, class);
        }
    }
    for turn in 0..2 {
        let batch = shared
            .outbox
            .take_for(NonZeroUsize::MIN.saturating_add(7), &BTreeSet::new());
        assert_eq!(batch.outcomes.len(), 8);
        assert!(
            batch.outcomes[..7]
                .iter()
                .all(|outcome| outcome.priority == Priority::Normal)
        );
        assert_eq!(batch.outcomes[0].key, key(&format!("Normal-{}", turn * 7)));
        assert_eq!(batch.outcomes[7].key, key(&format!("Prefetch-{turn}")));
        assert_eq!(batch.outcomes[7].priority, Priority::Prefetch);
    }
    let tail = shared.take(ALL);
    assert!(
        tail.outcomes
            .iter()
            .all(|outcome| outcome.priority == Priority::Prefetch)
    );
    drop(tail);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn current_visible_keys_overtake_completed_and_running_prefetches_without_rewriting_history() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(2, limits(64, 32), wake);
    assert!(
        shared
            .submit(job("visible-running", 77, Priority::Prefetch))
            .is_ok()
    );
    let running = shared.try_start(1).expect("running prefetch");
    for round in 0..12 {
        completed(
            &shared,
            &format!("earlier-{round}"),
            round,
            Priority::Prefetch,
        );
    }
    let selected = BTreeSet::from([key("earlier-11")]);
    let first = shared
        .outbox
        .take_for(NonZeroUsize::MIN.saturating_add(7), &selected);
    assert_eq!(first.outcomes[0].key, key("earlier-11"));
    assert_eq!(first.outcomes[0].priority, Priority::Prefetch);
    assert_eq!(first.outcomes[1].key, key("earlier-0"));
    drop(first);
    assert!(
        !shared.promote(&key("visible-running")),
        "running priority stays historical"
    );
    shared.finish(running, Ok(health(77)));
    let second = shared.outbox.take_for(
        NonZeroUsize::MIN.saturating_add(7),
        &BTreeSet::from([key("visible-running")]),
    );
    assert_eq!(second.outcomes[0].key, key("visible-running"));
    assert_eq!(second.outcomes[0].generation, Generation::new(77));
    assert_eq!(second.outcomes[0].priority, Priority::Prefetch);
    assert_eq!(second.outcomes[1].key, key("earlier-7"));
    drop(second);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn replacing_a_partial_rearms_its_wake_but_cancelled_reposts_do_not() {
    let (wake, mut receiver) = wake_channel();
    let shared = Shared::new(1, limits(2, 1), wake);
    assert!(shared.submit(job("stream", 1, Priority::Normal)).is_ok());
    let read = shared.try_start(0).expect("read");
    shared.post_partial(&read, health(1));
    assert!(receiver.try_take());
    shared.post_partial(&read, health(2));
    assert!(
        receiver.try_take(),
        "replacing a partial is still a publication"
    );
    assert_eq!(shared.load().undelivered, 1);
    assert!(shared.cancel(&key("stream")));
    shared.post_partial(&read, health(3));
    assert!(!receiver.try_take(), "a cancelled repost publishes nothing");
    assert_eq!(shared.load().undelivered, 0);
    shared.finish(read, Ok(health(4)));
    assert!(
        receiver.try_take(),
        "the cancelled terminal still wakes its consumer"
    );
    let batch = shared.take(ALL);
    assert!(matches!(
        batch.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    ));
    drop(batch);
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn supersession_cancels_before_withdrawing_and_preserves_a_reentrant_newer_read() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(2, limits(4, 2), wake));
    assert!(shared.submit(job("same", 1, Priority::Normal)).is_ok());
    let old = Arc::new(shared.try_start(0).expect("old read"));
    shared.post_partial(&old, health(1));
    let weak_shared = Arc::downgrade(&shared);
    let weak_old = Arc::downgrade(&old);
    let guard = old.job.cancel.on_cancel(move || {
        let shared = weak_shared.upgrade().expect("pool alive");
        drop(
            shared
                .queue
                .try_lock()
                .expect("cancellation callback runs outside queue"),
        );
        drop(shared.outbox.hold_for_test());
        // Try the old worker's partial exactly between token cancellation
        // and the outer submit's withdrawal. It must not retain capacity.
        let old = weak_old.upgrade().expect("worker alive");
        assert!(old.job.cancel.is_cancelled());
        shared.post_partial(&old, health(2));
        assert!(shared.submit(job("same", 3, Priority::Normal)).is_ok());
        let newer = shared.try_start(1).expect("reentrant request runs");
        shared.finish(newer, Ok(health(3)));
    });
    assert!(shared.submit(job("same", 2, Priority::Normal)).is_ok());
    let entries = shared.outbox.audit();
    assert_eq!(
        entries.len(),
        1,
        "the old partial was withdrawn; the new terminal survives"
    );
    let load = shared.load();
    assert_eq!((load.queued, load.running, load.undelivered), (0, 1, 1));
    let delivered = shared.take(ALL);
    assert_eq!(delivered.outcomes[0].generation, Generation::new(3));
    assert_eq!(rows(&delivered.outcomes[0].delivery), Some(3));
    drop(delivered);
    drop(guard);
    let old = Arc::try_unwrap(old).expect("no callback retains a running read");
    shared.finish(old, Ok(health(4)));
    let cancelled = shared.take(ALL);
    assert!(matches!(
        cancelled.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    ));
    drop(cancelled);
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn terminal_handoff_excludes_an_idle_snapshot_until_the_outcome_is_counted() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(2, 1), wake));
    assert!(shared.submit(job("handoff", 1, Priority::Normal)).is_ok());
    let read = shared.try_start(0).expect("read");
    let held_outbox = shared.outbox.hold_for_test();
    let worker = Arc::clone(&shared);
    let finishing = std::thread::spawn(move || worker.finish(read, Ok(health(1))));
    wait::until("handoff holds queue while waiting to post", || {
        shared.queue.try_lock().is_err()
    });
    let observing = Arc::clone(&shared);
    let (sent, seen) = mpsc::channel();
    let observer = std::thread::spawn(move || sent.send(observing.load()).expect("snapshot"));
    assert!(matches!(seen.try_recv(), Err(mpsc::TryRecvError::Empty)));
    drop(held_outbox);
    finishing.join().expect("worker finishes");
    let load = seen
        .recv_timeout(Duration::from_secs(2))
        .expect("snapshot resumes");
    observer.join().expect("observer finishes");
    assert_eq!((load.queued, load.running, load.undelivered), (0, 0, 1));
    assert!(!load.is_idle());
    let delivered = shared.take(ALL);
    assert!(
        shared.load().is_idle(),
        "UI-owned values are no longer pending work"
    );
    assert_eq!(
        shared.load().held.total(),
        1,
        "delivered ownership still bounds admission"
    );
    drop(delivered);
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn same_key_queued_replacement_reclasses_without_exceeding_the_prefetch_share() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(3, 1), wake);
    assert!(shared.submit(job("hover", 1, Priority::Prefetch)).is_ok());
    assert!(shared.submit(job("normal", 2, Priority::Normal)).is_ok());
    assert_eq!(
        shared.submit(job("normal", 3, Priority::Prefetch)),
        Err(Refused::Busy(Saturation::PrefetchShare))
    );
    assert_eq!(
        shared.load().held,
        Held {
            normal: 0,
            prefetch: 1
        }
    );
    assert!(shared.submit(job("hover", 4, Priority::Normal)).is_ok());
    assert_eq!(
        shared.load().held,
        Held {
            normal: 1,
            prefetch: 0
        }
    );
    assert!(shared.cancel(&key("hover")));
    assert_eq!(shared.load().held, Held::default());
}
