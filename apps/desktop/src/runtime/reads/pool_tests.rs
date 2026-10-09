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
        generation: Generation::new(generation).expect("nonzero fixture generation"),
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
            pool.submit(job(&format!("fast-{round}"), round + 1, Priority::Normal)),
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

/// Force the terminal-already-present branch with an uncancelled token:
/// a failure is a terminal payload, not permission for a stray partial to
/// replace it. All publication and admission logic is the real pool core.
#[test]
fn a_failed_terminal_rejects_stray_partials_and_holds_its_identity_until_final_drop() {
    for failure in [
        ReadFailure::Cancelled,
        ReadFailure::Fault(crate::core::ErrorValue::new(
            crate::core::FaultCode::Protocol,
            "the original terminal fault",
        )),
    ] {
        let (wake, _receiver) = wake_channel();
        let shared = Shared::new(1, limits(1, 0), wake);
        assert!(
            shared
                .submit(job("failed-terminal", 17, Priority::Normal))
                .is_ok()
        );
        let read = shared.try_start(0).expect("admitted read starts");
        let identity = read.permit.id();
        let cancel = read.job.cancel.clone();
        shared.post_partial(&read, health(1));
        let stray = ReadOutcome {
            key: read.job.key.clone(),
            generation: read.job.generation,
            worker: read.worker,
            priority: read.job.priority,
            delivery: Delivery::Partial(health(99)),
            permit: read.permit.clone(),
        };
        shared.finish(read, Err(failure.clone()));
        assert!(
            !cancel.is_cancelled(),
            "test the terminal fence, not the token fence"
        );
        assert_eq!(shared.load().undelivered, 1);
        assert_eq!(shared.load().running, 0);
        for _ in 0..3 {
            let posted = shared.outbox.post_partial(stray.clone(), &cancel);
            assert!(
                !posted.posted,
                "a later partial cannot resurrect this queued terminal"
            );
            let refused = posted.unused.expect("stray returned for an unlocked drop");
            assert_eq!(refused.permit.id(), identity);
            assert!(matches!(refused.delivery, Delivery::Partial(_)));
            drop(refused);
            assert_eq!(
                shared.load().undelivered,
                1,
                "only the original terminal remains"
            );
            assert_eq!(shared.load().held.total(), 1);
        }
        drop(stray);
        let terminal = shared.take(ALL);
        assert_eq!(terminal.outcomes.len(), 1);
        assert_eq!(terminal.remaining, 0);
        let outcome = &terminal.outcomes[0];
        assert_eq!(outcome.permit.id(), identity);
        assert_eq!(outcome.key, key("failed-terminal"));
        assert_eq!(
            outcome.generation,
            Generation::new(17).expect("nonzero fixture generation")
        );
        assert_eq!(outcome.worker, 0);
        assert_eq!(outcome.priority, Priority::Normal);
        assert_eq!(outcome.delivery, Delivery::Terminal(Err(failure)));
        let ui_clone = outcome.clone();
        drop(terminal);
        assert_eq!(
            shared.load().held.total(),
            1,
            "the UI clone retains the admission"
        );
        assert_eq!(
            shared.submit(job("blocked", 18, Priority::Normal)),
            Err(Refused::Busy(Saturation::Reads))
        );
        assert!(
            shared.take(ALL).outcomes.is_empty(),
            "no stray or duplicate terminal follows"
        );
        drop(ui_clone);
        assert_eq!(shared.load().held, Held::default());
        assert!(shared.submit(job("fresh", 19, Priority::Normal)).is_ok());
        let fresh = shared.try_start(0).expect("released capacity admits work");
        assert_ne!(fresh.permit.id(), identity);
        shared.finish(fresh, Ok(health(2)));
        drop(shared.take(ALL));
        assert!(shared.load().is_idle());
        assert_eq!(shared.load().held, Held::default());
    }
}

