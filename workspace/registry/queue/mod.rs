//! The durable job queue — the poison-pill-safe spine of the pipeline.
//!
//! A postgres-backed work queue using `SELECT ... FOR UPDATE SKIP LOCKED` so
//! many workers dequeue disjoint batches without blocking each other. Every job
//! carries its [`heart::ResolutionState`], its attempt count, and a lease; a
//! failed job is retried *only* if its [`heart::FailureKind`] is retriable *and*
//! it is under the attempt ceiling — otherwise it is dead-lettered. This is what
//! makes a package whose parse crashes or hangs a *bounded* problem rather than
//! a livelock.

use std::{num::NonZeroU32, time::Duration};

use chrono::{DateTime, Utc};
use heart::{
	BackendKind, Cold, Connect, ConnectError, ConnectFailure, FailureKind, Live, PackageId,
	ResolutionState,
};
use sqlx::{Row, postgres::PgRow};

use crate::{
	error::QueueError,
	schema::{codec, queries},
};

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
/// cannot be reused after the job is settled; non-terminal [`Queue::renew_lease`]
/// borrows it so the caller can heartbeat across a run.
///
/// It does **NOT** prove the lease is *still held now*. Leases **expire**, and
/// the reclaimer ([`Queue::reclaim_expired_leases`]) may hand this job to another
/// worker while this witness is still in hand. Holding a `LeasedJob` at
/// `complete()` therefore proves "I leased this at dequeue" — a *temporal*
/// property (still-leased-at-commit) is fundamentally not expressible as a value
/// that outlives the instant it was true. That race is closed at runtime, not by
/// this type: see the `lease_until IS NOT NULL AND lease_until > now()` guard on
/// the `complete`/`fail`/`renew_lease` SQL (`queries::queue::lease_still_held`),
/// which surfaces [`QueueError::LeaseLost`] when it matches no row.
///
/// So the witness and the runtime guard are **complementary, not redundant**:
/// the type rules out mint-forgery at compile time; the SQL guard rules out the
/// expiry/reclaim race at commit time. Neither subsumes the other.
///
/// This mirrors the capability-witness pattern in Phase 4a authz and the sandbox
/// `Job::seal` typestate — but scoped honestly: it makes "settle a job you never
/// leased" unrepresentable, not "settle a job whose lease you already lost".
#[derive(Debug)]
pub struct LeasedJob {
	/// The underlying job row.
	job: Job,
	/// The lease deadline as stamped at dequeue time (informational only). The
	/// DB is the authoritative source — this snapshot lets a caller cheaply
	/// notice a lease that has *already* lapsed locally, but it can go stale:
	/// only the runtime guard on the settling UPDATE proves the lease still held.
	lease_until: DateTime<Utc>,
}

impl LeasedJob {
	/// The underlying job (immutable reference).
	pub fn job(&self) -> &Job { &self.job }

	/// The stable job id.
	pub fn id(&self) -> JobId { self.job.id }

	/// The package this job targets.
	pub fn package(&self) -> PackageId { self.job.package }

	/// The current [`ResolutionState`] as decoded from the row at dequeue time.
	pub fn state(&self) -> &ResolutionState { &self.job.state }

	/// How many times this job has been attempted (includes the current attempt).
	pub fn attempts(&self) -> u32 { self.job.attempts }

	/// The lease deadline as stamped by `dequeue_batch` at dequeue time. This is
	/// a local snapshot, not a live claim: it can lapse (and the row be
	/// reclaimed) without this value changing. Treat it as a cheap hint for
	/// "should I even bother heartbeating?"; the authoritative check is the
	/// runtime lease guard on `complete`/`fail`/`renew_lease`.
	pub fn lease_until(&self) -> DateTime<Utc> { self.lease_until }

	/// Consume the witness and return the inner [`Job`]. Prefer the typed
	/// accessors above; `into_inner` is an escape hatch for callers that need
	/// the whole struct (e.g. to serialise it for observability).
	pub fn into_inner(self) -> Job { self.job }
}

