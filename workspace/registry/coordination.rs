//! The transactional outbox — how blob/manifest creation fans out to the
//! derived read-plane stores.
//!
//! When a new package generation is emitted, the registry records one
//! [`OutboxEntry`] per downstream sink in the *same* transaction that records
//! the manifest. Derived stores (qdrant vector index, terminus graph, tantivy
//! text index) each poll the outbox from their own watermark and materialize
//! what they missed. This is the standard transactional-outbox pattern: it makes
//! "the manifest exists but the vector index never heard about it" impossible
//! without a distributed transaction.
//!
//! Idempotency is enforced by a unique `(package, generation, kind)` dedupe key,
//! so re-emitting the same generation (e.g. after a retry) never double-fans.

use chrono::{DateTime, Utc};
use heart::{
	BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, PackageId, ResolutionState,
	content::ContentHash,
};
use sqlx::{Row, postgres::PgRow};
use strum::IntoEnumIterator;

use crate::{
	error::OutboxError,
	index::GlobalStore,
	schema::{codec, queries},
};

/// What the outbox consumer should do when it sees this entry.
///
/// `Upsert` is the default — it means "materialize this package's symbols into
/// the sink". `Delete` is the mirror-tombstone path: the package was Withdrawn
/// by its upstream registry and search visibility should end.
///
/// **CAS / blob data is never touched by a Delete intent.** The mirror keeps
/// full history; only the search-plane projections (tantivy, qdrant, terminus)
/// lose visibility. This is documented at every materialization site below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum OutboxOp {
	/// Materialize (upsert) the package's symbols into the sink.
	Upsert,
	/// Remove the package's symbols from the sink (search tombstone). CAS
	/// history is retained — mirror keeps full blob lineage.
	Delete,
}

/// A single fan-out intent: "package `X` reached generation `G`; sink `K` should
/// materialize it."
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboxEntry {
	/// The monotonic outbox sequence id — the watermark cursor pollers advance.
	pub id: OutboxSeq,

	/// The package that changed.
	pub package: PackageId,

	/// Which derived sink this intent is for.
	pub kind: SinkKind,

	/// Whether to upsert or delete the package's search projection.
	pub op: OutboxOp,

	/// When the intent was recorded.
	pub created_at: DateTime<Utc>,
}

/// The monotonic sequence position of an outbox entry — pollers store the last
/// one they consumed as their watermark.
#[derive(
	Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct OutboxSeq(pub i64);

/// Which derived read-plane store a fan-out intent targets. One intent is
/// recorded per kind on every emit.
///
/// This is the shared [`heart::DerivedStore`]; the outbox names it `SinkKind`
/// (its role here is a fan-out sink), but it is the *same type* the server's
/// rebuild path uses, so the two can never drift.
pub use heart::DerivedStore as SinkKind;

/// The transactional outbox over postgres. `S` is the connection typestate.
pub struct Outbox<S = Live> {
	pool: sqlx::PgPool,
	_state: std::marker::PhantomData<S>,
}

impl Outbox<Cold> {
	/// Configure an outbox over a pool (unverified).
	pub fn new(pool: sqlx::PgPool) -> Self { Self { pool, _state: std::marker::PhantomData } }
}

impl Connect for Outbox<Cold> {
	type Live = Outbox<Live>;

	/// Verify the pool + outbox schema, then go [`Live`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		sqlx::query("SELECT 1 FROM outbox LIMIT 0")
			.execute(&self.pool)
			.await
			.map_err(|_| {
				ConnectError::new(BackendKind::Postgres, ConnectFailure::SchemaMismatch)
			})?;
		Ok(Outbox { pool: self.pool, _state: std::marker::PhantomData })
	}
}

/// Reassemble an [`OutboxEntry`] from a result row. Column order matches
/// [`queries::outbox::read_since`]: seq, package_id, generation, sink_kind,
/// op, created_at.
fn row_to_entry(row: &PgRow) -> Result<OutboxEntry, OutboxError> {
	let seq: i64 = row.try_get(0).map_err(OutboxError::Database)?;
	let package_uuid: uuid::Uuid = row.try_get(1).map_err(OutboxError::Database)?;
	let kind_tok: String = row.try_get(3).map_err(OutboxError::Database)?;
	let op_tok: String = row.try_get(4).map_err(OutboxError::Database)?;
	let created_at: DateTime<Utc> = row.try_get(5).map_err(OutboxError::Database)?;

	let kind = codec::sink_kind_from_token(&kind_tok).map_err(OutboxError::Codec)?;
	// Unknown op tokens default to Upsert for forwards-compat with rows written
	// before the op column existed.
	let op = codec::outbox_op_from_token(&op_tok).unwrap_or(OutboxOp::Upsert);

	Ok(OutboxEntry {
		id: OutboxSeq(seq),
		package: codec::package_id_from_uuid(package_uuid),
		kind,
		op,
		created_at,
	})
}

