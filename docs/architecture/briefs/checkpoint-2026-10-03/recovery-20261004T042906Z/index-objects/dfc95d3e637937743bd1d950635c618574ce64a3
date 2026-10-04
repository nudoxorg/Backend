//! A fixed pool of read workers with bounded admission.
//!
//! Page reads (documents, neighbourhoods, outlines, dossiers, searches) run
//! on `N` worker threads, each owning its own session, so one slow read never
//! blocks the others. Index and admin work stays on the engine actor's own
//! lane. The pool never mutates the owner.
//!
//! Scheduling rules:
//! - **Bounded admission.** A read holds one [`super::permit::ReadPermit`]
//!   from admission until the last outcome carrying it is dropped by the UI,
//!   not merely until its worker returns. At most [`ReadLimits::reads`]
//!   reads are held at once, and prefetches never take the share reserved
//!   for reads a view waits on. A refused read is a typed [`Refused`].
//! - **Latest wins per key.** Submitting a job for a key that is already
//!   queued replaces the queued job and cancels it; a running job for the key
//!   is cancelled (its token is set) and its result is dropped on landing by
//!   the store's generation check. Undelivered outcomes of the key's older
//!   generations are withdrawn.
//! - **Priority.** Normal jobs run before prefetch jobs; FIFO within one
//!   priority. When every admission is held, a normal job takes a queued
//!   prefetch's admission and the caller is told which prefetch it evicted.
//! - **Affinity.** A continuation page runs on the worker whose session
//!   issued the continuation, because the producer certificate lives there.
//! - **One undelivered outcome per read.** A newer partial page replaces an
//!   undelivered one, and the terminal outcome replaces it in place, so the
//!   outbox is bounded by admission. Posting never blocks a worker, and a
//!   terminal outcome is never refused.
//! - **Wake, don't poll.** Every posted outcome wakes the UI through one
//!   coalescing [`WakeSender`]; the UI takes outcomes in small batches.

use super::outbox::{Batch, Outbox};
use super::permit::{Held, Ledger, PermitShare, ReadLimits, ReadPermit, Saturation};
use super::{Delivery, OutlineCache, PageReader, Priority, ReadContext, ReadJob, ReadOutcome};
use crate::core::{ErrorValue, FaultCode};
use crate::model::pages::{Generation, PageKey, PageValue, ReadFailure};
use crate::runtime::actor::{ActorStartError, CancellationToken};
use crate::runtime::wake::{WakeReceiver, WakeSender, wake_channel};
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};

/// A read the pool took.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "an evicted prefetch's page slot must be cancelled"]
pub struct Admitted {
    /// The queued prefetch whose admission this read took, when every
    /// admission was held. It will never run and posts no outcome: the
    /// caller cancels its page slot now.
    pub evicted: Option<Evicted>,
}

/// A queued prefetch evicted for a read a view waits on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Evicted {
    /// Resource the prefetch would have filled.
    pub key: PageKey,
    /// Generation it was issued with.
    pub generation: Generation,
}

/// Why the pool refused a read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refused {
    /// Every admission it may use is held, and no queued prefetch can give
    /// one up. Capacity returns as the UI lands outcomes.
    Busy(Saturation),
    /// The pool has shut down.
    Closed,
}

impl Refused {
    /// The fault a view shows for a read the pool refused.
    #[must_use]
    pub fn failure(self) -> ReadFailure {
        ReadFailure::Fault(ErrorValue::new(
            FaultCode::Transport,
            match self {
                Self::Busy(_) => "The reader is busy with too many pages. Try this page again.",
                Self::Closed => "The reader has shut down.",
            },
        ))
    }
}

/// What the pool holds right now.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadLoad {
    /// Jobs no worker has taken yet.
    pub queued: usize,
    /// Jobs a worker is running.
    pub running: usize,
    /// Outcomes waiting for the UI.
    pub undelivered: usize,
    /// Admissions held, from queue to the UI's final drop.
    pub held: Held,
}

