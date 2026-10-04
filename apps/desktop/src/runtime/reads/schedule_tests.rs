//! Every interleaving of a few actors over the pool's admission, outbox, and
//! landing operations, run on the pool's own types without threads.
//!
//! Each operation holds its locks for its whole effect, so one operation is
//! the unit of interleaving; enumerating every order of the actors' steps
//! covers every schedule the threads could produce at that granularity.
//! After every step, oracles that do not read the ledger's own arithmetic
//! check it against the set of live admission holders (queued jobs, running
//! reads, undelivered outcomes, and outcomes in the UI's hands).

use super::tests::{health, job, key, limits, rows};
use super::*;
use crate::runtime::reads::permit::ReadId;
use crate::runtime::wake::wake_channel;
use std::collections::BTreeSet;

/// Every order of `lengths[actor]` steps per actor that keeps each actor's
/// own steps in order.
fn interleavings(lengths: &[usize]) -> Vec<Vec<usize>> {
    fn extend(remaining: &mut [usize], prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if remaining.iter().all(|left| *left == 0) {
            out.push(prefix.clone());
            return;
        }
        for actor in 0..remaining.len() {
            if remaining[actor] > 0 {
                remaining[actor] -= 1;
                prefix.push(actor);
                extend(remaining, prefix, out);
                prefix.pop();
                remaining[actor] += 1;
            }
        }
    }
    let mut out = Vec::new();
    extend(&mut lengths.to_vec(), &mut Vec::new(), &mut out);
    out
}

#[test]
fn the_enumerator_yields_every_order_once() {
    let orders = interleavings(&[2, 1, 1]);
    assert_eq!(orders.len(), 12, "4! / 2!");
    assert_eq!(orders.iter().collect::<BTreeSet<_>>().len(), 12);
    assert!(
        orders
            .iter()
            .all(|order| order.iter().filter(|actor| **actor == 0).count() == 2)
    );
}

/// What the UI took, in the order it took it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Taken {
    read: ReadId,
    key: PageKey,
    rows: Option<u64>,
    terminal: bool,
}

/// One schedule's world: the pool core, what each worker runs, and what
/// the UI holds.
struct World {
    shared: Shared,
    workers: Vec<Option<RunningRead>>,
    hands: Vec<ReadOutcome>,
    taken: Vec<Taken>,
    cancelled: BTreeSet<PageKey>,
    schedule: Vec<usize>,
}

impl World {
    fn new(workers: usize, limits: ReadLimits) -> Self {
        let (wake, _receiver) = wake_channel();
        Self {
            shared: Shared::new(workers, limits, wake),
            workers: (0..workers).map(|_| None).collect(),
            hands: Vec::new(),
            taken: Vec::new(),
            cancelled: BTreeSet::new(),
            schedule: Vec::new(),
        }
    }

    /// Admissions alive right now: one per queued job, and one per started
    /// read that a worker, the outbox, or the UI still holds.
    fn live(&self) -> usize {
        let mut started = self
            .workers
            .iter()
            .flatten()
            .map(|read| read.permit.id())
            .collect::<BTreeSet<_>>();
        started.extend(
            self.shared
                .outbox
                .audit()
                .into_iter()
                .map(|entry| entry.read),
        );
        started.extend(self.hands.iter().map(|outcome| outcome.permit.id()));
        self.shared.queued_reads().len() + started.len()
    }