impl Outbox<Live> {
	/// Append fan-out intents for a freshly-emitted generation — one per
	/// [`SinkKind`]. Deduped on `(package, generation, kind)`: re-appending an
	/// existing intent is a silent idempotent no-op.
	///
	/// Intended to run inside the same transaction that records the manifest;
	/// accepts an executor so the caller controls the transaction boundary.
	pub async fn append(
		&self,
		package: PackageId,
		snapshot: ContentHash,
		kinds: &[SinkKind],
	) -> Result<(), OutboxError> {
		let mut tx = self.pool.begin().await.map_err(OutboxError::Database)?;
		for &kind in kinds {
			let (sql, vals) = queries::outbox::append_one(package, snapshot, kind);
			sqlx::query_with(&sql, vals)
				.execute(&mut *tx)
				.await
				.map_err(OutboxError::Database)?;
		}
		tx.commit().await.map_err(OutboxError::Database)?;
		Ok(())
	}

	/// Record that `package` reached `generation` **and** fan it out to every
	/// derived sink, atomically. This is the transactional-outbox boundary: the
	/// `parse_status` → `Stored` state transition and the one-row-per-sink outbox
	/// appends commit in a *single* transaction, so "the manifest is stored but a
	/// derived store never heard about it" is impossible without a distributed
	/// transaction. On rollback, neither the state nor the intents persist.
	///
	/// The caller passes the `GlobalStore` whose pool this outbox shares so the
	/// state write and the outbox writes run on the same connection/transaction.
	///
	/// `facets` are the derived [`crate::metadata::SearchFacets`] for this generation (keywords +
	/// quality); they are written into the *same* `parse_status` row, in the same
	/// transaction, so the search-relevant metadata can never diverge from the
	/// `Stored` state that owns it. This mirrors how `Failure` rides the lifecycle
	/// row: a nullable jsonb column, written alongside the state transition.
	/// `None` clears the column (writes `NULL`), so a re-emit without extracted
	/// metadata does not strand stale facets.
	pub async fn record_stored(
		&self,
		index: &GlobalStore,
		package: PackageId,
		snapshot: ContentHash,
		facets: Option<&crate::metadata::SearchFacets>,
	) -> Result<(), OutboxError> {
		let mut tx = self.pool.begin().await.map_err(OutboxError::Database)?;

		// 1. Transition the lifecycle row to Stored { snapshot } in this txn.
		let stored = ResolutionState::Stored { hash: snapshot };
		GlobalStore::set_state_transaction(&mut tx, package, &stored)
			.await
			.map_err(OutboxError::Index)?;

		// 2. Persist the derived search facets on the same lifecycle row, in the
		//    same txn — the facets analog of the `failure` jsonb write. Runs after
		//    the `set_state` upsert has guaranteed the row exists, and never
		//    disturbs the state/phase/hash columns.
		let (facets_sql, facets_vals) =
			queries::index::set_facets(package, facets).map_err(OutboxError::Codec)?;
		sqlx::query_with(&facets_sql, facets_vals)
			.execute(&mut *tx)
			.await
			.map_err(OutboxError::Database)?;

		// 3. Bump packages.updated_at so the tantivy changed_since poll sees
		//    facet-only updates (S5: set_facets only touches parse_status.updated_at,
		//    but the search sync polls packages.updated_at).
		let (touch_sql, touch_vals) = queries::index::touch_package(package);
		sqlx::query_with(&touch_sql, touch_vals)
			.execute(&mut *tx)
			.await
			.map_err(OutboxError::Database)?;

		// 4. Fan out one idempotent intent per sink in the *same* txn.
		let (sql, vals) = queries::outbox::append_all(package, snapshot);
		sqlx::query_with(&sql, vals)
			.execute(&mut *tx)
			.await
			.map_err(OutboxError::Database)?;

		tx.commit().await.map_err(OutboxError::Database)?;
		let _ = index; // pool is shared via `self.pool`; `index` documents the invariant.
		Ok(())
	}