/// The job one worker is running: its key, generation, and token.
#[derive(Clone, Debug)]
struct RunningJob {
    key: PageKey,
    generation: Generation,
    cancel: CancellationToken,
}

/// A job waiting for a worker, with the admission it holds.
#[derive(Debug)]
struct QueuedRead {
    job: ReadJob,
    permit: ReadPermit,
}

/// A read a worker took. The worker's share of the read's admission moves
/// into the terminal outcome when the read finishes.
#[derive(Debug)]
pub(super) struct RunningRead {
    worker: usize,
    job: ReadJob,
    permit: PermitShare,
}

#[derive(Debug, Default)]
struct Queue {
    jobs: VecDeque<QueuedRead>,
    running: Vec<Option<RunningJob>>,
    closed: bool,
}

impl Queue {
    /// Removes every queued read `matches` selects. Dropping one returns its
    /// admission: a queued read has no outcome to wait for.
    fn extract(&mut self, matches: impl Fn(&QueuedRead) -> bool) -> Vec<QueuedRead> {
        let mut extracted = Vec::new();
        let mut index = 0;
        while index < self.jobs.len() {
            if self.jobs.get(index).is_some_and(&matches) {
                extracted.extend(self.jobs.remove(index));
            } else {
                index += 1;
            }
        }
        extracted
    }

    /// Tokens of running jobs `matches` selects.
    fn running_tokens(&self, matches: impl Fn(&RunningJob) -> bool) -> Vec<CancellationToken> {
        self.running
            .iter()
            .flatten()
            .filter(|running| matches(running))
            .map(|running| running.cancel.clone())
            .collect()
    }

    /// The best job `worker` may run: normal before prefetch, FIFO within
    /// a class, continuation pages only on their own worker.
    fn start(&mut self, worker: usize) -> Option<RunningRead> {
        if self.closed {
            return None;
        }
        let mut best: Option<(usize, Priority)> = None;
        for (index, queued) in self.jobs.iter().enumerate() {
            if queued.job.affinity.is_some_and(|pinned| pinned != worker) {
                continue;
            }
            if best.is_none_or(|(_, priority)| queued.job.priority > priority) {
                best = Some((index, queued.job.priority));
            }
        }
        let (index, _) = best?;
        let QueuedRead { job, permit } = self.jobs.remove(index)?;
        if let Some(slot) = self.running.get_mut(worker) {
            *slot = Some(RunningJob {
                key: job.key.clone(),
                generation: job.generation,
                cancel: job.cancel.clone(),
            });
        }
        Some(RunningRead {
            worker,
            job,
            permit: permit.share(),
        })
    }
}

/// State shared by the pool's handle and its workers.
#[derive(Debug)]
pub(super) struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    ledger: Arc<Ledger>,
    outbox: Outbox,
    wake: WakeSender,
    outlines: OutlineCache,
}

