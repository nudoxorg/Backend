//! The durable job queue — the poison-pill-safe spine of the pipeline.
//!
//! A scratch-backed work queue (INDEX-PLAN IP-4): jobs live in the ephemeral
//! `scratch.sqlite` `jobs` table and leases in its `claims` table. Every job
//! carries its [`heart::ResolutionState`], its attempt count, and a claim
//! lease; a failed job is retried *only* if its [`heart::FailureKind`] is
//! retriable *and* it is under the attempt ceiling — otherwise it is
//! dead-lettered. A retry is not runnable until its backoff elapses
//! (`not_before` on the job payload). This is what makes a package whose parse
//! crashes or hangs a *bounded* problem rather than a livelock.
//!
//! The scratch store is a single `rusqlite` connection (`!Sync`), so it is held
//! behind a [`Mutex`]. Leasing is atomic via the `claims` table's
//! insert-if-absent-or-expired protocol
//! ([`crate::scratch::ScratchStore::claim_job`]), which is the scratch analog
//! of the former postgres `SELECT ... FOR UPDATE SKIP LOCKED` — two workers
//! racing to claim the same job see at most one success.

use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::scratch::{
    ScratchStore,
    jobs::{JobRow, JobState},
};
use chrono::{DateTime, TimeZone, Utc};
use heart::{FailureKind, PackageId, ResolutionState};

use crate::{error::QueueError, schema::codec};

/// The scratch `jobs.kind` tag every indexing job carries. A single kind today;
/// present so the scratch table can host other work kinds without ambiguity.
const INDEX_JOB_KIND: &str = "index_package";

/// One unit of pipeline work: index this package.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Job {
    /// The stable job id (row identity, distinct from the package it targets).
    pub id: JobId,

    /// The package to index.
    pub package: PackageId,

    /// The package's current lifecycle state (mirrors the global index; carried
    /// on the job so a worker needn't re-read it to resume).
    pub state: ResolutionState,

    /// How many times this job has been attempted.
    pub attempts: u32,

    /// When the job was first enqueued.
    pub enqueued_at: DateTime<Utc>,

    /// The lease deadline: while set and in the future, the job is owned by a
    /// worker and invisible to `dequeue_batch`. `None` when idle.
    pub lease_until: Option<DateTime<Utc>>,
}

/// A compile-time witness that a job was leased by THIS worker **at dequeue
/// time**.
///
/// # What this witness proves — and what it does NOT
///
/// `dequeue_batch` is the only constructor: callers cannot mint a `LeasedJob`
/// by wrapping an arbitrary `Job`. So the witness closes exactly one bug at
/// compile time — *operating on a job this worker never dequeued* (mint-forgery
/// / passing a bare `JobId` to a terminal op). Terminal operations —
/// [`Queue::complete`] and [`Queue::fail`] — *consume* the value so the witness
/// cannot be reused after the job is settled; non-terminal
/// [`Queue::renew_lease`] borrows it so the caller can heartbeat across a run.
///
/// It does **NOT** prove the lease is *still held now*. Leases **expire**, and
/// the reclaimer ([`Queue::reclaim_expired_leases`]) may hand this job to
/// another worker while this witness is still in hand. Holding a `LeasedJob` at
/// `complete()` therefore proves "I leased this at dequeue" — a *temporal*
/// property (still-leased-at-commit) is fundamentally not expressible as a
/// value that outlives the instant it was true. That race is closed at runtime,
/// not by this type: the settling operations re-check the `claims` row and
/// surface [`QueueError::LeaseLost`] when the claim is gone or belongs to
/// another worker.
///
/// So the witness and the runtime guard are **complementary, not redundant**:
/// the type rules out mint-forgery at compile time; the claim re-check rules
/// out the expiry/reclaim race at commit time. Neither subsumes the other.
///
/// This mirrors the capability-witness pattern in Phase 4a authz and the
/// sandbox `Job::seal` typestate — but scoped honestly: it makes "settle a job
/// you never leased" unrepresentable, not "settle a job whose lease you already
/// lost".
#[derive(Debug)]
pub struct LeasedJob {
    /// The underlying job row.
    job: Job,
    /// The lease deadline as stamped at dequeue time (informational only). The
    /// scratch `claims` row is the authoritative source — this snapshot lets a
    /// caller cheaply notice a lease that has *already* lapsed locally, but it
    /// can go stale: only the claim re-check on the settling operation
    /// proves the lease still held.
    lease_until: DateTime<Utc>,
}

impl LeasedJob {
    /// The underlying job (immutable reference).
    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The stable job id.
    pub fn id(&self) -> JobId {
        self.job.id
    }

    /// The package this job targets.
    pub fn package(&self) -> PackageId {
        self.job.package
    }

    /// The current [`ResolutionState`] as decoded from the row at dequeue time.
    pub fn state(&self) -> &ResolutionState {
        &self.job.state
    }

    /// How many times this job has been attempted (includes the current
    /// attempt).
    pub fn attempts(&self) -> u32 {
        self.job.attempts
    }