/// A real worker floods progress, then blocks on an explicit Condvar gate.
/// The UI holds its last partial while cancelling. The reader deliberately
/// returns success anyway: the pool must publish Cancelled and keep capacity
/// charged until the terminal and every held partial clone are dropped.
#[test]
fn a_cancelled_worker_success_keeps_its_flooded_partial_admission_until_ui_clones_drop() {
    struct FloodThenSuccess {
        flooded: mpsc::Sender<()>,
        returned_after_cancel: mpsc::Sender<bool>,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }
    impl PageReader for FloodThenSuccess {
        fn read(
            &mut self,
            request: &ReadRequest,
            context: &ReadContext<'_>,
        ) -> Result<PageValue, ReadFailure> {
            if name_of(request) != "flood-held" {
                return Ok(health(20_001));
            }
            for page in 1..=10_000 {
                context.publish(health(page));
            }
            self.flooded.send(()).expect("flood completed");
            let (lock, ready) = &*self.gate;
            let (released, timeout) = ready
                .wait_timeout_while(
                    lock.lock().expect("release predicate"),
                    wait::HUNG,
                    |released| !*released,
                )
                .expect("bounded worker gate");
            assert!(
                *released && !timeout.timed_out(),
                "the UI releases this reader after cancellation"
            );
            drop(released);
            self.returned_after_cancel
                .send(context.cancel.is_cancelled())
                .expect("late-success observation");
            Ok(health(20_000))
        }
    }
    let (flooded, flood_seen) = mpsc::channel();
    let (returned_after_cancel, returned) = mpsc::channel();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let reader_gate = Arc::clone(&gate);
    let pool = ReadPool::start_with(1, limits(1, 0), move |_| FloodThenSuccess {
        flooded: flooded.clone(),
        returned_after_cancel: returned_after_cancel.clone(),
        gate: Arc::clone(&reader_gate),
    })
    .expect("real worker pool");
    assert!(pool.submit(job("flood-held", 1, Priority::Normal)).is_ok());
    flood_seen
        .recv_timeout(wait::HUNG)
        .expect("worker published the complete flood");
    assert_eq!(
        (
            pool.load().running,
            pool.load().undelivered,
            pool.load().held.total()
        ),
        (1, 1, 1)
    );
    let held_partial = pool.take(ALL);
    assert_eq!(held_partial.outcomes.len(), 1);
    assert_eq!(rows(&held_partial.outcomes[0].delivery), Some(10_000));
    assert!(matches!(
        held_partial.outcomes[0].delivery,
        Delivery::Partial(_)
    ));
    let identity = held_partial.outcomes[0].permit.id();
    let partial_clone = held_partial.outcomes[0].clone();
    assert!(pool.cancel(&key("flood-held")));
    assert_eq!(
        pool.load().undelivered,
        0,
        "the UI already owns the only partial"
    );
    assert_eq!(pool.load().held.total(), 1);
    {
        let (lock, ready) = &*gate;
        *lock.lock().expect("release predicate") = true;
        ready.notify_all();
    }
    assert_eq!(
        returned.recv_timeout(wait::HUNG),
        Ok(true),
        "the reader knowingly returned success after cancellation"
    );
    wait::until("the cancelled worker's terminal committed", || {
        let load = pool.load();
        load.running == 0 && load.undelivered == 1
    });
    let terminal = pool.take(ALL);
    assert_eq!(terminal.outcomes.len(), 1);
    assert_eq!(terminal.outcomes[0].permit.id(), identity);
    assert_eq!(terminal.outcomes[0].key, key("flood-held"));
    assert_eq!(
        terminal.outcomes[0].generation,
        Generation::new(1).expect("nonzero fixture generation")
    );
    assert_eq!(
        terminal.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    );
    drop(terminal);
    assert!(pool.load().is_idle());
    assert_eq!(
        pool.load().held.total(),
        1,
        "completed work is still held by UI partials"
    );
    assert_eq!(
        pool.submit(job("fresh", 2, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
    drop(held_partial);
    assert_eq!(
        pool.load().held.total(),
        1,
        "the remaining partial clone owns capacity"
    );
    assert!(pool.take(ALL).outcomes.is_empty());
    drop(partial_clone);
    assert_eq!(pool.load().held, Held::default());
    assert!(pool.submit(job("fresh", 2, Priority::Normal)).is_ok());
    wait::until(
        "a fresh read progresses after the last UI clone drops",
        || {
            let load = pool.load();
            load.running == 0 && load.undelivered == 1
        },
    );
    let fresh = pool.take(ALL);
    assert_eq!(fresh.outcomes.len(), 1);
    assert_ne!(fresh.outcomes[0].permit.id(), identity);
    assert_eq!(fresh.outcomes[0].key, key("fresh"));
    assert_eq!(
        fresh.outcomes[0].generation,
        Generation::new(2).expect("nonzero fixture generation")
    );
    assert_eq!(
        fresh.outcomes[0].delivery,
        Delivery::Terminal(Ok(health(20_001)))
    );
    drop(fresh);
    assert!(pool.load().is_idle());
    assert_eq!(pool.load().held, Held::default());
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
        [Generation::new(2).expect("nonzero fixture generation")]
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
                generation: Generation::new(2).expect("nonzero fixture generation")
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
                generation: Generation::new(3).expect("nonzero fixture generation")
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
            completed(&shared, &format!("{class:?}-{round}"), round + 1, class);
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
            round + 1,
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
    assert_eq!(
        second.outcomes[0].generation,
        Generation::new(77).expect("nonzero fixture generation")
    );
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
    let (sent, seen) = mpsc::channel();
    let guard = old.job.cancel.on_cancel(move || {
        let shared = weak_shared.upgrade().expect("pool alive");
        assert_callback_locks_are_free(&shared);
        // Try the old worker's partial exactly between token cancellation
        // and the outer submit's withdrawal. It must not retain capacity.
        let old = weak_old.upgrade().expect("worker alive");
        assert!(old.job.cancel.is_cancelled());
        shared.post_partial(&old, health(2));
        assert!(shared.submit(job("same", 3, Priority::Normal)).is_ok());
        let newer = shared.try_start(1).expect("reentrant request runs");
        shared.finish(newer, Ok(health(3)));
        sent.send(())
            .expect("all reentrant callback assertions passed");
    });
    assert!(shared.submit(job("same", 2, Priority::Normal)).is_ok());
    seen.recv_timeout(Duration::from_secs(1))
        .expect("reentrant callback completed without isolated assertion panic");
    let entries = shared.outbox.audit();
    assert_eq!(
        entries.len(),
        1,
        "the old partial was withdrawn; the new terminal survives"
    );
    let load = shared.load();
    assert_eq!((load.queued, load.running, load.undelivered), (0, 1, 1));
    let delivered = shared.take(ALL);
    assert_eq!(
        delivered.outcomes[0].generation,
        Generation::new(3).expect("nonzero fixture generation")
    );
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

#[test]
fn bounded_drain_rearms_25_results_in_eight_eight_eight_one_then_wakes_after_empty() {
    let (mut pool, _) = gated(2, limits(32, 16));
    let mut receiver = pool.take_wake().expect("wake receiver");
    for round in 0..25 {
        assert!(
            pool.submit(job(&format!("burst-{round}"), round + 1, Priority::Normal))
                .is_ok()
        );
    }
    wait::until("25 terminals wait for UI, with no active worker", || {
        let load = pool.load();
        (load.queued, load.running, load.undelivered) == (0, 0, 25) && receiver.counts().0 == 25
    });
    for (size, remaining) in [(8, 17), (8, 9), (8, 1), (1, 0)] {
        assert!(receiver.try_take(), "pending results kept a wake");
        let batch = pool.drain();
        assert_eq!(batch.len(), size);
        assert_eq!(pool.load().undelivered, remaining);
        assert_eq!(pool.load().is_idle(), remaining == 0);
        drop(batch);
    }
    assert!(!receiver.try_take(), "empty outbox schedules no idle wake");
    assert_eq!(pool.load().held, Held::default());
    assert!(
        pool.submit(job("after-empty", 26, Priority::Normal))
            .is_ok()
    );
    wait::until("new post-empty terminal wakes again", || {
        receiver.try_take()
    });
    assert_eq!(pool.load().undelivered, 1);
    let batch = pool.drain();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].key, key("after-empty"));
    drop(batch);
    assert!(pool.load().is_idle());
    assert!(!receiver.try_take());
    assert_eq!(pool.load().held, Held::default());
}

/// Callback guards are all try-locks: a regression fails immediately instead
/// of blocking the test in the scheduler, outbox, or leaf admission ledger.
fn assert_callback_locks_are_free(shared: &Shared) {
    drop(
        shared
            .queue
            .try_lock()
            .expect("callback must not inherit queue lock"),
    );
    drop(
        shared
            .outbox
            .try_hold_for_test()
            .expect("callback must not inherit outbox lock"),
    );
    assert_eq!(
        shared
            .ledger
            .try_held_for_test()
            .expect("callback must not inherit ledger lock"),
        shared.load().held
    );
}

#[test]
fn cancellation_and_shutdown_callbacks_reenter_after_jobs_are_removed() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(4, 2), wake));
    let first = job("cancel-reenter", 1, Priority::Normal);
    let cancelled_token = first.cancel.clone();
    assert!(shared.submit(first).is_ok());
    let callback_pool = Arc::downgrade(&shared);
    let (sent, seen) = mpsc::channel();
    let cancel_guard = cancelled_token.on_cancel(move || {
        let shared = callback_pool.upgrade().expect("pool alive");
        assert_callback_locks_are_free(&shared);
        assert!(shared.queued_reads().is_empty());
        assert!(
            shared
                .submit(job("cancel-reenter", 2, Priority::Normal))
                .is_ok()
        );
        let read = shared.try_start(0).expect("reentrant newer read");
        shared.finish(read, Ok(health(2)));
        sent.send(()).expect("callback observation");
    });
    assert!(shared.cancel(&key("cancel-reenter")));
    seen.recv_timeout(Duration::from_secs(1))
        .expect("cancel callback completed");
    let preserved = shared.take(ALL);
    assert_eq!(
        preserved.outcomes[0].generation,
        Generation::new(2).expect("nonzero fixture generation"),
        "cancel's captured IDs cannot withdraw reentrant newer work"
    );
    drop(preserved);
    drop(cancel_guard);

    let shutting_down = job("shutdown-reenter", 3, Priority::Normal);
    let closing_token = shutting_down.cancel.clone();
    assert!(shared.submit(shutting_down).is_ok());
    let callback_pool = Arc::downgrade(&shared);
    let (sent, seen) = mpsc::channel();
    let close_guard = closing_token.on_cancel(move || {
        let shared = callback_pool.upgrade().expect("pool alive");
        assert_callback_locks_are_free(&shared);
        assert!(shared.queued_reads().is_empty());
        assert_eq!(
            shared.submit(job("forbidden-reopen", 4, Priority::Normal)),
            Err(Refused::Closed)
        );
        assert!(!shared.cancel(&key("shutdown-reenter")));
        shared.close();
        sent.send(()).expect("close observation");
    });
    shared.close();
    seen.recv_timeout(Duration::from_secs(1))
        .expect("shutdown callback completed");
    drop(close_guard);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn worker_start_rejects_unknown_or_busy_slots_without_consuming_a_queued_admission() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(3, 1), wake);
    assert!(shared.submit(job("first", 1, Priority::Normal)).is_ok());
    assert!(shared.try_start(1).is_none(), "unknown worker owns no slot");
    assert_eq!(shared.queued_reads().len(), 1);
    let first = shared.try_start(0).expect("idle worker");
    let first_id = first.permit.id();
    assert!(shared.submit(job("second", 2, Priority::Normal)).is_ok());
    assert!(
        shared.try_start(0).is_none(),
        "busy worker cannot overwrite its current read"
    );
    assert_eq!(shared.queued_reads().len(), 1);
    assert_eq!(
        shared.queue().running[0]
            .as_ref()
            .expect("first retained")
            .read,
        first_id
    );
    shared.finish(first, Ok(health(1)));
    let second = shared
        .try_start(0)
        .expect("cleared worker may take the next read");
    assert_ne!(second.permit.id(), first_id);
    shared.finish(second, Ok(health(2)));
    drop(shared.take(ALL));
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn completed_obsolete_outbox_replacement_at_capacity_retries_after_unlocked_drop() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(1, 0), wake);
    completed(&shared, "same", 1, Priority::Normal);
    assert_eq!(shared.load().held.total(), 1);
    assert_eq!(
        shared.submit(job("same", 2, Priority::Normal)),
        Ok(Admitted { evicted: None })
    );
    assert_eq!(
        shared.load().held.total(),
        1,
        "obsolete admission was returned before replacement admission"
    );
    assert_eq!(shared.load().undelivered, 0);
    let replacement = shared
        .try_start(0)
        .expect("new generation owns the only slot");
    assert_eq!(
        replacement.job.generation,
        Generation::new(2).expect("nonzero fixture generation")
    );
    shared.finish(replacement, Ok(health(2)));
    let batch = shared.take(ALL);
    assert_eq!(batch.outcomes.len(), 1);
    assert_eq!(
        batch.outcomes[0].generation,
        Generation::new(2).expect("nonzero fixture generation")
    );
    drop(batch);
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn replacement_never_reclaims_a_running_or_ui_held_admission() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(1, 0), wake);
    assert!(shared.submit(job("same", 1, Priority::Normal)).is_ok());
    let running = shared.try_start(0).expect("running read");
    shared.post_partial(&running, health(1));
    let ui = shared.take(ALL);
    let clone = ui.outcomes[0].clone();
    shared.post_partial(&running, health(2));
    assert_eq!(
        shared.submit(job("same", 2, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
    assert_eq!(shared.load().held.total(), 1);
    assert_eq!(
        shared.load().undelivered,
        0,
        "obsolete partial was disposable, running ownership was not"
    );
    shared.finish(running, Ok(health(3)));
    drop(shared.take(ALL));
    assert!(shared.load().is_idle());
    assert_eq!(
        shared.submit(job("same", 3, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads)),
        "UI-owned partial still keeps its actual read admitted"
    );
    drop(ui);
    assert_eq!(shared.load().held.total(), 1, "a clone owns the same share");
    drop(clone);
    assert_eq!(
        shared.submit(job("same", 4, Priority::Normal)),
        Ok(Admitted { evicted: None })
    );
    assert!(shared.cancel(&key("same")));
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn reentrant_newer_read_supersedes_the_waiting_second_admission_phase() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
    let original = job("same", 1, Priority::Normal);
    let original_token = original.cancel.clone();
    assert!(shared.submit(original).is_ok());
    let original = Arc::new(Mutex::new(Some(shared.try_start(0).expect("old read"))));
    shared.post_partial(
        original.lock().expect("test worker").as_ref().expect("old"),
        health(1),
    );
    let callback_pool = Arc::downgrade(&shared);
    let callback_worker = Arc::downgrade(&original);
    let (sent, seen) = mpsc::channel();
    let guard = original_token.on_cancel(move || {
        let shared = callback_pool.upgrade().expect("pool alive");
        assert_callback_locks_are_free(&shared);
        assert_eq!(
            shared.load().queued,
            1,
            "admission cleanup is visible as queued work"
        );
        let old = callback_worker
            .upgrade()
            .expect("worker alive")
            .lock()
            .expect("test worker")
            .take()
            .expect("worker consumes old read");
        shared.finish(old, Ok(health(1)));
        assert_eq!(
            shared.submit(job("same", 3, Priority::Normal)),
            Ok(Admitted { evicted: None })
        );
        let newer = shared.try_start(0).expect("newer owns the sole admission");
        shared.finish(newer, Ok(health(3)));
        sent.send(())
            .expect("all second-admission callback assertions passed");
    });
    assert_eq!(
        shared.submit(job("same", 2, Priority::Normal)),
        Err(Refused::Superseded)
    );
    seen.recv_timeout(Duration::from_secs(1))
        .expect("second-admission callback completed without isolated assertion panic");
    drop(guard);
    let batch = shared.take(ALL);
    assert_eq!(batch.outcomes.len(), 1);
    assert_eq!(
        batch.outcomes[0].generation,
        Generation::new(3).expect("nonzero fixture generation")
    );
    assert_eq!(rows(&batch.outcomes[0].delivery), Some(3));
    drop(batch);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn obsolete_outbox_cleanup_precedes_eviction_of_another_keys_useful_prefetch() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(2, 1), wake);
    completed(&shared, "same", 1, Priority::Normal);
    assert!(shared.submit(job("hover", 2, Priority::Prefetch)).is_ok());
    assert_eq!(shared.load().held.total(), 2);
    assert_eq!(
        shared.submit(job("same", 3, Priority::Normal)),
        Ok(Admitted { evicted: None })
    );
    assert_eq!(
        shared.queued_reads(),
        vec![
            (key("hover"), Priority::Prefetch),
            (key("same"), Priority::Normal)
        ]
    );
    let normal = shared.try_start(0).expect("replacement normal read");
    assert_eq!(normal.job.key, key("same"));
    shared.finish(normal, Ok(health(3)));
    let prefetch = shared.try_start(0).expect("useful prefetch was preserved");
    assert_eq!(prefetch.job.key, key("hover"));
    shared.finish(prefetch, Ok(health(2)));
    drop(shared.take(ALL));
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn cancel_or_close_during_admission_cleanup_cannot_reopen_the_old_request() {
    for closing in [false, true] {
        let (wake, _receiver) = wake_channel();
        let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
        let old = job("same", 1, Priority::Normal);
        let token = old.cancel.clone();
        assert!(shared.submit(old).is_ok());
        let running = shared.try_start(0).expect("old running read");
        shared.post_partial(&running, health(1));
        let callback_pool = Arc::downgrade(&shared);
        let (sent, seen) = mpsc::channel();
        let guard = token.on_cancel(move || {
            let shared = callback_pool.upgrade().expect("pool alive");
            assert_callback_locks_are_free(&shared);
            assert_eq!(
                shared.load().queued,
                1,
                "the cleanup ticket is registered before callbacks"
            );
            if closing {
                shared.close();
            } else {
                assert!(shared.cancel(&key("same")));
            }
            assert_eq!(shared.load().queued, 0, "revocation removed the ticket");
            sent.send(())
                .expect("all revocation callback assertions passed");
        });
        assert_eq!(
            shared.submit(job("same", 2, Priority::Normal)),
            Err(if closing {
                Refused::Closed
            } else {
                Refused::Superseded
            })
        );
        seen.recv_timeout(Duration::from_secs(1))
            .expect("revocation callback completed without isolated assertion panic");
        drop(guard);
        assert!(shared.queued_reads().is_empty());
        shared.finish(running, Ok(health(2)));
        drop(shared.take(ALL));
        assert!(shared.load().is_idle());
        assert_eq!(shared.load().held, Held::default());
    }
}

#[test]
fn isolated_callback_failure_cannot_strand_an_admission_cleanup_ticket() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
    let old = job("same", 1, Priority::Normal);
    let token = old.cancel.clone();
    assert!(shared.submit(old).is_ok());
    let running = shared.try_start(0).expect("running read");
    shared.post_partial(&running, health(1));
    let callback_pool = Arc::downgrade(&shared);
    let (sent, seen) = mpsc::channel();
    let guard = token.on_cancel(move || {
        let shared = callback_pool.upgrade().expect("pool alive");
        assert_callback_locks_are_free(&shared);
        assert_eq!(shared.load().queued, 1);
        sent.send(()).expect("pre-panic schedule observation");
        panic!("deliberate callback unwind");
    });
    let (sent, later_seen) = mpsc::channel();
    let later = token.on_cancel(move || {
        sent.send(()).expect("later wait must still wake");
    });
    assert_eq!(
        shared.submit(job("same", 2, Priority::Normal)),
        Err(Refused::Busy(Saturation::Reads))
    );
    seen.recv_timeout(Duration::from_secs(1))
        .expect("callback assertions reached deliberate panic");
    later_seen
        .recv_timeout(Duration::from_secs(1))
        .expect("callback isolation attempts later wait");
    drop((guard, later));
    let load = shared.load();
    assert_eq!((load.queued, load.running, load.undelivered), (0, 1, 0));
    assert_eq!(
        load.held.total(),
        1,
        "the actual old read still owns its admission"
    );
    shared.finish(running, Ok(health(2)));
    drop(shared.take(ALL));
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn replacement_after_callback_failure_wakes_an_actually_parked_worker() {
    let (pool, _gate) = gated(1, limits(1, 0));
    let mut old = job("old", 1, Priority::Normal);
    old.affinity = Some(usize::MAX); // No worker can consume the old generation.
    let token = old.cancel.clone();
    assert!(pool.submit(old).is_ok());
    wait::until(
        "worker entered its CV wait with only the old affinity job",
        || {
            let queue = pool.shared.queue();
            queue.waiting.contains(&0) && queue.jobs.len() == 1
        },
    );
    let (sent, seen) = mpsc::channel();
    let first = token.on_cancel(move || {
        sent.send(()).expect("old notification");
        panic!("replacement callback failure");
    });
    let mut replacement = job("old", 2, Priority::Normal);
    replacement.affinity = Some(0);
    assert_eq!(pool.submit(replacement), Ok(Admitted { evicted: None }));
    seen.recv_timeout(Duration::from_secs(1))
        .expect("deliberate callback reached");
    wait::until(
        "parked worker consumed replacement without another submission",
        || pool.load().undelivered == 1,
    );
    let delivered = pool.take(ALL);
    assert_eq!(delivered.outcomes.len(), 1);
    assert_eq!(
        delivered.outcomes[0].generation,
        Generation::new(2).expect("nonzero fixture generation")
    );
    assert_eq!(rows(&delivered.outcomes[0].delivery), Some(1));
    drop((delivered, first));
    assert_eq!(pool.load().held, Held::default());
}

struct InterruptibleReader {
    worker: usize,
    gate: Arc<(Mutex<bool>, Condvar)>,
    shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>>,
    began: mpsc::Sender<usize>,
    destroyed: mpsc::Sender<usize>,
    interrupted: mpsc::Sender<(usize, bool)>,
}

impl Drop for InterruptibleReader {
    fn drop(&mut self) {
        let _ = self.destroyed.send(self.worker);
    }
}

impl PageReader for InterruptibleReader {
    fn read(
        &mut self,
        _request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        let failing = context
            .cancel
            .on_cancel(|| panic!("first blocking-reader interrupt fails"));
        let gate = Arc::clone(&self.gate);
        let shared = Arc::clone(&self.shared);
        let interrupt = context.cancel.on_cancel(move || {
            let pool = shared
                .lock()
                .expect("test pool reference")
                .as_ref()
                .expect("installed pool")
                .upgrade()
                .expect("pool alive");
            assert_callback_locks_are_free(&pool);
            let (lock, ready) = &*gate;
            *lock.lock().expect("reader wait predicate") = true;
            ready.notify_all();
        });
        self.began
            .send(self.worker)
            .expect("registered both interrupts");
        let (lock, ready) = &*self.gate;
        let (released, timeout) = ready
            .wait_timeout_while(
                lock.lock().expect("reader wait predicate"),
                wait::HUNG,
                |released| !*released,
            )
            .expect("blocking wait");
        let interrupted = *released && !timeout.timed_out() && context.cancel.is_cancelled();
        drop((released, failing, interrupt));
        self.interrupted
            .send((self.worker, interrupted))
            .expect("actual wait interruption observation");
        if interrupted {
            Err(ReadFailure::Cancelled)
        } else {
            Ok(health(99))
        }
    }
}

#[test]
fn shutdown_isolates_all_callback_failures_interrupts_readers_and_joins_workers() {
    let (began, observed) = mpsc::channel();
    let (destroyed, dropped) = mpsc::channel();
    let (interrupted, interrupt_seen) = mpsc::channel();
    let shared_slot = Arc::new(Mutex::new(None));
    let gates = (0..3)
        .map(|_| Arc::new((Mutex::new(false), Condvar::new())))
        .collect::<Vec<_>>();
    let pool = ReadPool::start_with(3, limits(3, 0), |worker| InterruptibleReader {
        worker,
        gate: Arc::clone(&gates[worker]),
        shared: Arc::clone(&shared_slot),
        began: began.clone(),
        destroyed: destroyed.clone(),
        interrupted: interrupted.clone(),
    })
    .expect("three real workers");
    *shared_slot.lock().expect("test reference") = Some(Arc::downgrade(&pool.shared));
    for worker in 0..2 {
        let mut read = job(&format!("blocked-{worker}"), 1, Priority::Normal);
        read.affinity = Some(worker);
        assert!(pool.submit(read).is_ok());
    }
    let mut started = vec![
        observed
            .recv_timeout(wait::HUNG)
            .expect("first blocking reader"),
        observed
            .recv_timeout(wait::HUNG)
            .expect("second blocking reader"),
    ];
    started.sort_unstable();
    assert_eq!(started, vec![0, 1]);
    wait::until("third worker atomically parked before close", || {
        pool.shared.queue().waiting.contains(&2)
    });
    let shared = Arc::clone(&pool.shared);
    let (sent, done) = mpsc::channel();
    let closing = thread::spawn(move || {
        drop(pool);
        sent.send(()).expect("Drop completed after joins");
    });
    let completed = done.recv_timeout(wait::HUNG);
    // Failure cleanup opens only owned test gates, allowing blocked readers
    // to finish even when the regression under test skipped their interrupt.
    if completed.is_err() {
        for gate in &gates {
            let (lock, ready) = &**gate;
            *lock.lock().expect("cleanup gate") = true;
            ready.notify_all();
        }
        shared.ready.notify_all();
    }
    assert!(
        completed.is_ok(),
        "Drop must return after interrupting all readers and waking the idle worker"
    );
    closing.join().expect("pool destructor thread");
    let mut ended = (0..3)
        .map(|_| {
            dropped
                .recv_timeout(Duration::from_secs(1))
                .expect("reader destroyed before Drop returned")
        })
        .collect::<Vec<_>>();
    ended.sort_unstable();
    assert_eq!(ended, vec![0, 1, 2]);
    let mut interrupts = (0..2)
        .map(|_| {
            interrupt_seen
                .recv_timeout(Duration::from_secs(1))
                .expect("blocking wait outcome")
        })
        .collect::<Vec<_>>();
    interrupts.sort_unstable();
    assert_eq!(
        interrupts,
        vec![(0, true), (1, true)],
        "timeout or isolated lock assertion cannot masquerade as a real interrupt"
    );
    let load = shared.load();
    assert_eq!((load.queued, load.running, load.undelivered), (0, 0, 2));
    assert!(
        !load.is_idle(),
        "joined workers still have undelivered terminal results"
    );
    let terminal = shared.take(ALL);
    assert_eq!(terminal.outcomes.len(), 2);
    assert!(terminal.outcomes.iter().all(|outcome| matches!(
        outcome.delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    )));
    drop(terminal);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn another_unwind_removes_only_its_exact_preparing_ticket() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(2, 0), wake);
    let retired = CancellationToken::new();
    let newer = CancellationToken::new();
    {
        let mut queue = shared.queue();
        queue.preparing.push(Preparing {
            key: key("same"),
            generation: job("same", 1, Priority::Normal).generation,
            cancel: retired.clone(),
        });
        queue.preparing.push(Preparing {
            key: key("same"),
            generation: job("same", 2, Priority::Normal).generation,
            cancel: newer.clone(),
        });
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _cleanup = PreparingCleanup {
            shared: &shared,
            token: &retired,
            active: true,
        };
        panic!("non-callback cleanup unwind");
    }));
    assert!(
        result.is_err(),
        "only registered callback failures are isolated"
    );
    let queue = shared.queue();
    assert_eq!(queue.preparing.len(), 1);
    assert!(std::ptr::eq(
        queue.preparing.first().expect("newer ticket").cancel.flag(),
        newer.flag()
    ));
    drop(queue);
    shared.close();
    assert!(newer.is_cancelled());
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn readiness_guard_wakes_a_parked_worker_during_non_callback_unwind() {
    let (pool, _gate) = gated(1, limits(1, 0));
    wait::until("worker atomically parked before committed mutation", || {
        pool.shared.queue().waiting.contains(&0)
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Force the committed-mutation/cleanup-unwind cut without calling
        // submit (which would already notify). This uses the same typed
        // queued admission and borrowed guard as submit's committed phase.
        let permit = pool
            .shared
            .ledger
            .admit(Priority::Normal)
            .expect("admission");
        pool.shared.queue().jobs.push_back(QueuedRead {
            job: job("guard-wake", 3, Priority::Normal),
            permit,
        });
        let _notify = ReadyNotify {
            ready: &pool.shared.ready,
            armed: true,
        };
        panic!("cleanup after committed admission");
    }));
    assert!(
        result.is_err(),
        "readiness notification does not isolate unrelated panics"
    );
    wait::until(
        "borrowed guard woke worker without another mutation",
        || pool.load().undelivered == 1,
    );
    let delivered = pool.take(ALL);
    assert_eq!(delivered.outcomes.len(), 1);
    assert_eq!(
        delivered.outcomes[0].generation,
        Generation::new(3).expect("nonzero fixture generation")
    );
    assert_eq!(rows(&delivered.outcomes[0].delivery), Some(1));
    drop(delivered);
    assert!(pool.load().is_idle());
    assert_eq!(pool.load().held, Held::default());
}