/// The stable identity of a queued job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobId(pub uuid::Uuid);

impl JobId {
	/// Bridge the postgres `bigserial` row id (`jobs.id`, an `i64`) into the
	/// public [`JobId`] UUID by placing it in the low 64 bits. Lossless: the
	/// high 64 bits are always zero, so [`JobId::to_serial`] recovers it exactly.
	fn from_serial(id: i64) -> Self { JobId(uuid::Uuid::from_u128(id as u64 as u128)) }

	/// Recover the postgres `bigserial` row id from a bridged [`JobId`].
	fn to_serial(self) -> i64 { (self.0.as_u128() as u64) as i64 }
}

/// The retry schedule the queue applies when a job fails. Combined with
/// [`FailureKind::is_retriable`] to decide retry-vs-dead-letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
	/// The attempt ceiling; at or beyond this, a job is dead-lettered regardless
	/// of failure kind. `NonZero` so a misconfigured `0` (which would dead-letter
	/// every job on its first failure) is unrepresentable.
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
		let scaled = self.base_backoff.saturating_mul(factor.min(u32::MAX as u64) as u32);
		scaled.min(self.max_backoff)
	}

	/// The decision a failure yields under this policy: retry (with a delay) or
	/// dead-letter. Retries only if the kind is retriable *and* attempts remain.
	pub fn decide(&self, kind: FailureKind, attempts: u32) -> RetryDecision {
		if kind.is_retriable() && attempts < self.max_attempts.get() {
			RetryDecision::Retry { after: self.backoff_for(attempts) }
		} else {
			RetryDecision::DeadLetter
		}
	}
}

/// A job's scheduling priority: higher dequeues first, ties broken FIFO by
/// `enqueued_at` (see [`Queue::dequeue_batch`]). A thin, `Ord` newtype over the
/// `jobs.priority int` column so callers cannot confuse it with an attempt count
/// or a lease. The [`Default`] is `0` — the value every existing caller carries,
/// so priority is a purely additive scheduling hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Priority(i32);

impl Priority {
	/// The default, neutral priority (`0`): normal FIFO scheduling.
	pub const NORMAL: Priority = Priority(0);

	/// Construct an explicit priority. Larger values run earlier.
	pub const fn new(value: i32) -> Self { Priority(value) }

	/// The raw `int` value bound to the `jobs.priority` column.
	pub const fn get(self) -> i32 { self.0 }
}

impl From<i32> for Priority {
	fn from(value: i32) -> Self { Priority(value) }
}

/// What to do with a failed job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
	/// Re-enqueue after the given delay.
	Retry { after: Duration },
	/// Move to the dead-letter state; stop attempting.
	DeadLetter,
}

/// The durable postgres job queue. `S` is the connection typestate.
pub struct Queue<S = Live> {
	pool: sqlx::PgPool,
	policy: RetryPolicy,
	_state: std::marker::PhantomData<S>,
}

impl Queue<Cold> {
	/// Configure a queue over a pool + retry policy (unverified).
	pub fn new(pool: sqlx::PgPool, policy: RetryPolicy) -> Self {
		Self { pool, policy, _state: std::marker::PhantomData }
	}
}

impl Connect for Queue<Cold> {
	type Live = Queue<Live>;

	/// Verify the pool + queue schema, then go [`Live`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		// The `jobs` table is created by the index schema application; assert it exists.
		sqlx::query("SELECT 1 FROM jobs LIMIT 0")
			.execute(&self.pool)
			.await
			.map_err(|_| {
				ConnectError::new(BackendKind::Postgres, ConnectFailure::SchemaMismatch)
			})?;
		Ok(Queue { pool: self.pool, policy: self.policy, _state: std::marker::PhantomData })
	}
}