    /// The lease deadline as stamped by `dequeue_batch` at dequeue time. This
    /// is a local snapshot, not a live claim: it can lapse (and the row be
    /// reclaimed) without this value changing. Treat it as a cheap hint for
    /// "should I even bother heartbeating?"; the authoritative check is the
    /// claim re-check on `complete`/`fail`/`renew_lease`.
    pub fn lease_until(&self) -> DateTime<Utc> {
        self.lease_until
    }

    /// Consume the witness and return the inner [`Job`]. Prefer the typed
    /// accessors above; `into_inner` is an escape hatch for callers that need
    /// the whole struct (e.g. to serialise it for observability).
    pub fn into_inner(self) -> Job {
        self.job
    }
}

/// The stable identity of a queued job.
///
/// A job's scratch `job_key` is the package uuid hex, so there is exactly one
/// live job per package (the former postgres `ON CONFLICT (package)`
/// idempotency) and [`JobId`] round-trips losslessly to and from that key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobId(pub uuid::Uuid);

impl JobId {
    /// The job's scratch `job_key`: the target package's uuid, simple (dashed)
    /// hex. One job per package, so the key is the package identity.
    fn job_key(self) -> String {
        self.0.to_string()
    }

    /// Recover a [`JobId`] from a scratch `job_key`. Errors are impossible for
    /// keys this crate wrote (they are always package uuids); a malformed key
    /// therefore indicates a foreign writer and is surfaced as `None`.
    fn from_job_key(key: &str) -> Option<Self> {
        uuid::Uuid::parse_str(key).ok().map(JobId)
    }
}

/// The JSON payload persisted in the scratch `jobs.payload` column — the fields
/// the postgres `jobs` row carried as native columns (package, priority, and
/// the state discriminant), now travelling as one serialised blob.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct JobPayload {
    /// The target package uuid.
    package: uuid::Uuid,
    /// The scheduling priority (higher dequeues first).
    priority: i32,
    /// The lifecycle-state discriminant token (see [`state_discriminant`]).
    state_token: String,
    /// Unix milliseconds before which a `Queued` job must not be claimed.
    ///
    /// Absent (or `None`) means immediately runnable. Stamped when a retriable
    /// failure is re-queued and cleared on a successful claim. Lives in the
    /// payload because that blob is the in-row field
    /// [`ScratchStore::next_queued_jobs`] already returns; the jobs table
    /// has no separate column for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_before: Option<i64>,
}

/// The retry schedule the queue applies when a job fails. Combined with
/// [`FailureKind::is_retriable`] to decide retry-vs-dead-letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
    /// The attempt ceiling; at or beyond this, a job is dead-lettered
    /// regardless of failure kind. `NonZero` so a misconfigured `0` (which
    /// would dead-letter every job on its first failure) is
    /// unrepresentable.
    pub max_attempts: NonZeroU32,

    /// The base backoff for the first retry.
    pub base_backoff: Duration,

    /// The cap on exponential backoff growth.
    pub max_backoff: Duration,
}

impl RetryPolicy {
    /// Compute the backoff before the `attempt`-th retry (exponential, capped).
    pub fn backoff_for(&self, attempt: u32) -> Duration {
        // base * 2^attempt, saturating, then clamped to max_backoff.
        let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
        let scaled = self
            .base_backoff
            .saturating_mul(factor.min(u64::from(u32::MAX)) as u32);
        scaled.min(self.max_backoff)
    }

    /// The decision a failure yields under this policy: retry (with a delay) or
    /// dead-letter. Retries only if the kind is retriable *and* attempts
    /// remain.
    pub fn decide(&self, kind: FailureKind, attempts: u32) -> RetryDecision {
        if kind.is_retriable() && attempts < self.max_attempts.get() {
            RetryDecision::Retry {
                after: self.backoff_for(attempts),
            }
        } else {
            RetryDecision::DeadLetter
        }
    }
}

/// A job's scheduling priority: higher dequeues first, ties broken FIFO by
/// `enqueued_at` (see [`Queue::dequeue_batch`]). A thin, `Ord` newtype over the
/// job payload's `priority` field so callers cannot confuse it with an attempt
/// count or a lease. The [`Default`] is `0` — the value every existing caller
/// carries, so priority is a purely additive scheduling hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Priority(i32);

impl Priority {
    /// The default, neutral priority (`0`): normal FIFO scheduling.
    pub const NORMAL: Priority = Priority(0);

    /// Construct an explicit priority. Larger values run earlier.
    pub const fn new(value: i32) -> Self {
        Priority(value)
    }

    /// The raw `int` value carried on the job payload.
    pub const fn get(self) -> i32 {
        self.0
    }
}

impl From<i32> for Priority {
    fn from(value: i32) -> Self {
        Priority(value)
    }
}

/// What to do with a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Re-enqueue after the given delay.
    Retry { after: Duration },
    /// Move to the dead-letter state; stop attempting.
    DeadLetter,
}