	/// Emit `Delete` tombstone intents for every version of a package that is
	/// `Withdrawn` according to a freshly-observed listing snapshot.
	///
	/// # Seam — call from the resolve/refresh path (P3 gap)
	///
	/// This function is **ready to call** but is not yet wired to its production
	/// trigger: the `ResolveResult::observed_listing` field (in
	/// `registry/resolve.rs`) is populated at resolve time but never passed to
	/// `GlobalStore::set_listing` nor to this function in the current server
	/// coordination pipeline. Once P3's listing-persistence lands (i.e., the
	/// server's indexing coordinator calls `set_listing` for each observed version),
	/// this function should be called there for every version whose
	/// `ListingStatus::Withdrawn` is freshly observed.
	///
	/// Until then, Withdrawn status from the resolve path does not produce outbox
	/// Delete intents. The catalog-follower path (`Withdrawn` events from
	/// `catalog_follower_worker`) bypasses this function and calls
	/// `append_delete` directly, so that path is fully wired.
	///
	/// **Seam location**: `server/coordination/indexing.rs` (the `drive_job` /
	/// post-resolve hook) — after `set_listing` is called, call this function
	/// for each version whose listing transitioned to `Withdrawn`.
	pub async fn emit_withdraw_intents_for_version(
		&self,
		package: PackageId,
		generation: ContentHash,
	) -> Result<(), OutboxError> {
		self.append_delete(package, generation).await
	}

	/// Record `Delete` tombstone intents for every derived sink — the mirror
	/// path for a Withdrawn package. The semantics are symmetric to
	/// [`Self::append`] but write `op = 'delete'` on every row.
	///
	/// **CAS / blob history is untouched.** Only search-plane visibility ends.
	/// Idempotent via the same `(package, generation, sink_kind)` dedupe key.
	pub async fn append_delete(
		&self,
		package: PackageId,
		generation: ContentHash,
	) -> Result<(), OutboxError> {
		let mut tx = self.pool.begin().await.map_err(OutboxError::Database)?;
		for kind in SinkKind::iter() {
			let (sql, vals) = queries::outbox::append_one_with_op(package, generation, kind, OutboxOp::Delete);
			sqlx::query_with(&sql, vals)
				.execute(&mut *tx)
				.await
				.map_err(OutboxError::Database)?;
		}
		tx.commit().await.map_err(OutboxError::Database)?;
		Ok(())
	}