/// Reassemble a [`Job`] from a claimed `jobs` result row. Column order matches
/// [`queries::queue::dequeue_batch`]'s `RETURNING`:
/// `id, package_id, state, attempts, enqueued_at, lease_until`.
///
/// The `jobs` table stores only the state *discriminant* — the associated data
/// (`phase`, `content_hash`, `failure` payload) live in `parse_status`.  We
/// decode the discriminant faithfully: `deadlettered` produces
/// [`ResolutionState::DeadLettered`] with a sentinel [`heart::Failure`], so
/// callers can branch on the variant without consulting `parse_status` just to
/// learn a job is poisoned.
fn row_to_job(row: &PgRow) -> Result<Job, QueueError> {
	let id: i64 = row.try_get(0).map_err(QueueError::Database)?;
	let package_uuid: uuid::Uuid = row.try_get(1).map_err(QueueError::Database)?;
	let state_tok: String = row.try_get(2).map_err(QueueError::Database)?;
	let attempts: i32 = row.try_get(3).map_err(QueueError::Database)?;
	let enqueued_at: DateTime<Utc> = row.try_get(4).map_err(QueueError::Database)?;
	let lease_until: Option<DateTime<Utc>> = row.try_get(5).map_err(QueueError::Database)?;

	let package = codec::package_id_from_uuid(package_uuid);

	// Decode the state discriminant that the jobs table stores.  The richer
	// associated data (phase, content hash, failure payload) is in parse_status;
	// a worker that needs it will read parse_status directly.  What matters here
	// is that every discriminant round-trips correctly — in particular
	// `deadlettered` must NOT silently collapse to `Unindexed`.
	let state = state_from_discriminant(&state_tok, attempts.max(0) as u32, enqueued_at);

	Ok(Job {
		id: JobId::from_serial(id),
		package,
		state,
		attempts: attempts.max(0) as u32,
		enqueued_at,
		lease_until,
	})
}

/// Decode a jobs-table state discriminant into a [`ResolutionState`].
///
/// Because the `jobs` table stores only the discriminant (not the full
/// associated columns that `parse_status` carries), we synthesise minimal
/// placeholder payloads for variants that have associated data:
///
/// - `"progressing"` → `Progressing(Phase::Acquiring)` — the most conservative
///   phase; a worker that dequeues such a job will overwrite it immediately.
/// - `"failed"` / `"deadlettered"` → `Failed(_)` / `DeadLettered(_)` with a
///   sentinel [`heart::Failure`] whose `message` names it as a stub.  The real
///   failure detail is in `parse_status.failure`; this sentinel is enough for
///   callers to branch on the variant (e.g. to refuse to retry a dead-lettered
///   job) without an extra round-trip.
/// - `"stored"` → `Stored { hash: ContentHash::of_bytes(&[]) }` — a sentinel
///   hash; a stored job is terminal and will never be claimed again.
/// - `"unindexed"` (and any unknown token) → `Unindexed { needed: false }`.
fn state_from_discriminant(
	token: &str,
	attempts: u32,
	at: DateTime<Utc>,
) -> ResolutionState {
	match token {
		"progressing" => ResolutionState::Progressing(heart::Phase::Acquiring),
		"stored" => ResolutionState::Stored {
			hash: heart::ContentHash::of_bytes(&[]),
		},
		"failed" | "deadlettered" => {
			let f = heart::Failure {
				attempts,
				phase: heart::Phase::Acquiring,
				message: format!(
					"[stub] jobs-row discriminant `{token}`; real failure in parse_status"
				),
				cause: None,
				at,
			};
			if token == "deadlettered" {
				ResolutionState::DeadLettered(f)
			} else {
				ResolutionState::Failed(f)
			}
		}
		_ => ResolutionState::Unindexed { needed: false },
	}
}

impl Queue<Live> {
	/// Enqueue a package for indexing at the default priority (`0`). Idempotent on
	/// `package`: an existing non-terminal job for the same package is a no-op
	/// (returns its id). Thin wrapper over [`Queue::enqueue_with_priority`].
	pub async fn enqueue(&self, package: PackageId) -> Result<JobId, QueueError> {
		self.enqueue_with_priority(package, Priority::default()).await
	}

