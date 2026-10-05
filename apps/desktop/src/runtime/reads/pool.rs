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
use super::permit::{
    AdmissionFailure, Held, Ledger, PermitShare, ReadId, ReadLimits, ReadPermit, Saturation,
};
use super::{Delivery, OutlineCache, PageReader, Priority, ReadContext, ReadJob, ReadOutcome};
use crate::core::{ErrorValue, FaultCode};
use crate::model::pages::{Generation, PageKey, PageValue, ReadFailure};
use crate::runtime::actor::{ActorStartError, CancellationToken};
use crate::runtime::wake::{WakeReceiver, WakeSender, wake_channel};
use std::collections::{BTreeSet, VecDeque};
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
    /// No fresh monotonic identity remains in this pool. Dropping admitted
    /// reads cannot recover this permanent refusal.
    IdentityExhausted,
    /// The pool has shut down.
    Closed,
    /// A newer same-key request or cancellation won during admission cleanup.
    Superseded,
}

impl From<AdmissionFailure> for Refused {
    fn from(value: AdmissionFailure) -> Self {
        match value {
            AdmissionFailure::Capacity(saturation) => Self::Busy(saturation),
            AdmissionFailure::IdentityExhausted => Self::IdentityExhausted,
        }
    }
}

impl Refused {
    /// The fault a view shows for a read the pool refused.
    #[must_use]
    pub fn failure(self) -> ReadFailure {
        match self {
            Self::Superseded => ReadFailure::Cancelled,
            Self::Busy(_) => ReadFailure::Fault(ErrorValue::new(
                FaultCode::Transport,
                "The reader is busy with too many pages. Try this page again.",
            )),
            Self::IdentityExhausted => ReadFailure::Fault(ErrorValue::new(
                FaultCode::Protocol,
                "The reader has exhausted its read identities.",
            )),
            Self::Closed => ReadFailure::Fault(ErrorValue::new(
                FaultCode::Transport,
                "The reader has shut down.",
            )),
        }
    }
}

/// What the pool holds right now.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadLoad {
    /// Jobs no worker has taken yet, including bounded foreground admission
    /// cleanup tickets. Tickets hold no permit and are never dispatched.
    pub queued: usize,
    /// Jobs a worker is running.
    pub running: usize,
    /// Outcomes waiting for the UI.
    pub undelivered: usize,
    /// Admissions held, from queue to the UI's final drop.
    pub held: Held,
}

impl ReadLoad {
    /// Snapshot of work that still needs a worker or UI landing. Delivered
    /// clones can hold capacity without being work: held is not readiness.
    #[must_use]
    pub const fn activity(self) -> super::PoolLoad {
        super::PoolLoad {
            queued: self.queued,
            running: self.running,
            undelivered: self.undelivered,
        }
    }

    #[must_use]
    pub const fn is_idle(self) -> bool {
        self.activity().is_idle()
    }
}

/// The job one worker is running: its key, generation, and token.
#[derive(Clone, Debug)]
struct RunningJob {
    key: PageKey,
    generation: Generation,
    cancel: CancellationToken,
    read: ReadId,
    // Exact running ownership is revoked under queue before callbacks run.
    revoked: bool,
}

/// A job waiting for a worker, with the admission it holds.
#[derive(Debug)]
struct QueuedRead {
    job: ReadJob,
    permit: ReadPermit,
}

/// A foreground submit retrying admission after dropping obsolete outbox
/// entries. This ticket holds no permit or page-slot authority. The existing
/// request token fences phase two against same-key reentrant submit/cancel.
#[derive(Debug)]
struct Preparing {
    key: PageKey,
    cancel: CancellationToken,
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
    preparing: Vec<Preparing>,
    running: Vec<Option<RunningJob>>,
    // Under queue, a test can prove a real worker has atomically entered
    // Condvar::wait before exercising a callback unwind; no production state.
    #[cfg(test)]
    waiting: BTreeSet<usize>,
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

    /// Revoke exact running identities before invoking their tokens outside
    /// the lock. Finish uses this same queue record, so a delayed callback
    /// cannot race a previously sampled successful terminal into the outbox.
    fn revoke_running(
        &mut self,
        matches: impl Fn(&RunningJob) -> bool,
    ) -> Vec<(ReadId, CancellationToken)> {
        self.running
            .iter_mut()
            .flatten()
            .filter(|running| matches(running))
            .map(|running| {
                running.revoked = true;
                (running.read, running.cancel.clone())
            })
            .collect()
    }