impl Shared {
    pub(super) fn new(workers: usize, limits: ReadLimits, wake: WakeSender) -> Self {
        Self {
            queue: Mutex::new(Queue {
                running: vec![None; workers.max(1)],
                ..Queue::default()
            }),
            ready: Condvar::new(),
            ledger: Ledger::new(limits),
            outbox: Outbox::default(),
            wake,
            outlines: OutlineCache::default(),
        }
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one job, or says why not. Every token is cancelled and every
    /// displaced value dropped after the locks are released.
    pub(super) fn submit(&self, job: ReadJob) -> Result<Admitted, Refused> {
        let key = job.key.clone();
        let generation = job.generation;
        let priority = job.priority;
        let (admitted, cancelled) = {
            let mut queue = self.queue();
            if queue.closed {
                return Err(Refused::Closed);
            }
            // Latest wins per key. The replaced jobs' admissions return as
            // they drop here, before the replacement asks for one.
            let mut cancelled = queue
                .extract(|queued| queued.job.key == key)
                .into_iter()
                .map(|replaced| replaced.job.cancel)
                .collect::<Vec<_>>();
            cancelled.extend(
                queue.running_tokens(|running| {
                    running.key == key && running.generation != generation
                }),
            );
            let admitted = match self.ledger.admit(priority) {
                Ok(permit) => Ok((permit, None)),
                Err(saturation) if priority == Priority::Normal => {
                    match queue.extract_first(|queued| queued.job.priority == Priority::Prefetch) {
                        Some(QueuedRead {
                            job: victim,
                            mut permit,
                        }) => {
                            permit.promote();
                            cancelled.push(victim.cancel);
                            Ok((
                                permit,
                                Some(Evicted {
                                    key: victim.key,
                                    generation: victim.generation,
                                }),
                            ))
                        }
                        None => Err(Refused::Busy(saturation)),
                    }
                }
                Err(saturation) => Err(Refused::Busy(saturation)),
            };
            let admitted = admitted.map(|(permit, evicted)| {
                queue.jobs.push_back(QueuedRead { job, permit });
                Admitted { evicted }
            });
            (admitted, cancelled)
        };
        // The slot now belongs to `generation`: older outcomes for the key
        // can only be superseded on landing, so they stop holding capacity.
        // A superseded read that is still running delivers its terminal
        // outcome later, as every read the UI did not cancel does.
        let stale = self
            .outbox
            .withdraw(|outcome| outcome.key == key && outcome.generation != generation);
        for token in &cancelled {
            token.cancel();
        }
        drop(stale);
        if admitted.is_ok() {
            self.ready.notify_all();
        }
        admitted
    }

    /// Raises queued jobs for `key` to normal priority and moves their
    /// admissions out of the prefetch share.
    pub(super) fn promote(&self, key: &PageKey) -> bool {
        let mut queue = self.queue();
        let mut found = false;
        for queued in queue
            .jobs
            .iter_mut()
            .filter(|queued| queued.job.key == *key)
        {
            queued.job.priority = Priority::Normal;
            queued.permit.promote();
            found = true;
        }
        found
    }

    /// Cancels every queued or running job for `key` and withdraws its
    /// undelivered outcomes. Tokens are set before the withdrawal, so no
    /// partial page of these reads is posted after this returns.
    pub(super) fn cancel(&self, key: &PageKey) -> bool {
        let (removed, running) = {
            let mut queue = self.queue();
            let removed = queue.extract(|queued| queued.job.key == *key);
            let running = queue.running_tokens(|running| running.key == *key);
            (removed, running)
        };
        let found = !removed.is_empty() || !running.is_empty();
        for token in removed
            .iter()
            .map(|queued| &queued.job.cancel)
            .chain(&running)
        {
            token.cancel();
        }
        drop(removed);
        drop(self.outbox.withdraw(|outcome| outcome.key == *key));
        found
    }

    /// Takes the next job `worker` may run, without waiting.
    #[cfg(test)]
    pub(super) fn try_start(&self, worker: usize) -> Option<RunningRead> {
        self.queue().start(worker)
    }

    /// Waits for the next job `worker` may run; `None` once the pool closes.
    fn next_read(&self, worker: usize) -> Option<RunningRead> {
        let mut queue = self.queue();
        loop {
            if queue.closed {
                return None;
            }
            if let Some(read) = queue.start(worker) {
                return Some(read);
            }
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Posts a partial page of a running read, unless it was cancelled.
    pub(super) fn post_partial(&self, read: &RunningRead, value: PageValue) {
        let outcome = ReadOutcome {
            key: read.job.key.clone(),
            generation: read.job.generation,
            worker: read.worker,
            priority: read.job.priority,
            delivery: Delivery::Partial(value),
            permit: read.permit.clone(),
        };
        let unused = self.outbox.post_partial(outcome, &read.job.cancel);
        let posted = unused.is_none();
        drop(unused);
        if posted {
            self.wake.wake();
        }
    }

    /// Ends a running read: posts its terminal outcome, which carries the
    /// worker's share of the admission, then frees the worker's slot. The
    /// outcome is queued before the slot clears, so a read is always either
    /// running or undelivered until the UI takes it.
    pub(super) fn finish(&self, read: RunningRead, result: Result<PageValue, ReadFailure>) {
        let RunningRead {
            worker,
            job,
            permit,
        } = read;
        let result = if job.cancel.is_cancelled() {
            Err(ReadFailure::Cancelled)
        } else {
            result
        };
        let replaced = self.outbox.post_terminal(ReadOutcome {
            key: job.key,
            generation: job.generation,
            worker,
            priority: job.priority,
            delivery: Delivery::Terminal(result),
            permit,
        });
        if let Some(slot) = self.queue().running.get_mut(worker) {
            *slot = None;
        }
        drop(replaced);
        self.wake.wake();
    }

    /// Takes one landing turn's outcomes.
    pub(super) fn take(&self, limit: NonZeroUsize) -> Batch {
        self.outbox.take(limit)
    }

    /// Closes the queue: queued jobs drop (returning their admissions) and
    /// running ones are told to stop. Workers exit after their current read.
    fn close(&self) {
        let (removed, running) = {
            let mut queue = self.queue();
            queue.closed = true;
            let removed = queue.jobs.drain(..).collect::<Vec<_>>();
            let running = queue.running_tokens(|_| true);
            (removed, running)
        };
        for token in removed
            .iter()
            .map(|queued| &queued.job.cancel)
            .chain(&running)
        {
            token.cancel();
        }
        drop(removed);
        self.ready.notify_all();
    }

    /// Queued jobs, for the schedule oracles.
    #[cfg(test)]
    pub(super) fn queued_reads(&self) -> Vec<(PageKey, Priority)> {
        self.queue()
            .jobs
            .iter()
            .map(|queued| (queued.job.key.clone(), queued.job.priority))
            .collect()
    }

    pub(super) fn load(&self) -> ReadLoad {
        let (queued, running) = {
            let queue = self.queue();
            (queue.jobs.len(), queue.running.iter().flatten().count())
        };
        ReadLoad {
            queued,
            running,
            undelivered: self.outbox.len(),
            held: self.ledger.held(),
        }
    }
}

impl Queue {
    /// Removes the oldest queued read `matches` selects.
    fn extract_first(&mut self, matches: impl Fn(&QueuedRead) -> bool) -> Option<QueuedRead> {
        let index = self.jobs.iter().position(matches)?;
        self.jobs.remove(index)
    }
}

impl RunningRead {
    /// Runs the read on this worker's reader. A panicking reader becomes one
    /// typed fault instead of taking the worker (and every later read) down.
    fn run<R: PageReader>(
        &self,
        reader: &mut R,
        shared: &Shared,
    ) -> Result<PageValue, ReadFailure> {
        if self.job.cancel.is_cancelled() {
            return Err(ReadFailure::Cancelled);
        }
        let publish = |value| shared.post_partial(self, value);
        let context = ReadContext {
            worker: self.worker,
            cancel: &self.job.cancel,
            outlines: &shared.outlines,
            progress: Some(&publish),
        };
        let _reading = crate::runtime::traffic::Reading::begin();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            reader.read(&self.job.request, &context)
        }))
        .unwrap_or_else(|_| {
            Err(ReadFailure::Fault(ErrorValue::new(
                FaultCode::Protocol,
                "the read worker panicked while mapping a reply",
            )))
        })
    }
}