	/// Enqueue a package for indexing at an explicit scheduling [`Priority`].
	/// Higher priorities dequeue first (see [`Queue::dequeue_batch`], which orders
	/// `priority DESC, enqueued_at ASC`). Idempotent on `package`: an existing
	/// non-terminal job is a no-op and its *original* priority is preserved
	/// (`ON CONFLICT DO NOTHING`), so a re-enqueue never silently reprioritizes a
	/// job already claimed by a worker.
	pub async fn enqueue_with_priority(
		&self,
		package: PackageId,
		priority: Priority,
	) -> Result<JobId, QueueError> {
		let (sql, vals) = queries::queue::enqueue(package, priority.get());
		// `ON CONFLICT DO NOTHING RETURNING id` yields a row only on a fresh
		// insert; on a conflict (existing live job) we look the id back up.
		let inserted = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		if let Some(row) = inserted {
			let id: i64 = row.try_get(0).map_err(QueueError::Database)?;
			return Ok(JobId::from_serial(id));
		}
		// Conflict: return the existing job's id.
		let (get_sql, get_vals) = queries::queue::get_job_id_for_package(package);
		let row = sqlx::query_with(&get_sql, get_vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?
			.ok_or(QueueError::NotFound { package })?;
		let id: i64 = row.try_get(0).map_err(QueueError::Database)?;
		Ok(JobId::from_serial(id))
	}

	/// Claim up to `limit` runnable jobs, leasing each for `lease`.
	///
	/// Uses `SELECT ... FOR UPDATE SKIP LOCKED LIMIT $limit` so concurrent
	/// workers claim disjoint sets without contention, stamping `lease_until =
	/// now() + lease` and bumping `attempts`.
	///
	/// Returns [`LeasedJob`] witnesses — proof that each returned job was leased
	/// by THIS worker *at this dequeue*. That is a compile-time guard against
	/// operating on a job the caller never dequeued; it is **not** a proof the
	/// lease is still held later (leases expire and can be reclaimed). Pass these
	/// witnesses to [`Queue::complete`], [`Queue::fail`], and
	/// [`Queue::renew_lease`], each of which additionally guards on the lease
	/// still being live at commit time and returns [`QueueError::LeaseLost`] if
	/// the reclaimer got there first.
	pub async fn dequeue_batch(
		&self,
		limit: usize,
		lease: Duration,
	) -> Result<Vec<LeasedJob>, QueueError> {
		let lease_until = Utc::now()
			+ chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::days(1));
		let (sql, vals) = queries::queue::dequeue_batch(limit as u64, lease_until);
		let rows = sqlx::query_with(&sql, vals)
			.fetch_all(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		rows.iter()
			.map(|row| {
				let job = row_to_job(row)?;
				// The effective lease deadline: the DB stamped `lease_until` on
				// the row, but we computed it locally just above so we can carry
				// it on the witness without an extra SELECT.
				let effective_lease = job.lease_until.unwrap_or(lease_until);
				Ok(LeasedJob { job, lease_until: effective_lease })
			})
			.collect()
	}

	/// Extend a claimed job's lease to `now() + lease` — the heartbeat a
	/// long-running worker beats periodically so its lease never lapses under a
	/// fast reclaimer (letting [`crate::queue::Queue`] run a much shorter default
	/// lease than the job deadline).
	///
	/// Requires a [`LeasedJob`] witness: only a job that THIS worker dequeued
	/// (and that hasn't been settled yet) can have its lease renewed.
	///
	/// Guarded on the lease still being held (`lease_until IS NOT NULL AND
	/// lease_until > now()`): if the reclaimer already returned this job to the
	/// runnable set (the worker was too slow, or paused), the update affects no
	/// row and this returns [`QueueError::LeaseLost`]. That is the signal for the
	/// worker to abandon its now-orphaned run rather than keep computing a result
	/// it can no longer commit.
	pub async fn renew_lease(&self, leased: &LeasedJob, lease: Duration) -> Result<(), QueueError> {
		let lease_until = Utc::now()
			+ chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::days(1));
		let (sql, vals) = queries::queue::renew_lease(leased.id().to_serial(), lease_until);
		let renewed = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		match renewed {
			Some(_) => Ok(()),
			// No row updated → the lease had already expired and been reclaimed.
			None => Err(QueueError::LeaseLost {
				package: job_package(&self.pool, leased.id()).await?,
			}),
		}
	}