/// The durable scratch-backed job queue.
///
/// # Typestate simplification (INDEX-PLAN IP-4)
///
/// The former `Queue<S = Live>` `Cold`/`Live`/[`Connect`](heart::Connect)
/// typestate existed to gate query methods behind a verified postgres
/// connection. The scratch store is a local sqlite file opened synchronously
/// (and infallibly probed) by [`ScratchStore::open`]; there is no remote
/// connection to verify, so the typestate no longer pays for itself and has
/// been removed. A `Queue` is directly usable once constructed.
pub struct Queue {
    /// The shared scratch store. `Arc<Mutex<_>>` because the connection is
    /// `!Sync` and is shared with the session store and other scratch consumers
    /// of the same source.
    scratch: Arc<Mutex<ScratchStore>>,
    /// This worker's opaque identity, recorded as `claims.claimed_by` so a
    /// claim can be attributed and a foreign worker's live claim rejected.
    worker_identity: String,
    /// The retry schedule.
    policy: RetryPolicy,
    /// Test clock for the retry gate, in unix milliseconds. `None` uses the
    /// wall clock. Production builds do not carry this knob.
    #[cfg(test)]
    now_override_ms: Mutex<Option<i64>>,
}

impl Queue {
    /// Configure a queue over a shared scratch store + retry policy.
    /// `worker_identity` is recorded on every claim this queue acquires.
    pub fn new(
        scratch: Arc<Mutex<ScratchStore>>,
        worker_identity: String,
        policy: RetryPolicy,
    ) -> Self {
        Self {
            scratch,
            worker_identity,
            policy,
            #[cfg(test)]
            now_override_ms: Mutex::new(None),
        }
    }

    /// Pin the clock used for the retry `not_before` gate. Lease bookkeeping
    /// stays on the wall clock.
    #[cfg(test)]
    fn set_now_ms(&self, now_ms: i64) {
        *self
            .now_override_ms
            .lock()
            .expect("queue test clock mutex is never poisoned") = Some(now_ms);
    }

    /// Unix milliseconds for the retry gate.
    fn now_ms(&self) -> i64 {
        #[cfg(test)]
        if let Some(now_ms) = *self
            .now_override_ms
            .lock()
            .expect("queue test clock mutex is never poisoned")
        {
            return now_ms;
        }
        wall_clock_unix_ms()
    }