#[test]
fn identity_exhaustion_cannot_become_an_obsolete_outbox_capacity_retry() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(1, 0), wake);
    shared.ledger.set_next_for_test(std::num::NonZeroU64::MAX);
    assert!(shared.submit(job("same", 1, Priority::Normal)).is_ok());
    let running = shared.try_start(0).expect("final admitted read");
    assert_eq!(running.permit.id().get(), u64::MAX);
    shared.finish(running, Ok(health(1)));
    assert_eq!(shared.load().undelivered, 1);
    assert_eq!(
        shared.submit(job("same", 2, Priority::Normal)),
        Err(Refused::IdentityExhausted)
    );
    // The captured obsolete terminal drops, but freeing its capacity cannot
    // revive the permanent identity refusal or leave a preparing retry ticket.
    assert_eq!(shared.load().queued, 0);
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
    assert_eq!(
        shared.submit(job("other", 3, Priority::Normal)),
        Err(Refused::IdentityExhausted)
    );
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn queued_transfers_keep_a_never_started_identity_after_minting_exhausts() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(2, 1), wake);
    shared.ledger.set_next_for_test(std::num::NonZeroU64::MAX);
    let original = job("hover", 1, Priority::Prefetch);
    let old_token = original.cancel.clone();
    assert!(shared.submit(original).is_ok());
    let identity = shared
        .queue()
        .jobs
        .front()
        .expect("queued hover")
        .permit
        .id();
    let replacement = job("hover", 2, Priority::Prefetch);
    let replacement_token = replacement.cancel.clone();
    assert!(shared.submit(replacement).is_ok());
    assert!(old_token.is_cancelled());
    assert_eq!(
        shared
            .queue()
            .jobs
            .front()
            .expect("queued replacement")
            .permit
            .id(),
        identity
    );
    assert!(
        shared.outbox.ids(|_| true).is_empty(),
        "never-started identities cannot be stale captured outbox IDs"
    );
    assert_eq!(
        shared.submit(job("view", 3, Priority::Normal)),
        Ok(Admitted {
            evicted: Some(Evicted {
                key: key("hover"),
                generation: Generation::new(2).expect("nonzero fixture generation")
            })
        })
    );
    assert!(replacement_token.is_cancelled());
    assert_eq!(
        shared.load().held,
        Held {
            normal: 1,
            prefetch: 0
        }
    );
    let running = shared
        .try_start(0)
        .expect("transferred queued admission starts once");
    assert_eq!(running.permit.id(), identity);
    shared.finish(running, Ok(health(3)));
    let batch = shared.take(ALL);
    assert_eq!(batch.outcomes.len(), 1);
    assert_eq!(
        batch.outcomes[0].generation,
        Generation::new(3).expect("nonzero fixture generation")
    );
    assert_eq!(batch.outcomes[0].permit.id(), identity);
    let ui_copy = batch.outcomes[0].clone();
    drop(batch);
    assert_eq!(shared.load().held.total(), 1);
    drop(ui_copy);
    assert_eq!(shared.load().held, Held::default());
    assert!(shared.load().is_idle());
    assert_eq!(
        shared.submit(job("new", 4, Priority::Normal)),
        Err(Refused::IdentityExhausted)
    );
}