    fn check(&self) {
        let held = self.shared.ledger.held();
        let limits = self.shared.ledger.limits();
        let context = format!("schedule {:?}", self.schedule);
        assert!(
            held.total() <= limits.reads(),
            "{context}: over the read limit: {held:?}"
        );
        assert!(
            held.prefetch <= limits.prefetch(),
            "{context}: over the prefetch share: {held:?}"
        );
        assert_eq!(
            held.total(),
            self.live(),
            "{context}: ledger disagrees with the live holders"
        );
        let queued_ids = self
            .shared
            .queue()
            .jobs
            .iter()
            .map(|queued| queued.permit.id())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            queued_ids.len(),
            self.shared.queued_reads().len(),
            "{context}: queued identities must be unique"
        );
        let mut started_ids = self
            .workers
            .iter()
            .flatten()
            .map(|read| read.permit.id())
            .collect::<BTreeSet<_>>();
        started_ids.extend(self.shared.outbox.audit().iter().map(|entry| entry.read));
        started_ids.extend(self.hands.iter().map(|outcome| outcome.permit.id()));
        assert!(
            queued_ids.is_disjoint(&started_ids),
            "{context}: a queued transfer cannot reuse a started identity"
        );
        let load = self.shared.load();
        let queued = self.shared.queued_reads().len();
        let running = self.workers.iter().flatten().count();
        let waiting = self.shared.outbox.audit().len();
        assert_eq!(
            (load.queued, load.running, load.undelivered),
            (queued, running, waiting),
            "{context}: readiness lost a handoff"
        );
        assert_eq!(
            load.is_idle(),
            queued == 0 && running == 0 && waiting == 0,
            "{context}: idle must include undelivered work, but not delivered hands"
        );
        let undelivered = self.shared.outbox.audit();
        let reads = undelivered
            .iter()
            .map(|entry| entry.read)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            reads.len(),
            undelivered.len(),
            "{context}: a read has two undelivered outcomes"
        );
        for entry in &undelivered {
            assert!(
                !(entry.partial && self.cancelled.contains(&entry.key)),
                "{context}: a cancelled read's partial page is still undelivered"
            );
        }
        for (index, earlier) in self.taken.iter().enumerate() {
            for later in self
                .taken
                .iter()
                .skip(index + 1)
                .filter(|later| later.read == earlier.read)
            {
                assert!(
                    !earlier.terminal,
                    "{context}: the UI took an outcome after its read's terminal"
                );
                assert!(
                    later.rows > earlier.rows || later.terminal,
                    "{context}: an older partial landed after a newer one"
                );
            }
        }
    }

    fn start(&mut self, worker: usize) {
        if self.workers[worker].is_none() {
            self.workers[worker] = self.shared.try_start(worker);
        }
    }

    fn partial(&mut self, worker: usize, page: u64) {
        // Posted whatever the token says: the worker's own check may have
        // passed just before the UI cancelled.
        if let Some(read) = &self.workers[worker] {
            self.shared.post_partial(read, health(page));
        }
    }

    fn finish(&mut self, worker: usize, page: u64) {
        if let Some(read) = self.workers[worker].take() {
            self.shared.finish(read, Ok(health(page)));
        }
    }

    fn record(&mut self, batch: &Batch) {
        for outcome in &batch.outcomes {
            self.taken.push(Taken {
                read: outcome.permit.id(),
                key: outcome.key.clone(),
                rows: rows(&outcome.delivery),
                terminal: matches!(outcome.delivery, Delivery::Terminal(_)),
            });
        }
    }

    fn take_and_hold(&mut self, limit: NonZeroUsize) {
        let batch = self.shared.take(limit);
        self.record(&batch);
        self.hands.extend(batch.outcomes);
    }

    fn take_and_land(&mut self) {
        let batch = self.shared.take(NonZeroUsize::MAX);
        self.record(&batch);
    }

    fn cancel(&mut self, key: &PageKey) {
        let _ = self.shared.cancel(key);
        self.cancelled.insert(key.clone());
    }

    /// Lets everything finish and land, then proves every admission returned.
    fn settle(mut self) -> Vec<Taken> {
        for worker in 0..self.workers.len() {
            self.finish(worker, 0);
        }
        self.hands.clear();
        self.take_and_land();
        self.check();
        self.shared.close();
        self.take_and_land();
        let context = format!("schedule {:?}", self.schedule);
        assert_eq!(
            self.shared.ledger.held(),
            Held::default(),
            "{context}: an admission never returned"
        );
        self.taken
    }
}