fn run_worker<R: PageReader>(worker: usize, mut reader: R, shared: &Shared) {
    while let Some(read) = shared.next_read(worker) {
        let result = read.run(&mut reader, shared);
        shared.finish(read, result);
    }
}

/// A fixed pool of read workers.
pub struct ReadPool {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
    wake: Option<WakeReceiver>,
}

impl std::fmt::Debug for ReadPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadPool")
            .field("workers", &self.workers.len())
            .field("load", &self.load())
            .finish_non_exhaustive()
    }
}

impl ReadPool {
    /// Starts `workers` threads, each with its own reader from `make`, with
    /// the window's [`ReadLimits::DEFAULT`].
    ///
    /// # Errors
    /// Returns [`ActorStartError`] when a worker thread cannot start.
    pub fn start<R: PageReader>(
        workers: usize,
        make: impl Fn(usize) -> R,
    ) -> Result<Self, ActorStartError> {
        Self::start_with(workers, ReadLimits::DEFAULT, make)
    }

    /// [`Self::start`] with explicit admission limits.
    ///
    /// # Errors
    /// Returns [`ActorStartError`] when a worker thread cannot start.
    pub fn start_with<R: PageReader>(
        workers: usize,
        limits: ReadLimits,
        make: impl Fn(usize) -> R,
    ) -> Result<Self, ActorStartError> {
        let (wake, receiver) = wake_channel();
        let workers = workers.max(1);
        let mut pool = Self {
            shared: Arc::new(Shared::new(workers, limits, wake)),
            workers: Vec::with_capacity(workers),
            wake: Some(receiver),
        };
        for index in 0..workers {
            let reader = make(index);
            let shared = Arc::clone(&pool.shared);
            let handle = thread::Builder::new()
                .name(format!("nudox-read-{index}"))
                .spawn(move || run_worker(index, reader, &shared))
                .map_err(|error| ActorStartError::from_spawn_error(&error))?;
            pool.workers.push(handle);
        }
        Ok(pool)
    }