    /// The best job `worker` may run: normal before prefetch, FIFO within
    /// a class, continuation pages only on their own worker.
    fn start(&mut self, worker: usize) -> Option<RunningRead> {
        let slot = self.running.get_mut(worker)?;
        if self.closed || slot.is_some() {
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
        let permit = permit.share();
        // This exclusive field borrow pins the valid idle worker ownership
        // before any queued job or admission is consumed.
        debug_assert!(slot.is_none(), "one read per worker");
        *slot = Some(RunningJob {
            key: job.key.clone(),
            generation: job.generation,
            cancel: job.cancel.clone(),
            read: permit.id(),
            revoked: false,
        });
        Some(RunningRead {
            worker,
            job,
            permit,
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

/// A committed queue mutation must notify parked workers even if a later
/// cleanup unwinds. Borrowed, payload-free, and created only after queue is
/// unlocked. Callback isolation belongs to CancellationToken; this guard
/// also covers other unwinds. This is readiness, not worker joining.
struct ReadyNotify<'a> {
    ready: &'a Condvar,
    armed: bool,
}

impl Drop for ReadyNotify<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.ready.notify_all();
        }
    }
}

/// A non-notification cleanup unwind must not strand a permit-free
/// preparing ticket and make readiness wait forever. Removal is identity-specific;
/// the ticket drops after queue is unlocked and releases no real admission.
struct PreparingCleanup<'a> {
    shared: &'a Shared,
    token: &'a CancellationToken,
    active: bool,
}