    /// The lock is only ever held over infallible-to-acquire scratch calls and
    /// is never held across an `.await`, so poisoning is unreachable.
    fn locked(&self) -> std::sync::MutexGuard<'_, ScratchStore> {
        self.scratch
            .lock()
            .expect("scratch queue mutex is never poisoned: guarded sections cannot panic")
    }

    /// Drop every lease claim — restart recovery: a fresh process holds no
    /// leases, so surviving claim rows are stale crash artifacts.
    pub async fn clear_all_leases(&self) -> Result<u64, QueueError> {
        let cleared = self
            .locked()
            .clear_all_job_claims()
            .map_err(QueueError::Scratch)?;
        Ok(cleared as u64)
    }

    /// Enqueue a package for indexing at the default priority (`0`). Idempotent
    /// on `package`: an existing job for the same package is a no-op
    /// (returns its id). Thin wrapper over
    /// [`Queue::enqueue_with_priority`].
    pub async fn enqueue(&self, package: PackageId) -> Result<JobId, QueueError> {
        self.enqueue_with_priority(package, Priority::default())
            .await
    }

    /// Enqueue a package for indexing at an explicit scheduling [`Priority`].
    /// Higher priorities dequeue first (see [`Queue::dequeue_batch`]).
    /// Idempotent on `package`: because the scratch `job_key` is the
    /// package uuid, a second enqueue hits the primary-key and is a silent
    /// no-op that preserves the original job (and thus its priority) — a
    /// re-enqueue never reprioritizes a job already claimed by a worker.
    pub async fn enqueue_with_priority(
        &self,
        package: PackageId,
        priority: Priority,
    ) -> Result<JobId, QueueError> {
        let id = JobId(*package.as_uuid());
        let now = now_unix();
        let payload = JobPayload {
            package: *package.as_uuid(),
            priority: priority.get(),
            state_token: "unindexed".to_owned(),
            not_before: None,
        };
        let payload_json = serde_json::to_string(&payload).map_err(QueueError::Codec)?;
        let row = JobRow {
            job_key: id.job_key(),
            kind: INDEX_JOB_KIND.to_owned(),
            state: JobState::Queued,
            attempts: 0,
            enqueued_at: now,
            updated_at: now,
            payload: Some(payload_json),
        };
        let store = self.locked();
        match store.enqueue_job(&row) {
            Ok(()) => Ok(id),
            // Primary-key collision → a job for this package already exists.
            //
            // "Exists" is not the same as "live", and conflating the two made
            // a failed package permanently unindexable. `job_key` is the
            // package uuid, so a job that reached `Failed` (retries exhausted,
            // or a non-retryable error) keeps that key forever; `dequeue_batch`
            // only scans `Queued`, so nothing ever ran it again, and every
            // later `enqueue` no-oped on this very branch. The catalog
            // meanwhile still said `Unindexed { needed: true }`, so the two
            // planes disagreed with no path back.
            //
            // Observed 2026-08-16 on macOS: `rust/ryu@1.0.18` had a `failed`
            // job row from the previous day and sat at
            // `Unindexed { needed: true }` across repeated `POST /packages`
            // and a full server restart, never compiling.
            //
            // So a *terminal* job (`Done`/`Failed`) is returned to `Queued`
            // with a fresh attempt budget, which is what a caller asking to
            // enqueue it again plainly means. A *live* job (`Queued`,
            // `Claimed`, `Running`) is still preserved untouched — the
            // idempotence that keeps a re-enqueue from disturbing a job a
            // worker already holds is the half worth keeping, and
            // `requeue_terminal` re-checks the state in its own `WHERE` so a
            // job that got claimed between our read and our write is not
            // stolen.
            Err(error) if error.is_unique_violation() => {
                store
                    .requeue_terminal_job(&id.job_key(), now)
                    .map_err(QueueError::Scratch)?;
                Ok(id)
            }
            Err(other) => Err(QueueError::Scratch(other)),
        }
    }

    /// Claim up to `limit` runnable jobs, leasing each for `lease`.
    ///
    /// Scans `Queued` jobs in priority-then-FIFO order and atomically claims
    /// each via the `claims` table's insert-if-absent-or-expired protocol,
    /// so concurrent workers claim disjoint sets without double-leasing.
    /// Jobs whose payload `not_before` is still in the future are skipped.
    /// Each claimed job's `attempts` is bumped, its `not_before` cleared,
    /// and its state advanced to `Running`.
    ///
    /// Returns [`LeasedJob`] witnesses — proof that each returned job was
    /// leased by THIS worker *at this dequeue*. That is a compile-time
    /// guard against operating on a job the caller never dequeued; it is
    /// **not** a proof the lease is still held later (leases expire and can
    /// be reclaimed). Pass these witnesses to [`Queue::complete`],
    /// [`Queue::fail`], and [`Queue::renew_lease`], each of which
    /// additionally re-checks the claim at commit time and returns
    /// [`QueueError::LeaseLost`] if the reclaimer got there first.
    pub async fn dequeue_batch(
        &self,
        limit: usize,
        lease: Duration,
    ) -> Result<Vec<LeasedJob>, QueueError> {
        let now = now_unix();
        let lease_seconds = lease.as_secs() as i64;
        let lease_expires_at = now.saturating_add(lease_seconds);
        let lease_until = unix_to_datetime(lease_expires_at);

        let now_ms = self.now_ms();
        let store = self.locked();
        let candidates = queued_candidates(&store, limit, now_ms)?;

        let mut leased = Vec::new();
        for row in candidates {
            if leased.len() >= limit {
                break;
            }
            // Atomically claim: succeeds only if no live claim exists.
            let acquired = store
                .claim_job(&row.job_key, &self.worker_identity, now, lease_expires_at)
                .map_err(QueueError::Scratch)?;
            if !acquired {
                continue;
            }
            // The backoff applied only to the idle queued row. A successful claim
            // starts the attempt, so a later lease expiry must not keep the job
            // parked until the old `not_before`.
            if not_before_of(&row).is_some() {
                let encoded = encode_not_before(row.payload.as_deref(), None)?;
                store
                    .set_job_payload(&row.job_key, Some(&encoded))
                    .map_err(QueueError::Scratch)?;
            }
            // Bump attempts and mark Running in the same critical section.
            store
                .increment_job_attempts(&row.job_key, now)
                .map_err(QueueError::Scratch)?;
            store
                .set_job_state(&row.job_key, JobState::Running, now)
                .map_err(QueueError::Scratch)?;

            // Reflect the just-applied attempt increment on the witness (the fetched
            // `row` still carries the pre-increment count).
            let mut bumped = row.clone();
            bumped.attempts = row.attempts + 1;
            let job = row_to_job(&bumped, Some(lease_until))?;
            leased.push(LeasedJob { job, lease_until });
        }
        Ok(leased)
    }

    /// Extend a claimed job's lease to `now + lease` — the heartbeat a
    /// long-running worker beats periodically so its lease never lapses under a
    /// fast reclaimer.
    ///
    /// Requires a [`LeasedJob`] witness: only a job that THIS worker dequeued
    /// (and that hasn't been settled yet) can have its lease renewed.
    /// Re-checks that this worker still owns the claim; if the reclaimer
    /// already returned the job (the worker was too slow, or paused), this
    /// returns [`QueueError::LeaseLost`] — the signal for the worker to
    /// abandon its now-orphaned run.
    pub async fn renew_lease(&self, leased: &LeasedJob, lease: Duration) -> Result<(), QueueError> {
        let now = now_unix();
        let new_expires = now.saturating_add(lease.as_secs() as i64);
        let key = leased.id().job_key();
        let store = self.locked();
        if !self.owns_claim(&store, &key, now)? {
            return Err(QueueError::LeaseLost {
                package: leased.package(),
            });
        }
        store
            .renew_job_claim(&key, new_expires)
            .map_err(QueueError::Scratch)?;
        Ok(())
    }

    /// Mark a job complete and remove it from the runnable set.
    ///
    /// Consumes the [`LeasedJob`] witness: after `complete` returns the job is
    /// terminal and the witness cannot be reused. The witness proves only that
    /// THIS worker leased the job at dequeue; it does **not** prove the lease
    /// is still held now.
    ///
    /// **The real guard against the expiry/reclaim race is the claim
    /// re-check**: if the lease expired and the reclaimer returned the job
    /// to the runnable set, this returns [`QueueError::LeaseLost`] rather
    /// than silently reporting success — so a worker that lost the race
    /// cannot commit a result it no longer owns. `_state` is the terminal
    /// `Stored` state the caller has already recorded in the global index;
    /// the job row is simply settled to `Done` and its claim released.
    pub async fn complete(
        &self,
        leased: LeasedJob,
        _state: &ResolutionState,
    ) -> Result<(), QueueError> {
        let now = now_unix();
        let key = leased.id().job_key();
        let package = leased.package();
        let store = self.locked();
        if !self.owns_claim(&store, &key, now)? {
            return Err(QueueError::LeaseLost { package });
        }
        store
            .set_job_state(&key, JobState::Done, now)
            .map_err(QueueError::Scratch)?;
        store.release_job_claim(&key).map_err(QueueError::Scratch)?;
        Ok(())
    }

    /// Record a failure and apply the retry policy: either re-arm the job with
    /// backoff (release the claim so it can be re-dequeued) or dead-letter the
    /// poison job. The single place retry-vs-dead-letter is decided.
    ///
    /// Consumes the [`LeasedJob`] witness. As with [`Queue::complete`], the
    /// witness only proves this worker leased the job at dequeue; the claim
    /// re-check is what proves the lease is still live at commit. Returns
    /// [`QueueError::LeaseLost`] if the claim was reclaimed mid-flight.
    pub async fn fail(
        &self,
        leased: LeasedJob,
        kind: FailureKind,
        message: String,
    ) -> Result<RetryDecision, QueueError> {
        let _ = message; // recorded as the Failure payload in the global index by the caller.
        let now = now_unix();
        let now_ms = self.now_ms();
        let key = leased.id().job_key();
        let package = leased.package();
        let attempts = leased.attempts();
        let decision = self.policy.decide(kind, attempts);

        let store = self.locked();
        if !self.owns_claim(&store, &key, now)? {
            return Err(QueueError::LeaseLost { package });
        }
        match decision {
            RetryDecision::Retry { after } => {
                // Back to Queued, but invisible to dequeue until `after` elapses.
                let not_before = now_ms.saturating_add(duration_millis(after));
                let row = store
                    .get_job(&key)
                    .map_err(QueueError::Scratch)?
                    .ok_or(QueueError::NotFound { package })?;
                let encoded = encode_not_before(row.payload.as_deref(), Some(not_before))?;
                store
                    .set_job_payload(&key, Some(&encoded))
                    .map_err(QueueError::Scratch)?;
                store
                    .set_job_state(&key, JobState::Queued, now)
                    .map_err(QueueError::Scratch)?;
            }
            RetryDecision::DeadLetter => {
                store
                    .set_job_state(&key, JobState::Failed, now)
                    .map_err(QueueError::Scratch)?;
            }
        }
        store.release_job_claim(&key).map_err(QueueError::Scratch)?;
        Ok(decision)
    }

    /// Reclaim jobs whose leases have expired (a worker died mid-flight),
    /// returning them to the runnable set. Run periodically by a sweeper.
    ///
    /// Returns the number of jobs reclaimed. Each expired claim is released and
    /// its job (if still `Running`) reset to `Queued` so it is re-dequeuable.
    pub async fn reclaim_expired_leases(&self) -> Result<u64, QueueError> {
        let now = now_unix();
        let store = self.locked();
        let expired = store.expired_claims(now).map_err(QueueError::Scratch)?;
        let mut reclaimed = 0u64;
        for claim in &expired {
            store
                .release_job_claim(&claim.job_key)
                .map_err(QueueError::Scratch)?;
            // Only reset jobs still marked Running (a settled job may have released
            // its own claim after expiry stamping; do not resurrect it).
            if let Some(job) = store.get_job(&claim.job_key).map_err(QueueError::Scratch)?
                && job.state == JobState::Running
            {
                store
                    .set_job_state(&claim.job_key, JobState::Queued, now)
                    .map_err(QueueError::Scratch)?;
            }
            reclaimed += 1;
        }
        Ok(reclaimed)
    }

    /// Count queued jobs, including those waiting on a retry `not_before`.
    ///
    /// Used by the catalog follower driver for backpressure: if the queue depth
    /// exceeds the configured ceiling the driver pauses rather than enqueueing
    /// more work.
    pub async fn pending_count(&self) -> Result<u64, QueueError> {
        let store = self.locked();
        let queued = store
            .next_queued_jobs(i64::MAX)
            .map_err(QueueError::Scratch)?;
        Ok(queued.len() as u64)
    }

    /// Whether this worker currently holds a live (unexpired) claim on `key`.
    /// The scratch-store analog of the postgres `lease_still_held` guard.
    fn owns_claim(&self, store: &ScratchStore, key: &str, now: i64) -> Result<bool, QueueError> {
        let claim = store.get_job_claim(key).map_err(QueueError::Scratch)?;
        Ok(match claim {
            Some(claim) => claim.claimed_by == self.worker_identity && claim.lease_expires_at > now,
            None => false,
        })
    }
}