/// Two reads on two workers while the UI takes slowly, holds a partial
/// page, cancels one key, and asks for a third read at the limit.
#[test]
fn partial_pages_cancellation_and_a_slow_ui_never_leak_or_exceed_capacity() {
    const WORKER_A: usize = 0;
    const WORKER_B: usize = 1;
    const UI: usize = 2;
    let orders = interleavings(&[4, 2, 5]);
    assert_eq!(orders.len(), 6_930);
    for order in orders {
        let mut world = World::new(2, limits(2, 1));
        let mut a = job("a", 1, Priority::Normal);
        a.affinity = Some(WORKER_A);
        let mut b = job("b", 2, Priority::Normal);
        b.affinity = Some(WORKER_B);
        assert!(world.shared.submit(a).is_ok() && world.shared.submit(b).is_ok());
        world.check();
        let mut steps = [0_usize; 3];
        for actor in order {
            world.schedule.push(actor);
            let step = steps[actor];
            steps[actor] += 1;
            match (actor, step) {
                (WORKER_A, 0) => world.start(WORKER_A),
                (WORKER_A, 1) => world.partial(WORKER_A, 1),
                (WORKER_A, 2) => world.partial(WORKER_A, 2),
                (WORKER_A, _) => world.finish(WORKER_A, 3),
                (WORKER_B, 0) => world.start(WORKER_B),
                (WORKER_B, _) => world.finish(WORKER_B, 10),
                (UI, 0) => world.take_and_hold(NonZeroUsize::MIN),
                (UI, 1) => world.cancel(&key("a")),
                (UI, 2) => world.take_and_land(),
                (UI, 3) => world.hands.clear(),
                (UI, _) => {
                    let live = world.live();
                    let admitted = world.shared.submit(job("c", 3, Priority::Normal));
                    let context = format!("schedule {:?}", world.schedule);
                    if live < 2 {
                        assert_eq!(
                            admitted,
                            Ok(Admitted { evicted: None }),
                            "{context}: refused with a free admission"
                        );
                    } else {
                        assert_eq!(
                            admitted,
                            Err(Refused::Busy(Saturation::Reads)),
                            "{context}: admitted past the limit"
                        );
                    }
                }
                (other, _) => panic!("no actor {other}"),
            }
            world.check();
        }
        let context = format!("schedule {:?}", world.schedule);
        let taken = world.settle();
        let terminals = |name: &str| {
            taken
                .iter()
                .filter(|taken| taken.key == key(name) && taken.terminal)
                .count()
        };
        assert_eq!(
            terminals("b"),
            1,
            "{context}: a read nobody cancelled lost its terminal outcome"
        );
        // a may be cancelled before it starts, and then worker A runs c.
        assert!(terminals("a") <= 1 && terminals("c") <= 1, "{context}");
    }
}

/// A prefetch, then two reads a view waits on, against one worker that
/// may start any of them first: a normal read takes a queued prefetch's
/// admission only while the prefetch is queued, and an evicted prefetch
/// never runs or posts an outcome.
#[test]
fn eviction_takes_only_a_queued_prefetch_in_every_order() {
    const WORKER: usize = 0;
    const UI: usize = 1;
    let orders = interleavings(&[4, 4]);
    assert_eq!(orders.len(), 70);
    for order in orders {
        let mut world = World::new(1, limits(2, 1));
        let mut evicted = false;
        let mut steps = [0_usize; 2];
        for actor in order {
            world.schedule.push(actor);
            let step = steps[actor];
            steps[actor] += 1;
            let context = format!("schedule {:?}", world.schedule);
            match (actor, step) {
                (WORKER, 0 | 2) => world.start(WORKER),
                (WORKER, _) => world.finish(WORKER, 1),
                (UI, 0) => assert!(
                    world.shared.submit(job("p", 1, Priority::Prefetch)).is_ok(),
                    "{context}"
                ),
                (UI, 3) => world.take_and_land(),
                (UI, step) => {
                    let live = world.live();
                    let queued_prefetch = world
                        .shared
                        .queued_reads()
                        .into_iter()
                        .find(|(_, priority)| *priority == Priority::Prefetch);
                    let admitted = world.shared.submit(job(
                        &format!("n{step}"),
                        1 + u64::try_from(step).expect("step"),
                        Priority::Normal,
                    ));
                    match (live < 2, queued_prefetch) {
                        (true, _) => {
                            assert_eq!(admitted, Ok(Admitted { evicted: None }), "{context}")
                        }
                        (false, Some((key, _))) => {
                            assert_eq!(
                                key,
                                PageKey::Symbol(
                                    crate::model::pages::SymbolRef::new("p").expect("symbol")
                                ),
                                "{context}"
                            );
                            assert_eq!(
                                admitted,
                                Ok(Admitted {
                                    evicted: Some(Evicted {
                                        key,
                                        generation: Generation::new(1)
                                            .expect("nonzero fixture generation")
                                    })
                                }),
                                "{context}"
                            );
                            evicted = true;
                        }
                        (false, None) => {
                            assert_eq!(admitted, Err(Refused::Busy(Saturation::Reads)), "{context}")
                        }
                    }
                }
                (other, _) => panic!("no actor {other}"),
            }
            if evicted {
                assert!(
                    world
                        .workers
                        .iter()
                        .flatten()
                        .all(|running| running.job.key != key("p")),
                    "{context}: an evicted prefetch ran"
                );
            }
            world.check();
        }
        let taken = world.settle();
        if evicted {
            assert!(
                taken.iter().all(|taken| taken.key != key("p")),
                "an evicted prefetch posted an outcome"
            );
        }
    }
}