#[test]
fn distinct_stale_started_ids_cannot_withdraw_the_final_replacement_identity() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(1, 0), wake);
    shared
        .ledger
        .set_next_for_test(std::num::NonZeroU64::new(u64::MAX - 1).expect("penultimate identity"));
    assert!(shared.submit(job("same", 1, Priority::Normal)).is_ok());
    let old = shared.try_start(0).expect("old read");
    let old_id = old.permit.id();
    shared.finish(old, Ok(health(1)));
    let captured = shared.outbox.ids(|_| true);
    assert!(captured.contains(&old_id));
    drop(shared.take(ALL));
    assert!(shared.submit(job("same", 2, Priority::Normal)).is_ok());
    let new = shared.try_start(0).expect("last fresh read");
    let new_id = new.permit.id();
    assert_ne!(old_id, new_id);
    assert_eq!(old_id.get(), u64::MAX - 1);
    assert_eq!(new_id.get(), u64::MAX);
    shared.finish(new, Ok(health(2)));
    let removed = shared
        .outbox
        .withdraw(|outcome| captured.contains(&outcome.permit.id()));
    assert!(
        removed.is_empty(),
        "a stale cleanup snapshot cannot target the new identity"
    );
    let batch = shared.take(ALL);
    assert_eq!(batch.outcomes.len(), 1);
    assert_eq!(
        batch.outcomes[0].generation,
        Generation::new(2).expect("nonzero fixture generation")
    );
    assert_eq!(rows(&batch.outcomes[0].delivery), Some(2));
    drop(batch);
    assert_eq!(shared.load().held, Held::default());
    assert_eq!(
        shared.submit(job("same", 3, Priority::Normal)),
        Err(Refused::IdentityExhausted)
    );
    assert!(shared.load().is_idle());
}