/// Queued jobs that are runnable at `now_ms`, in priority-then-FIFO order.
///
/// Scratch orders only by `enqueued_at` and does not know about `not_before`,
/// so a window packed with deferred jobs is re-read in full before the
/// in-process filter — otherwise a later runnable job would stay stuck behind
/// backoff rows.
fn queued_candidates(
    store: &ScratchStore,
    limit: usize,
    now_ms: i64,
) -> Result<Vec<JobRow>, QueueError> {
    let window = (limit as i64).saturating_mul(4).max(limit as i64);
    let mut candidates = store
        .next_queued_jobs(window)
        .map_err(QueueError::Scratch)?;
    if candidates.len() as i64 == window && candidates.iter().any(|row| deferred(row, now_ms)) {
        candidates = store
            .next_queued_jobs(i64::MAX)
            .map_err(QueueError::Scratch)?;
    }
    candidates.retain(|row| !deferred(row, now_ms));
    candidates.sort_by(|a, b| {
        runnable_order(
            (priority_of(a), a.enqueued_at),
            (priority_of(b), b.enqueued_at),
        )
    });
    Ok(candidates)
}

/// Unix milliseconds on the payload before which dequeue must skip this row.
fn not_before_of(row: &JobRow) -> Option<i64> {
    row.payload
        .as_deref()
        .and_then(|json| serde_json::from_str::<JobPayload>(json).ok())
        .and_then(|payload| payload.not_before)
}