	/// Mark a job complete and remove it from the runnable set.
	///
	/// Consumes the [`LeasedJob`] witness: after `complete` returns the job is
	/// terminal and the witness cannot be reused (the type is dropped). The
	/// witness proves only that THIS worker leased the job at dequeue; it does
	/// **not** prove the lease is still held now.
	///
	/// **The real guard against the expiry/reclaim race is the runtime lease
	/// check in the SQL** (`lease_still_held`: `lease_until IS NOT NULL AND
	/// lease_until > now()`): if the lease already expired and the reclaimer
	/// returned the row to the runnable set, the guarded delete affects no row
	/// and this returns [`QueueError::LeaseLost`] rather than silently reporting
	/// success — so a worker that lost the race cannot commit a result it no
	/// longer owns. `state` is the terminal `Stored` state the caller has already
	/// recorded in the global index; the queue row is simply settled.
	pub async fn complete(
		&self,
		leased: LeasedJob,
		_state: &ResolutionState,
	) -> Result<(), QueueError> {
		let job_id = leased.id();
		let package = leased.package();
		let (sql, vals) = queries::queue::complete(job_id.to_serial());
		let settled = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		// `leased` is consumed here regardless of outcome — a completed job
		// cannot be retried even if the DB write fails.
		match settled {
			Some(_) => Ok(()),
			// No row settled → the lease had expired and been reclaimed.
			None => Err(QueueError::LeaseLost {
				package: job_package(&self.pool, job_id)
					.await
					.unwrap_or(package),
			}),
		}
	}

	/// Record a failure and apply the retry policy: either re-arm the lease with
	/// backoff (a re-enqueue) or dead-letter the poison job. The single place
	/// retry-vs-dead-letter is decided.
	///
	/// Consumes the [`LeasedJob`] witness: after `fail` the job is either
	/// re-enqueued (where a new `LeasedJob` will be issued on the next
	/// `dequeue_batch`) or dead-lettered. Either way the current witness is
	/// terminal — it cannot be reused. As with [`Queue::complete`], the witness
	/// only proves this worker leased the job at dequeue; the guarded UPDATE
	/// (`lease_still_held`) is what proves the lease is still live at commit.
	///
	/// Returns [`QueueError::LeaseLost`] if that guard matches no row (the lease
	/// expired and was reclaimed mid-flight), so a lost race surfaces honestly
	/// instead of masquerading as a successful retry/dead-letter.
	pub async fn fail(
		&self,
		leased: LeasedJob,
		kind: FailureKind,
		message: String,
	) -> Result<RetryDecision, QueueError> {
		let _ = message; // recorded as the Failure payload in parse_status by the caller.
		let job_id = leased.id();
		let package = leased.package();
		// The attempt count is already on the witness; use it rather than an
		// extra DB round-trip.
		let attempts = leased.attempts();

		let decision = self.policy.decide(kind, attempts);
		let affected = match decision {
			RetryDecision::Retry { after } => {
				let next = Utc::now()
					+ chrono::Duration::from_std(after).unwrap_or_else(|_| chrono::Duration::zero());
				let (sql, vals) = queries::queue::fail_retry(job_id.to_serial(), next);
				sqlx::query_with(&sql, vals)
					.fetch_optional(&self.pool)
					.await
					.map_err(QueueError::Database)?
			}
			RetryDecision::DeadLetter => {
				let (sql, vals) = queries::queue::fail_deadletter(job_id.to_serial());
				sqlx::query_with(&sql, vals)
					.fetch_optional(&self.pool)
					.await
					.map_err(QueueError::Database)?
			}
		};
		// `leased` is consumed above — after this point the witness is gone
		// regardless of outcome.
		match affected {
			Some(_) => Ok(decision),
			None => Err(QueueError::LeaseLost {
				package: job_package(&self.pool, job_id)
					.await
					.unwrap_or(package),
			}),
		}
	}

