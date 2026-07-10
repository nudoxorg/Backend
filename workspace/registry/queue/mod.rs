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
/// [`queries::queue::dequeue_batch`]'s `RETURNING`: id, package_id, state,
/// attempts, enqueued_at, lease_until.
fn row_to_job(row: &PgRow) -> Result<Job, QueueError> {
	let id: i64 = row.try_get(0).map_err(QueueError::Database)?;
	let package_uuid: uuid::Uuid = row.try_get(1).map_err(QueueError::Database)?;
	let state_tok: String = row.try_get(2).map_err(QueueError::Database)?;
	let attempts: i32 = row.try_get(3).map_err(QueueError::Database)?;
	let enqueued_at: DateTime<Utc> = row.try_get(4).map_err(QueueError::Database)?;
	let lease_until: Option<DateTime<Utc>> = row.try_get(5).map_err(QueueError::Database)?;

	let package = codec::package_id_from_uuid(package_uuid);
	// The mirrored discriminant is enough to resume; a claimed job is `Unindexed`
	// (freshly enqueued) or being retried, so we carry the discriminant as the
	// re-enqueueable `Unindexed` state unless it is a richer stored/failed form
	// the worker will overwrite on its next transition anyway.
	let state = match state_tok.as_str() {
		"progressing" => ResolutionState::Progressing(heart::Phase::Acquiring),
		_ => ResolutionState::Unindexed { needed: false },
	};

	Ok(Job {
		id: JobId::from_serial(id),
		package,
		state,
		attempts: attempts.max(0) as u32,
		enqueued_at,
		lease_until,
	})
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
	pub async fn dequeue_batch(
		&self,
		limit: usize,
		lease: Duration,
	) -> Result<Vec<Job>, QueueError> {
		let lease_until = Utc::now()
			+ chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::days(1));
		let (sql, vals) = queries::queue::dequeue_batch(limit as u64, lease_until);
		let rows = sqlx::query_with(&sql, vals)
			.fetch_all(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		rows.iter().map(row_to_job).collect()
	}

	/// Extend a claimed job's lease to `now() + lease` — the heartbeat a
	/// long-running worker beats periodically so its lease never lapses under a
	/// fast reclaimer (letting [`crate::queue::Queue`] run a much shorter default
	/// lease than the job deadline).
	///
	/// Guarded on the lease still being held (`lease_until IS NOT NULL AND
	/// lease_until > now()`): if the reclaimer already returned this job to the
	/// runnable set (the worker was too slow, or paused), the update affects no
	/// row and this returns [`QueueError::LeaseLost`]. That is the signal for the
	/// worker to abandon its now-orphaned run rather than keep computing a result
	/// it can no longer commit.
	pub async fn renew_lease(&self, job: JobId, lease: Duration) -> Result<(), QueueError> {
		let lease_until = Utc::now()
			+ chrono::Duration::from_std(lease).unwrap_or_else(|_| chrono::Duration::days(1));
		let (sql, vals) = queries::queue::renew_lease(job.to_serial(), lease_until);
		let renewed = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		match renewed {
			Some(_) => Ok(()),
			// No row updated → the lease had already expired and been reclaimed.
			None => Err(QueueError::LeaseLost { package: job_package(&self.pool, job).await? }),
		}
	}

	/// Mark a job complete and remove it from the runnable set. Fails with
	/// [`QueueError::LeaseLost`] if the lease was already reclaimed (the guarded
	/// delete affected no row). `state` is the terminal `Stored` state the caller
	/// has already recorded in the global index; the queue row is simply settled.
	pub async fn complete(&self, job: JobId, _state: &ResolutionState) -> Result<(), QueueError> {
		let (sql, vals) = queries::queue::complete(job.to_serial());
		let settled = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		match settled {
			Some(_) => Ok(()),
			// No row settled → the lease had expired and been reclaimed.
			None => Err(QueueError::LeaseLost { package: job_package(&self.pool, job).await? }),
		}
	}

	/// Record a failure and apply the retry policy: either re-arm the lease with
	/// backoff (a re-enqueue) or dead-letter the poison job. The single place
	/// retry-vs-dead-letter is decided.
	pub async fn fail(
		&self,
		job: JobId,
		kind: FailureKind,
		message: String,
	) -> Result<RetryDecision, QueueError> {
		let _ = message; // recorded as the Failure payload in parse_status by the caller.

		// Read the current attempt count the policy branches on.
		let (a_sql, a_vals) = queries::queue::get_attempts(job.to_serial());
		let attempts_row = sqlx::query_with(&a_sql, a_vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(QueueError::Database)?;
		let attempts: u32 = match attempts_row {
			Some(r) => {
				let a: i32 = r.try_get(0).map_err(QueueError::Database)?;
				a.max(0) as u32
			}
			None => return Err(QueueError::LeaseLost { package: job_package(&self.pool, job).await? }),
		};

		let decision = self.policy.decide(kind, attempts);
		let affected = match decision {
			RetryDecision::Retry { after } => {
				let next = Utc::now()
					+ chrono::Duration::from_std(after).unwrap_or_else(|_| chrono::Duration::zero());
				let (sql, vals) = queries::queue::fail_retry(job.to_serial(), next);
				sqlx::query_with(&sql, vals)
					.fetch_optional(&self.pool)
					.await
					.map_err(QueueError::Database)?
			}
			RetryDecision::DeadLetter => {
				let (sql, vals) = queries::queue::fail_deadletter(job.to_serial());
				sqlx::query_with(&sql, vals)
					.fetch_optional(&self.pool)
					.await
					.map_err(QueueError::Database)?
			}
		};
		match affected {
			Some(_) => Ok(decision),
			None => Err(QueueError::LeaseLost { package: job_package(&self.pool, job).await? }),
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