/// Whether `row` is queued but still inside its retry backoff.
fn deferred(row: &JobRow, now_ms: i64) -> bool {
    not_before_of(row).is_some_and(|not_before| not_before > now_ms)
}

/// Rewrite a job payload's `not_before`, preserving every other field.
///
/// `payload_json` is the current blob. `None` means the row has no payload,
/// which cannot name a package and cannot carry a retry gate.
fn encode_not_before(
    payload_json: Option<&str>,
    not_before: Option<i64>,
) -> Result<String, QueueError> {
    let Some(json) = payload_json else {
        return Err(QueueError::NotFound {
            package: codec::package_id_from_uuid(uuid::Uuid::nil()),
        });
    };
    let mut payload: JobPayload = serde_json::from_str(json).map_err(QueueError::Codec)?;
    payload.not_before = not_before;
    serde_json::to_string(&payload).map_err(QueueError::Codec)
}

/// `duration` as unix-millisecond ticks, saturating at [`i64::MAX`].
fn duration_millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Wall-clock milliseconds since the Unix epoch.
fn wall_clock_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Read the priority carried on a scratch job's payload (0 if
/// absent/malformed).
fn priority_of(row: &JobRow) -> Priority {
    row.payload
        .as_deref()
        .and_then(|json| serde_json::from_str::<JobPayload>(json).ok())
        .map(|payload| Priority::new(payload.priority))
        .unwrap_or_default()
}

/// Reassemble a [`Job`] from a scratch `jobs` row.
fn row_to_job(row: &JobRow, lease_until: Option<DateTime<Utc>>) -> Result<Job, QueueError> {
    let payload: JobPayload = match &row.payload {
        Some(json) => serde_json::from_str(json).map_err(QueueError::Codec)?,
        None => {
            // A payload-less row cannot name its package; treat it as unrecoverable
            // rather than fabricate a package identity.
            return Err(QueueError::NotFound {
                package: codec::package_id_from_uuid(uuid::Uuid::nil()),
            });
        }
    };
    let id = JobId::from_job_key(&row.job_key).unwrap_or(JobId(payload.package));
    let package = codec::package_id_from_uuid(payload.package);
    let enqueued_at = unix_to_datetime(row.enqueued_at);
    let state = state_from_discriminant(
        &payload.state_token,
        row.attempts.max(0) as u32,
        enqueued_at,
    );
    Ok(Job {
        id,
        package,
        state,
        attempts: row.attempts.max(0) as u32,
        enqueued_at,
        lease_until,
    })
}