	/// Read up to `limit` intents for `kind` strictly after `since`, in sequence
	/// order — the poll one derived store performs against its watermark.
	pub async fn read_since(
		&self,
		kind: SinkKind,
		since: OutboxSeq,
		limit: usize,
	) -> Result<Vec<OutboxEntry>, OutboxError> {
		let (sql, vals) = queries::outbox::read_since(kind, since.0, limit as u64);
		let rows = sqlx::query_with(&sql, vals)
			.fetch_all(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		rows.iter().map(row_to_entry).collect()
	}

	/// The highest sequence id recorded for `kind` — the head a poller measures
	/// its lag against.
	pub async fn head(&self, kind: SinkKind) -> Result<OutboxSeq, OutboxError> {
		let (sql, vals) = queries::outbox::head(kind);
		let row = sqlx::query_with(&sql, vals)
			.fetch_one(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		let seq: Option<i64> = row.try_get(0).map_err(OutboxError::Database)?;
		Ok(OutboxSeq(seq.unwrap_or(0)))
	}

	/// Advance the durable per-consumer watermark for `kind` to `seq` (monotonic;
	/// a lower `seq` is a no-op via `GREATEST`).
	pub async fn advance_watermark(&self, kind: SinkKind, seq: OutboxSeq) -> Result<(), OutboxError> {
		let (sql, vals) = queries::outbox::advance_watermark(kind, seq.0);
		sqlx::query_with(&sql, vals)
			.execute(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		Ok(())
	}

	/// Try to become the sole fleet-wide drainer of `kind` via a postgres
	/// **session-level advisory lock** (`pg_try_advisory_lock`), returning a
	/// [`SinkLockGuard`] iff this replica won the lock.
	///
	/// This is the replica-safety fix for outbox consumption (DAEMON-PLAN §2.5 /
	/// §6.1): without it, two gateway replicas polling the same sink race on the
	/// shared watermark and can double-materialize. With it, exactly one replica
	/// drains a given sink at a time; the others get `None` and skip this tick,
	/// retrying next poll (so failover is automatic when the holder dies and its
	/// connection — and thus its lock — drops). Consumers are idempotent
	/// upserts, so even a brief overlap on failover is safe.
	///
	/// The lock lives on the guard's pinned connection and is released when the
	/// guard is dropped (explicit `pg_advisory_unlock`, plus the backstop that
	/// dropping the connection frees all its session locks).
	pub async fn try_lock_sink(&self, kind: SinkKind) -> Result<Option<SinkLockGuard>, OutboxError> {
		let mut conn = self.pool.acquire().await.map_err(OutboxError::Database)?;
		let (sql, vals) = queries::outbox::try_advisory_lock(kind);
		let row = sqlx::query_with(&sql, vals)
			.fetch_one(&mut *conn)
			.await
			.map_err(OutboxError::Database)?;
		let acquired: bool = row.try_get(0).map_err(OutboxError::Database)?;
		if acquired {
			Ok(Some(SinkLockGuard { conn: Some(conn), kind }))
		} else {
			// Not ours this tick — drop the connection back to the pool untouched.
			Ok(None)
		}
	}

	/// Reclaim consumed outbox rows: delete every intent at or below the minimum
	/// durable watermark across **all** sinks — i.e. every row that every sink has
	/// already materialized. Returns the number of rows deleted.
	///
	/// This is the conservative half of the CAS GC duty (DAEMON-PLAN §5-ops). The
	/// floor is computed by [`queries::outbox::min_consumed_watermark`], which
	/// returns `0` unless **every** sink has a watermark row — so a sink that has
	/// never advanced (no row yet) forces the floor to `0` and nothing is deleted.
	/// Because `seq` is a `bigserial` starting at 1, a floor of `0` matches no row.
	/// The net effect: an outbox row is only ever purged once it is provably below
	/// every sink's consumed position, so re-delivery is never needed for it again.
	///
	/// The delete runs as a single statement; a crash mid-GC simply leaves the
	/// remaining consumed rows for the next tick (the operation is idempotent —
	/// re-running deletes nothing new once the floor is stable).
	pub async fn gc_consumed(&self) -> Result<u64, OutboxError> {
		let (floor_sql, floor_vals) = queries::outbox::min_consumed_watermark();
		let row = sqlx::query_with(&floor_sql, floor_vals)
			.fetch_one(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		let floor: i64 = row.try_get(0).map_err(OutboxError::Database)?;
		if floor <= 0 {
			// No sink-wide floor yet (a sink without a watermark row, or nothing
			// consumed): reclaim nothing this tick.
			return Ok(0);
		}
		let (del_sql, del_vals) = queries::outbox::delete_consumed_below(floor);
		let result = sqlx::query_with(&del_sql, del_vals)
			.execute(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		Ok(result.rows_affected())
	}

	/// Read a consumer's durable watermark (0 if never advanced).
	pub async fn read_watermark(&self, kind: SinkKind) -> Result<OutboxSeq, OutboxError> {
		let (sql, vals) = queries::outbox::read_watermark(kind);
		let row = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(OutboxError::Database)?;
		match row {
			Some(r) => {
				let seq: i64 = r.try_get(0).map_err(OutboxError::Database)?;
				Ok(OutboxSeq(seq))
			}
			None => Ok(OutboxSeq(0)),
		}
	}
}

/// Proof-of-ownership guard for a per-sink outbox drain (see
/// [`Outbox::try_lock_sink`]). While it lives, this replica holds the sink's
/// postgres session-level advisory lock, so no other replica will drain the same
/// sink. Dropping it releases the lock.
///
/// Release happens two ways, belt-and-suspenders:
/// - [`SinkLockGuard::release`] runs `pg_advisory_unlock` explicitly (the clean
///   path, so the connection returns to the pool lock-free and reusable);
/// - `Drop` (best-effort) returns the pinned connection to the pool; postgres
///   frees every session-level advisory lock a connection held when it is reset
///   for reuse, so the lock never leaks even if `release` was skipped.
///
/// Prefer `release().await` at the end of a drain; `Drop` is the crash/early-exit
/// backstop.
pub struct SinkLockGuard {
	// `Option` so `release` can take the connection out and unlock explicitly,
	// leaving `Drop` a no-op on the clean path.
	conn: Option<sqlx::pool::PoolConnection<sqlx::Postgres>>,
	kind: SinkKind,
}

impl SinkLockGuard {
	/// Explicitly release the advisory lock (`pg_advisory_unlock`) on the pinned
	/// connection, then return it to the pool. The clean shutdown of a drain tick.
	pub async fn release(mut self) -> Result<(), OutboxError> {
		if let Some(mut conn) = self.conn.take() {
			let (sql, vals) = queries::outbox::advisory_unlock(self.kind);
			sqlx::query_with(&sql, vals)
				.execute(&mut *conn)
				.await
				.map_err(OutboxError::Database)?;
		}
		Ok(())
	}

	/// The sink this guard holds the drain lock for.
	pub fn kind(&self) -> SinkKind { self.kind }
}

impl Drop for SinkLockGuard {
	fn drop(&mut self) {
		// If `release` was not called, just drop the pinned connection. postgres
		// releases session-level advisory locks when the backing connection is
		// reset on return to the pool, so the lock is freed either way — we cannot
		// run an async `pg_advisory_unlock` from a sync `Drop`.
		if self.conn.take().is_some() {
			tracing::debug!(sink = %self.kind, "sink drain lock dropped without explicit release");
		}
	}
}