	/// Reclaim jobs whose leases have expired (a worker died mid-flight),
	/// returning them to the runnable set. Run periodically by a sweeper.
	pub async fn reclaim_expired_leases(&self) -> Result<u64, QueueError> {
		let (sql, vals) = queries::queue::reclaim_expired_leases();
		let result = sqlx::query_with(&sql, vals)
			.execute(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		Ok(result.rows_affected())
	}
}

/// Look up the [`PackageId`] a bridged job id targets — used only to fill a
/// [`QueueError::LeaseLost`] with meaningful provenance when a guarded write
/// affects no row.
async fn job_package(pool: &sqlx::PgPool, job: JobId) -> Result<PackageId, QueueError> {
	let (sql, vals) = queries::queue::get_package_for_job(job.to_serial());
	let row = sqlx::query_with(&sql, vals)
		.fetch_optional(pool)
		.await
		.map_err(QueueError::Database)?;
	match row {
		Some(r) => {
			let uuid: uuid::Uuid = r.try_get(0).map_err(QueueError::Database)?;
			Ok(codec::package_id_from_uuid(uuid))
		}
		// The row is gone entirely; synthesize a nil-package LeaseLost target.
		None => Ok(codec::package_id_from_uuid(uuid::Uuid::nil())),
	}
}

/// The total order the dequeue hot path claims runnable jobs in, as a *pure*
/// comparator: **priority descending, then FIFO by `enqueued_at` ascending**.
/// This is the Rust mirror of the `ORDER BY priority DESC, enqueued_at ASC`
/// clause inside [`queries::queue::dequeue_batch`]'s `SKIP LOCKED` select — kept
/// here so the ordering invariant is unit-testable without a live postgres (the
/// integration tests exercise the same order end-to-end against pg).
///
/// `Ordering::Less` means `a` is claimed *before* `b`.
pub fn runnable_order(a: (Priority, DateTime<Utc>), b: (Priority, DateTime<Utc>)) -> std::cmp::Ordering {
	let (a_prio, a_enq) = a;
	let (b_prio, b_enq) = b;
	// Higher priority first → reverse the natural (ascending) Priority order.
	b_prio.cmp(&a_prio).then(a_enq.cmp(&b_enq))
}

#[cfg(test)]
mod state_discriminant_tests {
	use super::*;

	fn now() -> DateTime<Utc> { Utc::now() }

	/// Every jobs-row state token decodes to the correct [`ResolutionState`]
	/// variant — in particular `deadlettered` must NOT collapse to `Unindexed`.
	#[test]
	fn all_discriminants_decode_to_correct_variant() {
		let t = now();

		assert!(
			matches!(
				state_from_discriminant("unindexed", 0, t),
				ResolutionState::Unindexed { .. }
			),
			"`unindexed` must decode as Unindexed"
		);
		assert!(
			matches!(
				state_from_discriminant("progressing", 1, t),
				ResolutionState::Progressing(_)
			),
			"`progressing` must decode as Progressing"
		);
		assert!(
			matches!(
				state_from_discriminant("stored", 0, t),
				ResolutionState::Stored { .. }
			),
			"`stored` must decode as Stored"
		);
		assert!(
			matches!(
				state_from_discriminant("failed", 2, t),
				ResolutionState::Failed(_)
			),
			"`failed` must decode as Failed"
		);
		// The key regression: `deadlettered` must not silently become Unindexed.
		assert!(
			matches!(
				state_from_discriminant("deadlettered", 3, t),
				ResolutionState::DeadLettered(_)
			),
			"`deadlettered` must decode as DeadLettered, not Unindexed"
		);
	}