/// Decode a job payload's state discriminant into a [`ResolutionState`].
///
/// Because the payload stores only the discriminant (not the full associated
/// data that the global index carries), we synthesise minimal placeholder
/// payloads for variants that have associated data:
///
/// - `"progressing"` → `Progressing(Phase::Acquiring)` — the most conservative
///   phase; a worker that dequeues such a job will overwrite it immediately.
/// - `"failed"` / `"deadlettered"` → `Failed(_)` / `DeadLettered(_)` with a
///   sentinel [`heart::Failure`] whose `message` names it as a stub. The real
///   failure detail is in the global index; this sentinel is enough for callers
///   to branch on the variant (e.g. to refuse to retry a dead-lettered job).
/// - `"stored"` → `Stored { hash: ContentHash::of_bytes(&[]) }` — a sentinel
///   hash; a stored job is terminal and will never be claimed again.
/// - `"unindexed"` (and any unknown token) → `Unindexed { needed: false }`.
fn state_from_discriminant(token: &str, attempts: u32, at: DateTime<Utc>) -> ResolutionState {
    match token {
        "progressing" => ResolutionState::Progressing(heart::Phase::Acquiring),
        "stored" => ResolutionState::Stored {
            hash: heart::ContentHash::of_bytes(&[]),
        },
        "failed" | "deadlettered" => {
            let failure = heart::Failure {
                attempts,
                phase: heart::Phase::Acquiring,
                message: format!(
                    "[stub] job payload discriminant `{token}`; real failure in global index"
                ),
                cause: None,
                at,
            };
            if token == "deadlettered" {
                ResolutionState::DeadLettered(failure)
            } else {
                ResolutionState::Failed(failure)
            }
        }
        _ => ResolutionState::Unindexed { needed: false },
    }
}

/// Wall-clock seconds since the Unix epoch, for the scratch bookkeeping
/// columns.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Convert a Unix-seconds timestamp into a UTC [`DateTime`], clamping an
/// out-of-range value to the epoch rather than panicking.
fn unix_to_datetime(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).unwrap())
}

/// The total order the dequeue hot path claims runnable jobs in, as a *pure*
/// comparator: **priority descending, then FIFO by `enqueued_at` ascending**.
///
/// `Ordering::Less` means `a` is claimed *before* `b`.
pub fn runnable_order(a: (Priority, i64), b: (Priority, i64)) -> std::cmp::Ordering {
    let (a_prio, a_enq) = a;
    let (b_prio, b_enq) = b;
    // Higher priority first → reverse the natural (ascending) Priority order.
    b_prio.cmp(&a_prio).then(a_enq.cmp(&b_enq))
}

#[cfg(test)]
mod state_discriminant_tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// Every job payload state token decodes to the correct [`ResolutionState`]
    /// variant — in particular `deadlettered` must NOT collapse to `Unindexed`.
    #[test]
    fn all_discriminants_decode_to_correct_variant() {
        let t = now();
        assert!(matches!(
            state_from_discriminant("unindexed", 0, t),
            ResolutionState::Unindexed { .. }
        ));
        assert!(matches!(
            state_from_discriminant("progressing", 1, t),
            ResolutionState::Progressing(_)
        ));
        assert!(matches!(
            state_from_discriminant("stored", 0, t),
            ResolutionState::Stored { .. }
        ));
        assert!(matches!(
            state_from_discriminant("failed", 2, t),
            ResolutionState::Failed(_)
        ));
        // The key regression: `deadlettered` must not silently become Unindexed.
        assert!(matches!(
            state_from_discriminant("deadlettered", 3, t),
            ResolutionState::DeadLettered(_)
        ));
    }

    /// Unknown tokens fall back to `Unindexed` rather than panicking.
    #[test]
    fn unknown_discriminant_falls_back_to_unindexed() {
        let state = state_from_discriminant("bogus_future_variant", 0, now());
        assert!(matches!(state, ResolutionState::Unindexed { .. }));
    }

    /// A dead-lettered stub carries the attempt count from the row.
    #[test]
    fn dead_lettered_stub_carries_attempt_count() {
        let state = state_from_discriminant("deadlettered", 7, now());
        if let ResolutionState::DeadLettered(f) = state {
            assert_eq!(f.attempts, 7);
        } else {
            panic!("expected DeadLettered");
        }
    }
}

#[cfg(test)]
mod runnable_order_tests {
    use super::*;

    /// Higher priority claims first; ties break FIFO by enqueue time.
    #[test]
    fn priority_desc_then_fifo() {
        let low = Priority::new(0);
        let high = Priority::new(10);
        assert_eq!(
            runnable_order((high, 100), (low, 1)),
            std::cmp::Ordering::Less
        );
        assert_eq!(runnable_order((low, 1), (low, 2)), std::cmp::Ordering::Less);
        assert_eq!(
            runnable_order((low, 2), (low, 1)),
            std::cmp::Ordering::Greater
        );
    }
}

#[cfg(test)]
mod queue_scratch_tests {
    use super::*;

    fn queue(identity: &str) -> Queue {
        let scratch = Arc::new(Mutex::new(
            ScratchStore::open_in_memory().expect("in-memory scratch open must not fail"),
        ));
        Queue::new(scratch, identity.to_owned(), policy())
    }