#[test]
fn cancelled_and_replaced_successes_parked_before_terminal_queue_admission_are_cancelled() {
    for replace in [false, true] {
        let (wake, _receiver) = wake_channel();
        let shared = Arc::new(Shared::new(1, limits(2, 0), wake));
        let old_job = job("finish-boundary", 1, Priority::Normal);
        let old_token = old_job.cancel.clone();
        assert!(shared.submit(old_job).is_ok());
        let old = shared.try_start(0).expect("exact admitted running read");
        let old_id = old.permit.id();
        let (parked, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let finishing = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            let success = Ok(health(41));
            let useful = success.clone();
            finishing.finish_impl(old, success, move || {
                parked
                    .send(useful)
                    .expect("constructed success before queue");
                released
                    .recv_timeout(wait::HUNG)
                    .expect("bounded finish release");
            });
            done.send(()).expect("finish returned");
        });
        assert_eq!(
            observed.recv_timeout(wait::HUNG).expect("finish parked"),
            Ok(health(41))
        );
        assert_eq!(shared.load().running, 1);
        assert_eq!(shared.load().undelivered, 0);
        let newer = job("finish-boundary", 2, Priority::Normal);
        let newer_token = newer.cancel.clone();
        if replace {
            assert!(shared.submit(newer).is_ok());
        } else {
            assert!(shared.cancel(&key("finish-boundary")));
        }
        assert!(old_token.is_cancelled());
        assert!(
            shared.outbox.audit().is_empty(),
            "cancellation withdrew before the delayed finish posts"
        );
        release.send(()).expect("release finish");
        completed
            .recv_timeout(wait::HUNG)
            .expect("bounded terminal completion");
        thread.join().expect("finish thread");
        let old_terminal = shared.take(ALL);
        assert_eq!(old_terminal.outcomes.len(), 1);
        assert_eq!(old_terminal.outcomes[0].permit.id(), old_id);
        assert_eq!(
            old_terminal.outcomes[0].delivery,
            Delivery::Terminal(Err(ReadFailure::Cancelled)),
            "cached success cannot be published after cancel returned"
        );
        drop(old_terminal);
        if replace {
            let replacement = shared
                .try_start(0)
                .expect("new identity runs after old finish");
            assert_ne!(replacement.permit.id(), old_id);
            assert_eq!(
                replacement.job.generation,
                Generation::new(2).expect("nonzero")
            );
            assert!(!newer_token.is_cancelled());
            shared.finish(replacement, Ok(health(42)));
            let terminal = shared.take(ALL);
            assert_eq!(terminal.outcomes.len(), 1);
            assert_eq!(
                terminal.outcomes[0].delivery,
                Delivery::Terminal(Ok(health(42)))
            );
            drop(terminal);
        }
        assert_eq!(shared.load().held, Held::default());
        assert!(shared.load().is_idle());
    }
}