impl Drop for PreparingCleanup<'_> {
    fn drop(&mut self) {
        if self.active {
            let ticket = { self.shared.queue().take_preparing(self.token) };
            drop(ticket);
        }
    }
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

    /// One scheduler lock order: queue -> outbox -> leaf ledger. Posting a
    /// partial and taking/withdrawing results only take outbox; release only
    /// takes ledger. Displaced values and cancellation callbacks run unlocked.
    /// Started identities and request tokens fence reentrant callbacks.
    pub(super) fn submit(&self, job: ReadJob) -> Result<Admitted, Refused> {
        let key = job.key.clone();
        let generation = job.generation;
        let priority = job.priority;
        let token = job.cancel.clone();
        let mut pending = Some(job);
        let (
            mut admitted,
            removed,
            retired_jobs,
            retired_tickets,
            cancelled,
            stale,
            rejected_permit,
            retry,
        ) = {
            let mut queue = self.queue();
            if queue.closed {
                return Err(Refused::Closed);
            }
            let mut removed = queue.extract(|queued| queued.job.key == key);
            let mut cancelled = removed
                .iter()
                .map(|queued| queued.job.cancel.clone())
                .collect::<Vec<_>>();
            let retired_tickets = queue.withdraw_preparing(&key);
            cancelled.extend(retired_tickets.iter().map(|ticket| ticket.cancel.clone()));
            let running = queue
                .revoke_running(|running| running.key == key && running.generation != generation);
            let mut stale = self
                .outbox
                .ids(|outcome| outcome.key == key && outcome.generation != generation);
            let obsolete_outbox = !stale.is_empty();
            stale.extend(running.iter().map(|(read, _)| *read));
            cancelled.extend(running.into_iter().map(|(_, token)| token));
            let mut rejected_permit = None;
            let mut retired_jobs = Vec::new();
            let permit = if let Some(replaced) = removed.pop() {
                let QueuedRead {
                    job: replaced_job,
                    mut permit,
                } = replaced;
                retired_jobs.push(replaced_job);
                match permit.reclass(priority) {
                    Ok(()) => Ok((permit, None)),
                    Err(saturation) => {
                        rejected_permit = Some(permit);
                        Err(AdmissionFailure::from(saturation))
                    }
                }
            } else if priority == Priority::Normal && obsolete_outbox {
                // First dispose this key's obsolete outbox ownership before
                // evicting another key's useful queued prefetch. Phase two
                // may still evict if a worker/UI share keeps capacity held.
                self.ledger.admit(priority).map(|permit| (permit, None))
            } else {
                queue.admit(&self.ledger, priority)
            };
            let admitted = match permit {
                Ok((permit, victim)) => {
                    let evicted = victim.as_ref().map(|victim| Evicted {
                        key: victim.key.clone(),
                        generation: victim.generation,
                    });
                    if let Some(victim) = victim {
                        cancelled.push(victim.cancel.clone());
                        retired_jobs.push(victim);
                    }
                    let job = pending
                        .take()
                        .unwrap_or_else(|| unreachable!("submit owns its pending job"));
                    queue.jobs.push_back(QueuedRead { job, permit });
                    Ok(Admitted { evicted })
                }
                Err(refused) => Err(Refused::from(refused)),
            };
            // Retry only when disposable obsolete outbox ownership may be
            // the reason a foreground read was refused. A ticket per such
            // key is bounded by the captured admitted outbox, not all asks.
            let retry = matches!(admitted, Err(Refused::Busy(_)))
                && priority == Priority::Normal
                && obsolete_outbox
                && queue.preparing.len() < self.ledger.limits().reads();
            if retry {
                queue.preparing.push(Preparing {
                    key: key.clone(),
                    cancel: token.clone(),
                });
            }
            (
                admitted,
                removed,
                retired_jobs,
                retired_tickets,
                cancelled,
                stale,
                rejected_permit,
                retry,
            )
        };
        let mut notify = ReadyNotify {
            ready: &self.ready,
            armed: admitted.is_ok(),
        };
        let mut _cleanup = PreparingCleanup {
            shared: self,
            token: &token,
            active: retry,
        };
        for old in &cancelled {
            old.cancel();
        }
        let withdrawn = self
            .outbox
            .withdraw(|outcome| stale.contains(&outcome.permit.id()));
        drop((
            withdrawn,
            removed,
            retired_jobs,
            retired_tickets,
            rejected_permit,
        ));
        if retry {
            // No lock is held while obsolete payloads return their shares.
            // A same-key callback may have installed a newer read meanwhile.
            let (answer, ticket, victim) = {
                let mut queue = self.queue();
                let ticket = queue.take_preparing(&token);
                let answer = if queue.closed {
                    Err(Refused::Closed)
                } else if ticket.is_none() || token.is_cancelled() {
                    Err(Refused::Superseded)
                } else {
                    queue.admit(&self.ledger, priority).map_err(Refused::from)
                };
                match answer {
                    Ok((permit, victim)) => {
                        let evicted = victim.as_ref().map(|victim| Evicted {
                            key: victim.key.clone(),
                            generation: victim.generation,
                        });
                        let job = pending
                            .take()
                            .unwrap_or_else(|| unreachable!("phase two owns its pending job"));
                        queue.jobs.push_back(QueuedRead { job, permit });
                        (Ok(Admitted { evicted }), ticket, victim)
                    }
                    Err(refused) => (Err(refused), ticket, None),
                }
            };
            // Phase two may also commit admission before this callback.
            notify.armed |= answer.is_ok();
            if let Some(victim) = &victim {
                victim.cancel.cancel();
            }
            drop((ticket, victim));
            admitted = answer;
            _cleanup.active = false;
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
        let (removed, running, preparing, stale) = {
            let mut queue = self.queue();
            let removed = queue.extract(|queued| queued.job.key == *key);
            let preparing = queue.withdraw_preparing(key);
            let running = queue.revoke_running(|running| running.key == *key);
            let mut stale = self.outbox.ids(|outcome| outcome.key == *key);
            stale.extend(running.iter().map(|(read, _)| *read));
            (
                removed,
                running
                    .into_iter()
                    .map(|(_, token)| token)
                    .collect::<Vec<_>>(),
                preparing,
                stale,
            )
        };
        let found = !preparing.is_empty()
            || !removed.is_empty()
            || !running.is_empty()
            || !stale.is_empty();
        for token in removed
            .iter()
            .map(|queued| &queued.job.cancel)
            .chain(&running)
            .chain(preparing.iter().map(|ticket| &ticket.cancel))
        {
            token.cancel();
        }
        let withdrawn = self
            .outbox
            .withdraw(|outcome| stale.contains(&outcome.permit.id()));
        drop((removed, withdrawn, preparing));
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
            #[cfg(test)]
            queue.waiting.insert(worker);
            queue = self
                .ready
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
            #[cfg(test)]
            queue.waiting.remove(&worker);
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
        let posted = self.outbox.post_partial(outcome, &read.job.cancel);
        let wake = posted.posted;
        drop(posted.unused);
        if wake {
            self.wake.wake();
        }
    }

    /// Ends a running read: posts its terminal outcome, which carries the
    /// worker's share of the admission, then frees the worker's slot. The
    /// outcome is queued before the slot clears, so a read is always either
    /// running or undelivered until the UI takes it.
    pub(super) fn finish(&self, read: RunningRead, result: Result<PageValue, ReadFailure>) {
        #[cfg(not(test))]
        self.finish_impl(read, result);
        #[cfg(test)]
        self.finish_impl(read, result, || {});
    }

    fn finish_impl(
        &self,
        read: RunningRead,
        result: Result<PageValue, ReadFailure>,
        #[cfg(test)] before_queue: impl FnOnce(),
    ) {
        let RunningRead {
            worker,
            job,
            permit,
        } = read;
        let identity = permit.id();
        #[cfg(test)]
        before_queue();
        // Hold queue across both publication and slot clearance. A load
        // snapshot takes the same queue -> outbox order and cannot see a gap.
        let mut queue = self.queue();
        let revoked = queue
            .running
            .get(worker)
            .and_then(Option::as_ref)
            .is_some_and(|running| running.read == identity && running.revoked);
        let (terminal, discarded) = if revoked || job.cancel.is_cancelled() {
            (Err(ReadFailure::Cancelled), Some(result))
        } else {
            (result, None)
        };
        let replaced = self.outbox.post_terminal(ReadOutcome {
            key: job.key,
            generation: job.generation,
            worker,
            priority: job.priority,
            delivery: Delivery::Terminal(terminal),
            permit,
        });
        if let Some(slot) = queue.running.get_mut(worker) {
            debug_assert!(
                slot.as_ref()
                    .is_some_and(|running| running.read == identity),
                "terminal must own its worker slot"
            );
            if slot
                .as_ref()
                .is_some_and(|running| running.read == identity)
            {
                *slot = None;
            }
        }
        drop(queue);
        // A useful success displaced by revocation is also dropped unlocked.
        drop((replaced, discarded));
        self.wake.wake();
    }

    /// Takes one landing turn's outcomes.
    pub(super) fn take(&self, limit: NonZeroUsize) -> Batch {
        self.outbox.take(limit)
    }

    /// Closes the queue: queued jobs drop (returning their admissions) and
    /// running ones are told to stop. Workers exit after their current read.
    fn close(&self) {
        let (removed, running, preparing) = {
            let mut queue = self.queue();
            queue.closed = true;
            let removed = queue.jobs.drain(..).collect::<Vec<_>>();
            let running = queue.revoke_running(|_| true);
            let preparing = queue.preparing.drain(..).collect::<Vec<_>>();
            (removed, running, preparing)
        };
        let _notify = ReadyNotify {
            ready: &self.ready,
            armed: true,
        };
        for token in removed
            .iter()
            .map(|queued| &queued.job.cancel)
            .chain(running.iter().map(|(_, token)| token))
            .chain(preparing.iter().map(|ticket| &ticket.cancel))
        {
            token.cancel();
        }
        drop((removed, preparing));
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

    /// The three readiness counts are one queue -> outbox snapshot. Held
    /// is a separate leaf-ledger observation of ownership, including values
    /// already delivered to UI hands; it never participates in is_idle.
    pub(super) fn load(&self) -> ReadLoad {
        let (queued, running, undelivered) = {
            let queue = self.queue();
            (
                queue.jobs.len() + queue.preparing.len(),
                queue.running.iter().flatten().count(),
                self.outbox.len(),
            )
        };
        ReadLoad {
            queued,
            running,
            undelivered,
            held: self.ledger.held(),
        }
    }
}

impl Queue {
    /// Admission and eviction share one ledger and queue decision. The
    /// returned victim is cancelled and dropped by the caller, unlocked.
    fn admit(
        &mut self,
        ledger: &Arc<Ledger>,
        priority: Priority,
    ) -> Result<(ReadPermit, Option<ReadJob>), AdmissionFailure> {
        match ledger.admit(priority) {
            Ok(permit) => Ok((permit, None)),
            Err(refused) if priority == Priority::Normal => {
                match self.extract_first(|queued| queued.job.priority == Priority::Prefetch) {
                    Some(QueuedRead { job, mut permit }) => {
                        // This permit has never started; transfer its existing
                        // identity even if no fresh identity can be minted.
                        permit.promote();
                        Ok((permit, Some(job)))
                    }
                    None => Err(refused),
                }
            }
            Err(refused) => Err(refused),
        }
    }

    /// Transfer ticket ownership to the caller, so even its key/token drop
    /// takes place after releasing queue and cancelling callbacks.
    fn withdraw_preparing(&mut self, key: &PageKey) -> Vec<Preparing> {
        let mut tickets = Vec::new();
        let mut index = 0;
        while index < self.preparing.len() {
            if self
                .preparing
                .get(index)
                .is_some_and(|ticket| ticket.key == *key)
            {
                tickets.push(self.preparing.remove(index));
            } else {
                index += 1;
            }
        }
        tickets
    }

    fn take_preparing(&mut self, token: &CancellationToken) -> Option<Preparing> {
        let index = self
            .preparing
            .iter()
            .position(|ticket| std::ptr::eq(ticket.cancel.flag(), token.flag()))?;
        Some(self.preparing.remove(index))
    }

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
    /// shutdown, or [`Refused::IdentityExhausted`] permanently when no fresh
    /// ID remains (an existing queued permit may still transfer). A refused
    /// job never runs and posts no outcome.
    pub fn submit(&self, job: ReadJob) -> Result<Admitted, Refused> {
        self.shared.submit(job)
    }

    /// A fixture submits into the real shared scheduler without cloning the
    /// owning handle or its worker-join lifecycle.
    #[cfg(test)]
    pub(crate) fn test_submitter(
        &self,
    ) -> impl Fn(ReadJob) -> Result<Admitted, Refused> + Send + Sync + 'static + use<> {
        let shared = Arc::clone(&self.shared);
        move |job| shared.submit(job)
    }

    /// Observe exact minted identities under the production lock order.
    #[cfg(test)]
    pub(crate) fn test_ids(&self, key: &PageKey) -> BTreeSet<u64> {
        let queue = self.shared.queue();
        let mut ids = self.shared.outbox.ids(|outcome| outcome.key == *key);
        ids.extend(
            queue
                .running
                .iter()
                .flatten()
                .filter(|read| read.key == *key)
                .map(|read| read.read),
        );
        ids.extend(
            queue
                .jobs
                .iter()
                .filter(|read| read.job.key == *key)
                .map(|read| read.permit.id()),
        );
        ids.into_iter().map(ReadId::get).collect()
    }

    #[cfg(test)]
    pub(crate) fn test_outcome_id(outcome: &ReadOutcome) -> u64 {
        outcome.permit.id().get()
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

    /// Completed package publications also supersede the shared outline
    /// cache. Its insert fence covers cancelled workers still finishing I/O.
    pub(crate) fn invalidate_package_outlines(
        &self,
        packages: &BTreeSet<crate::model::pages::PackageRef>,
    ) {
        self.shared.outlines.invalidate(packages);
    }

    /// Takes up to `limit` finished outcomes without waiting. Each keeps its
    /// read's admission until it is dropped.
    pub fn take(&self, limit: NonZeroUsize) -> Batch {
        self.shared.take(limit)
    }

    /// UI compatibility path: at most eight landings and a wake rearmed
    /// for the remaining outbox. The consumer yields after every turn.
    pub(crate) fn drain_for(&self, visible: &BTreeSet<PageKey>) -> Vec<ReadOutcome> {
        let batch = self.shared.outbox.take_for(super::LANDING_BUDGET, visible);
        if batch.remaining > 0 {
            self.shared.wake.wake();
        }
        batch.outcomes
    }

    /// Takes one bounded UI batch with no currently visible keys.
    pub fn drain(&self) -> Vec<ReadOutcome> {
        self.drain_for(&BTreeSet::new())
    }

    /// What the pool holds now.
    #[must_use]
    pub fn load(&self) -> ReadLoad {
        self.shared.load()
    }

    /// Returns the number of queued (not yet running) jobs.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.load().queued
    }

    /// Returns the number of jobs currently running.
    #[must_use]
    pub fn running(&self) -> usize {
        self.load().running
    }

    // Cancellation isolates faulty callbacks and attempts every registered
    // interrupt before we join. Joining still requires PageReader to honor
    // cancellation or return itself; arbitrary blocking readers have no
    // universal termination guarantee. This is the existing synchronous
    // lifetime boundary, not a detached thread or an unbounded GUI reaper.
    /// Revoke admission and interrupt readers without joining on the UI thread.
    pub(crate) fn stop(&self) { self.shared.close(); self.shared.wake.close(); }

    pub(crate) fn take_finish(&mut self) -> crate::runtime::worker_finish::WorkerFinish {
        self.stop();
        crate::runtime::worker_finish::WorkerFinish::from_workers(std::mem::take(&mut self.workers))
    }

    fn close_and_join(&mut self) {
        self.stop();
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