    fn shared(scratch: Arc<Mutex<ScratchStore>>, identity: &str) -> Queue {
        Queue::new(scratch, identity.to_owned(), policy())
    }

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: NonZeroU32::new(3).unwrap(),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_mins(1),
        }
    }

    fn package() -> PackageId {
        codec::package_id_from_uuid(uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn enqueue_is_idempotent_on_package() {
        let q = queue("worker-a");
        let pkg = package();
        let first = q.enqueue(pkg).await.unwrap();
        let second = q.enqueue(pkg).await.unwrap();
        assert_eq!(first, second, "same package yields same job id");
        assert_eq!(q.pending_count().await.unwrap(), 1, "no duplicate row");
    }

    #[tokio::test]
    async fn dequeue_leases_then_complete_settles() {
        let q = queue("worker-a");
        let pkg = package();
        q.enqueue(pkg).await.unwrap();
        let leased = q.dequeue_batch(10, Duration::from_secs(30)).await.unwrap();
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].package(), pkg);
        assert_eq!(leased[0].attempts(), 1, "dequeue bumps attempts");
        // While leased, it is not runnable.
        assert_eq!(q.pending_count().await.unwrap(), 0);
        let job = leased.into_iter().next().unwrap();
        q.complete(job, &ResolutionState::Stored {
            hash: heart::ContentHash::of_bytes(&[]),
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_live_claim_blocks_a_second_worker() {
        let scratch = Arc::new(Mutex::new(ScratchStore::open_in_memory().unwrap()));
        let worker_a = shared(Arc::clone(&scratch), "worker-a");
        let worker_b = shared(Arc::clone(&scratch), "worker-b");
        let pkg = package();
        worker_a.enqueue(pkg).await.unwrap();
        let a = worker_a
            .dequeue_batch(10, Duration::from_mins(5))
            .await
            .unwrap();
        assert_eq!(a.len(), 1, "worker-a claims the job");
        let b = worker_b
            .dequeue_batch(10, Duration::from_mins(5))
            .await
            .unwrap();
        assert!(b.is_empty(), "worker-b cannot claim a live-leased job");
    }

    #[tokio::test]
    async fn fail_retry_returns_job_to_runnable_set() {
        let q = queue("worker-a");
        let pkg = package();
        q.enqueue(pkg).await.unwrap();
        let leased = q.dequeue_batch(10, Duration::from_secs(30)).await.unwrap();
        let job = leased.into_iter().next().unwrap();
        // A retriable failure under the attempt ceiling re-queues.
        let decision = q
            .fail(job, FailureKind::Transient, "boom".to_owned())
            .await
            .unwrap();
        assert!(matches!(decision, RetryDecision::Retry { .. }));
        assert_eq!(q.pending_count().await.unwrap(), 1, "job stayed queued");
    }

    /// A retriable failure stays invisible until `after`, then claims normally.
    #[tokio::test]
    async fn retriable_failure_is_not_dequeued_until_after() {
        let q = queue("worker-a");
        let start_ms = 1_700_000_000_000;
        q.set_now_ms(start_ms);
        let pkg = package();
        q.enqueue(pkg).await.unwrap();
        let leased = q.dequeue_batch(10, Duration::from_secs(30)).await.unwrap();
        let job = leased.into_iter().next().unwrap();
        let decision = q
            .fail(job, FailureKind::Transient, "boom".to_owned())
            .await
            .unwrap();
        let RetryDecision::Retry { after } = decision else {
            panic!("retriable failure must retry, got {decision:?}");
        };
        let after_ms = i64::try_from(after.as_millis()).unwrap();
        assert!(after_ms > 0, "policy backoff is a real delay");

        q.set_now_ms(start_ms + after_ms - 1);
        assert!(
            q.dequeue_batch(10, Duration::from_secs(30))
                .await
                .unwrap()
                .is_empty(),
            "job must not be dequeued before `after`"
        );

        q.set_now_ms(start_ms + after_ms);
        let leased = q.dequeue_batch(10, Duration::from_secs(30)).await.unwrap();
        assert_eq!(leased.len(), 1, "job is dequeued once now >= after");
        assert_eq!(leased[0].package(), pkg);
    }

    #[tokio::test]
    async fn complete_after_reclaim_reports_lease_lost() {
        let q = queue("worker-a");
        let pkg = package();
        q.enqueue(pkg).await.unwrap();
        // Lease for zero seconds so the claim is already expired at reclaim time.
        let leased = q.dequeue_batch(10, Duration::from_secs(0)).await.unwrap();
        let job = leased.into_iter().next().unwrap();
        // The reclaimer returns the job to the runnable set.
        q.reclaim_expired_leases().await.unwrap();
        // Completing the now-orphaned witness must surface LeaseLost.
        let result = q
            .complete(job, &ResolutionState::Stored {
                hash: heart::ContentHash::of_bytes(&[]),
            })
            .await;
        assert!(matches!(result, Err(QueueError::LeaseLost { .. })));
    }
}