#[test]
fn exact_running_revocation_precedes_interrupt_callbacks_and_terminal_admission() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
    let request = job("unnotified-finish", 1, Priority::Normal);
    let token = request.cancel.clone();
    assert!(shared.submit(request).is_ok());
    let read = shared.try_start(0).expect("running identity");
    let id = read.permit.id();
    let (parked, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let finishing = Arc::clone(&shared);
    let thread = std::thread::spawn(move || {
        let success = Ok(health(53));
        let useful = success.clone();
        finishing.finish_impl(read, success, move || {
            parked.send(useful).expect("constructed success");
            released.recv_timeout(wait::HUNG).expect("bounded release");
        });
        done.send(()).expect("finish returned");
    });
    assert_eq!(
        observed.recv_timeout(wait::HUNG).expect("finish parked"),
        Ok(health(53))
    );
    // Force the existing cancellation split: its exact queue mutation has
    // committed, but outside-lock token notification has not run yet. A
    // token-only recheck is intentionally insufficient for this oracle.
    let revoked = shared.queue().revoke_running(|read| read.read == id);
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].0, id);
    assert!(!token.is_cancelled());
    release
        .send(())
        .expect("release finish before interrupt callback");
    completed.recv_timeout(wait::HUNG).expect("bounded finish");
    thread.join().expect("finish thread");
    let terminal = shared.take(ALL);
    assert_eq!(terminal.outcomes.len(), 1);
    assert_eq!(terminal.outcomes[0].permit.id(), id);
    assert_eq!(
        terminal.outcomes[0].delivery,
        Delivery::Terminal(Err(ReadFailure::Cancelled))
    );
    assert!(
        !token.is_cancelled(),
        "revocation, not an atomic-token coincidence, decided the terminal"
    );
    for (_, cancelled) in revoked {
        cancelled.cancel();
    }
    assert!(token.is_cancelled());
    drop(terminal);
    assert_eq!(shared.load().held, Held::default());
    assert!(shared.load().is_idle());
}