    /// Takes the wake receiver; the store's UI task awaits it.
    pub fn take_wake(&mut self) -> Option<WakeReceiver> {
        self.wake.take()
    }

    /// Returns the number of workers.
    #[must_use]
    pub fn workers(&self) -> usize {
        self.workers.len()
    }

    /// The admission limits.
    #[must_use]
    pub fn limits(&self) -> ReadLimits {
        self.shared.ledger.limits()
    }

    /// Admits one job. A queued job for the same key is replaced and
    /// cancelled; a running one with another generation is cancelled.
    ///
    /// # Errors
    /// [`Refused::Busy`] when every admission the job may use is held (a
    /// normal job first takes a queued prefetch's), [`Refused::Closed`] after
    /// shutdown. A refused job never runs and posts no outcome.
    pub fn submit(&self, job: ReadJob) -> Result<Admitted, Refused> {
        self.shared.submit(job)
    }

    /// Raises a queued job for `key` to normal priority. Returns whether a
    /// queued job was found (a running job needs no promotion).
    #[must_use]
    pub fn promote(&self, key: &PageKey) -> bool {
        self.shared.promote(key)
    }

    /// Cancels every queued or running job for `key` and withdraws its
    /// undelivered outcomes. Returns whether any job was found.
    #[must_use]
    pub fn cancel(&self, key: &PageKey) -> bool {
        self.shared.cancel(key)
    }

    /// Takes up to `limit` finished outcomes without waiting. Each keeps its
    /// read's admission until it is dropped.
    pub fn take(&self, limit: NonZeroUsize) -> Batch {
        self.shared.take(limit)
    }

    /// What the pool holds now.
    #[must_use]
    pub fn load(&self) -> ReadLoad {
        self.shared.load()
    }

    /// Returns the number of queued (not yet running) jobs.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.shared.queue().jobs.len()
    }

    /// Returns the number of jobs currently running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.shared.queue().running.iter().flatten().count()
    }

    fn close_and_join(&mut self) {
        self.shared.close();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        self.shared.wake.close();
    }
}

impl Drop for ReadPool {
    fn drop(&mut self) {
        self.close_and_join();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
#[path = "pool_tests.rs"]
mod tests;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
#[path = "schedule_tests.rs"]
mod schedule_tests;