	/// Unknown tokens fall back to `Unindexed` rather than panicking.
	#[test]
	fn unknown_discriminant_falls_back_to_unindexed() {
		let state = state_from_discriminant("bogus_future_variant", 0, now());
		assert!(
			matches!(state, ResolutionState::Unindexed { .. }),
			"unknown discriminants must not panic"
		);
	}

	/// A dead-lettered stub carries the attempt count from the row.
	#[test]
	fn dead_lettered_stub_carries_attempt_count() {
		let t = now();
		let state = state_from_discriminant("deadlettered", 7, t);
		if let ResolutionState::DeadLettered(f) = state {
			assert_eq!(f.attempts, 7, "attempt count must match the row value");
		} else {
			panic!("expected DeadLettered");
		}
	}
}

#[cfg(test)]
mod leased_job_witness_tests {
	use super::*;

	/// Helper: mint a `LeasedJob` as `dequeue_batch` would — the ONLY place this
	/// is valid. This simulates the internal constructor used in production.
	fn make_leased(id_serial: i64, package_uuid: uuid::Uuid, attempts: u32) -> LeasedJob {
		let now = Utc::now();
		let lease_until = now + chrono::Duration::minutes(2);
		let job = Job {
			id: JobId::from_serial(id_serial),
			package: codec::package_id_from_uuid(package_uuid),
			state: ResolutionState::Progressing(heart::Phase::Acquiring),
			attempts,
			enqueued_at: now,
			lease_until: Some(lease_until),
		};
		LeasedJob { job, lease_until }
	}

	/// A `LeasedJob` carries the same id as its inner `Job`.
	#[test]
	fn leased_job_id_matches_inner_job() {
		let uuid = uuid::Uuid::new_v4();
		let leased = make_leased(42, uuid, 1);
		assert_eq!(leased.id(), leased.job().id, "id() must match inner job id");
	}

	/// A `LeasedJob` carries the same package as its inner `Job`.
	#[test]
	fn leased_job_package_matches_inner_job() {
		let uuid = uuid::Uuid::new_v4();
		let leased = make_leased(99, uuid, 3);
		assert_eq!(
			leased.package(),
			leased.job().package,
			"package() must match inner job package"
		);
	}

	/// `attempts()` reflects what was in the row at dequeue time.
	#[test]
	fn leased_job_attempts_reflects_row() {
		let leased = make_leased(1, uuid::Uuid::new_v4(), 5);
		assert_eq!(leased.attempts(), 5);
	}

	/// `into_inner` yields the original `Job` — the escape hatch works.
	#[test]
	fn leased_job_into_inner_recovers_job() {
		let uuid = uuid::Uuid::new_v4();
		let leased = make_leased(7, uuid, 2);
		let job_id = leased.id();
		let inner = leased.into_inner();
		assert_eq!(inner.id, job_id, "into_inner() must yield the same job");
	}

	/// Witness discipline: verify that `complete` and `fail` require a
	/// `LeasedJob` (not a bare `JobId`). This is a compile-time property, but
	/// we document it as a unit test to make the intent explicit. The test body
	/// just confirms the type-level accessors work correctly — a call-site that
	/// passes a bare `JobId` will not compile.
	#[test]
	fn witness_required_for_terminal_ops_is_type_checked() {
		let uuid = uuid::Uuid::new_v4();
		let leased = make_leased(11, uuid, 1);
		// If this compiles, the witness is accessible and correctly typed.
		// The ONLY way to call queue.complete(leased, ...) is with a LeasedJob.
		let _id: JobId = leased.id();
		let _package: heart::PackageId = leased.package();
		let _attempts: u32 = leased.attempts();
		// No bare-JobId path to complete/fail exists — that's the compile-time
		// guarantee. The test above proves the accessors work; the enforcement is
		// in the method signatures (LeasedJob consumed by complete/fail).
	}
}