#[test]
fn preparation_expiry_preserves_success_and_real_fault_when_terminal_handoff_wins() {
    for fault in [false, true] {
        let (wake, _receiver) = wake_channel();
        let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
        let request = job("preparation-terminal", 1, Priority::Normal);
        let generation = request.generation;
        let token = request.cancel.clone();
        assert!(shared.submit(request).is_ok());
        let Some(read) = shared.try_start(0) else { panic!("the exact read must start"); };
        let result = if fault { Err(ReadFailure::Fault(ErrorValue::new(FaultCode::Protocol, "actual preparation failed"))) }
            else { Ok(health(67)) };
        let expected = Delivery::Terminal(result.clone());
        let held_outbox = shared.outbox.hold_for_test();
        let finishing = shared.clone();
        let worker = std::thread::spawn(move || finishing.finish(read, result));
        wait::until("terminal handoff owns queue while blocked on outbox", || shared.queue.try_lock().is_err());
        let expiring = shared.clone();
        let (started, observed) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let expiry = std::thread::spawn(move || {
            assert!(started.send(()).is_ok());
            assert!(done.send(expiring.expire_preparation(&key("preparation-terminal"), generation)).is_ok());
        });
        assert_eq!(observed.recv_timeout(wait::HUNG), Ok(()));
        assert!(matches!(completed.try_recv(), Err(mpsc::TryRecvError::Empty)), "expiry cannot split terminal handoff's queue ownership");
        drop(held_outbox);
        assert!(worker.join().is_ok(), "terminal publisher must retire");
        assert_eq!(completed.recv_timeout(wait::HUNG), Ok(PreparationExpiry::TerminalReady), "the posted terminal owns ordinary landing precedence");
        assert!(expiry.join().is_ok(), "expiry observer must retire");
        assert!(!token.is_cancelled(), "already posted terminal is not withdrawn or newly cancelled");
        token.cancel(); // the independent background deadline can fire later
        assert_eq!(shared.expire_preparation(&key("preparation-terminal"), generation), PreparationExpiry::TerminalReady,
            "a later deadline-token flag cannot reclassify an already posted terminal");
        let batch = shared.take(ALL);
        assert_eq!(batch.outcomes.len(), 1);
        assert_eq!(batch.outcomes[0].delivery, expected);
        drop(batch);
        assert_eq!(shared.load().held, Held::default());
    }
}

#[test]
fn preparation_expiry_before_terminal_handoff_cancels_only_the_expired_generation() {
    for fault in [false, true] {
        let (wake, _receiver) = wake_channel();
        let shared = Arc::new(Shared::new(2, limits(3, 0), wake));
        let request = job("preparation-late", 1, Priority::Normal);
        let generation = request.generation;
        let old_token = request.cancel.clone();
        assert!(shared.submit(request).is_ok());
        let Some(read) = shared.try_start(0) else { panic!("expired read must start"); };
        let result = if fault { Err(ReadFailure::Fault(ErrorValue::new(FaultCode::Protocol, "late real fault"))) }
            else { Ok(health(68)) };
        let (parked, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let finishing = shared.clone();
        let worker = std::thread::spawn(move || finishing.finish_impl(read, result, move || {
            assert!(parked.send(()).is_ok());
            assert_eq!(released.recv_timeout(wait::HUNG), Ok(()));
        }));
        assert_eq!(observed.recv_timeout(wait::HUNG), Ok(()));
        assert_eq!(shared.expire_preparation(&key("preparation-late"), generation), PreparationExpiry::Expired);
        assert!(old_token.is_cancelled());
        let successor = job("preparation-late", 2, Priority::Normal);
        let newer_generation = successor.generation;
        let newer_token = successor.cancel.clone();
        assert!(shared.submit(successor).is_ok());
        assert_eq!(shared.expire_preparation(&key("preparation-late"), generation), PreparationExpiry::Expired, "old expiry cannot cancel a queued successor");
        assert!(!newer_token.is_cancelled());
        let Some(newer) = shared.try_start(1) else { panic!("the successor must start independently"); };
        shared.finish(newer, Ok(health(69)));
        assert_eq!(shared.expire_preparation(&key("preparation-late"), generation), PreparationExpiry::Expired, "old expiry cannot withdraw the successor's terminal");
        assert!(!newer_token.is_cancelled());
        assert!(release.send(()).is_ok());
        assert!(worker.join().is_ok(), "delayed terminal publisher must retire");
        let batch = shared.take(ALL);
        assert_eq!(batch.outcomes.len(), 2);
        assert!(batch.outcomes.iter().any(|outcome| outcome.generation == generation
            && outcome.delivery == Delivery::Terminal(Err(ReadFailure::Cancelled))), "expiry won exact queue admission before the delayed terminal");
        assert!(batch.outcomes.iter().any(|outcome| outcome.generation == newer_generation
            && outcome.delivery == Delivery::Terminal(Ok(health(69)))), "successor's result remains independently admissible");
        drop(batch);
        assert_eq!(shared.load().held, Held::default());
    }
}

#[test]
fn preparation_expiry_withdraws_only_partials_and_runs_interrupt_callbacks_unlocked() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(2, limits(3, 0), wake));
    let request = job("preparation-partial", 1, Priority::Normal);
    let generation = request.generation;
    let token = request.cancel.clone();
    assert!(shared.submit(request).is_ok());
    let Some(read) = shared.try_start(0) else { panic!("partial read must start"); };
    shared.post_partial(&read, health(70));
    assert_eq!(shared.load().undelivered, 1);
    let callback_pool = shared.clone();
    let guard = token.on_cancel(move || {
        assert_callback_locks_are_free(&callback_pool);
        assert!(callback_pool.submit(job("preparation-partial", 2, Priority::Normal)).is_ok());
        let Some(newer) = callback_pool.try_start(1) else { panic!("callback successor must run"); };
        callback_pool.finish(newer, Ok(health(71)));
    });
    assert_eq!(shared.expire_preparation(&key("preparation-partial"), generation), PreparationExpiry::Expired);
    assert!(token.is_cancelled());
    assert!(shared.outbox.audit().iter().all(|outcome| !outcome.partial), "exact partial was withdrawn after cancellation");
    shared.finish(read, Ok(health(72)));
    let batch = shared.take(ALL);
    assert_eq!(batch.outcomes.len(), 2);
    assert!(batch.outcomes.iter().any(|outcome| rows(&outcome.delivery) == Some(71)), "unlocked callback's same-key successor survives cleanup");
    assert!(batch.outcomes.iter().any(|outcome| outcome.generation == generation
        && outcome.delivery == Delivery::Terminal(Err(ReadFailure::Cancelled))));
    drop((batch, guard));
    assert_eq!(shared.load().held, Held::default());
}


#[test]
fn preparation_expiry_fences_the_actual_second_admission_phase_before_it_can_enqueue() {
    let (wake, _receiver) = wake_channel();
    let shared = Arc::new(Shared::new(1, limits(1, 0), wake));
    let original = job("preparation-ticket", 1, Priority::Normal);
    let original_token = original.cancel.clone();
    assert!(shared.submit(original).is_ok());
    let Some(read) = shared.try_start(0) else { panic!("original read must start"); };
    shared.post_partial(&read, health(73));
    let original = Arc::new(Mutex::new(Some(read)));
    let waiting = job("preparation-ticket", 2, Priority::Normal);
    let waiting_generation = waiting.generation;
    let waiting_token = waiting.cancel.clone();
    let callback_pool = Arc::downgrade(&shared);
    let callback_worker = Arc::downgrade(&original);
    let (done, completed) = mpsc::channel();
    let guard = original_token.on_cancel(move || {
        let Some(shared) = callback_pool.upgrade() else { panic!("pool must remain alive"); };
        assert_callback_locks_are_free(&shared);
        assert!(shared.queue().preparing.iter().any(|ticket|
            ticket.key == key("preparation-ticket") && ticket.generation == waiting_generation));
        assert_eq!(shared.expire_preparation(&key("preparation-ticket"), waiting_generation), PreparationExpiry::Expired);
        let Some(original) = callback_worker.upgrade() else { panic!("worker must remain alive"); };
        let Some(read) = original.lock().unwrap_or_else(PoisonError::into_inner).take() else { panic!("the old worker still owns its read"); };
        shared.finish(read, Ok(health(74)));
        assert!(done.send(()).is_ok(), "callback assertions must reach the caller");
    });
    assert_eq!(shared.submit(waiting), Err(Refused::Superseded), "expired ticket cannot reacquire returned capacity in phase two");
    assert_eq!(completed.recv_timeout(wait::HUNG), Ok(()));
    assert!(waiting_token.is_cancelled());
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
    let newer = job("preparation-ticket", 3, Priority::Normal);
    let newer_token = newer.cancel.clone();
    assert!(shared.submit(newer).is_ok(), "a new generation remains admissible");
    assert!(!newer_token.is_cancelled());
    assert!(shared.cancel(&key("preparation-ticket")));
    drop(guard);
    assert_eq!(shared.load().held, Held::default());
}

#[test]
fn preparation_expiry_removes_only_the_exact_ticket_without_relying_on_a_background_token() {
    let (wake, _receiver) = wake_channel();
    let shared = Shared::new(1, limits(2, 0), wake);
    let expired = job("preparation-exact-ticket", 1, Priority::Normal);
    let newer = job("preparation-exact-ticket", 2, Priority::Normal);
    {
        let mut queue = shared.queue();
        queue.preparing.push(Preparing { key: expired.key.clone(), generation: expired.generation, cancel: expired.cancel.clone() });
        queue.preparing.push(Preparing { key: newer.key.clone(), generation: newer.generation, cancel: newer.cancel.clone() });
    }
    assert!(!expired.cancel.is_cancelled());
    assert!(!newer.cancel.is_cancelled());
    assert_eq!(shared.expire_preparation(&expired.key, expired.generation), PreparationExpiry::Expired);
    assert!(expired.cancel.is_cancelled());
    assert!(!newer.cancel.is_cancelled(), "old expiry cannot touch the newer same-key ticket");
    {
        let mut queue = shared.queue();
        assert_eq!(queue.preparing.len(), 1);
        assert!(queue.take_preparing(&expired.cancel).is_none(), "expired phase two has no ticket to consume");
        let Some(ticket) = queue.take_preparing(&newer.cancel) else { panic!("newer phase two must retain its exact ticket"); };
        assert_eq!(ticket.generation, newer.generation);
        drop(queue);
        drop(ticket);
    }
    assert!(shared.load().is_idle());
    assert_eq!(shared.load().held, Held::default());
}
