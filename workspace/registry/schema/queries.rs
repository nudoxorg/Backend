//! Typed query builders — real `sea-query` statements bound onto sqlx-postgres.
//!
//! Every builder here returns a `(String, SqlxValues)` pair via
//! [`sea_query_binder::SqlxBinder`], ready to hand to
//! `sqlx::query_with(&sql, values)`. Statement *construction* is concrete and
//! total; the thin async execution/row-mapping glue lives in the store modules
//! (`index`, `queue`, `coordination`, `persist`) and calls into these.
//!
//! Grouped by the store that owns the table:
//! - [`index`] — `packages` + `parse_status` + `symbols`;
//! - [`queue`] — `jobs`, incl. the `FOR UPDATE SKIP LOCKED` dequeue;
//! - [`outbox`] — `outbox` + `sink_watermarks`;
//! - [`persist`] — the restart-reconciliation bulk reset.

use sea_query::{
	Alias, Asterisk, Expr, Query, SelectStatement, SimpleExpr, UpdateStatement,
};
use sea_query_binder::{SqlxBinder, SqlxValues};

use crate::{
	coordination::SinkKind,
	schema::{
		codec::{self, StateColumns},
		Jobs, Outbox, Packages, ParseStatus, SinkWatermarks, Symbols,
	},
};

use heart::{
	ResolutionState, SymbolKind,
	content::ContentHash,
	ecosystem::Toolchain,
	identity::{SymbolId, PackageId},
};
use crate::package::Coordinates as PackageCoordinates;
use strum::IntoEnumIterator;

/// The Postgres flavour every statement is rendered + bound against.
type Pg = sea_query::PostgresQueryBuilder;

const PG: Pg = sea_query::PostgresQueryBuilder;

// ═════════════════════════════════════════════════════════════════════════════
// index — packages + parse_status + symbols
// ═════════════════════════════════════════════════════════════════════════════

/// Query builders over the `packages`, `parse_status`, and `symbols` tables.
pub mod index {
	use super::*;

	/// `INSERT INTO packages (...) VALUES (...) ON CONFLICT (id) DO UPDATE ...
	/// RETURNING id` — the idempotent identity upsert. On a repeated publish it
	/// refreshes the mutable columns (visibility, owner, toolchain, names,
	/// `updated_at`) but never the immutable identity tuple.
	pub fn upsert_package(
		coords: &PackageCoordinates,
		toolchain: &Toolchain,
	) -> Result<(String, SqlxValues), codec::CodecError> {
		let id = codec::package_id_to_uuid(coords.id());
		let ecosystem = codec::ecosystem_token(coords.ecosystem());
		let origin = coords.origin.token().to_string();
		let name_canonical = coords.name.canonical().to_string();
		let name_original = coords.name.original().to_string();
		let version_canonical = coords.version.canonical();
		let toolchain_json = codec::toolchain_to_json(toolchain)?;

		let (sql, values) = Query::insert()
			.into_table(Packages::Table)
			.columns([
				Packages::Id,
				Packages::Language,
				Packages::OriginToken,
				Packages::NameCanonical,
				Packages::NameOriginal,
				Packages::VersionCanonical,
				Packages::Toolchain,
			])
			.values_panic([
				id.into(),
				ecosystem.into(),
				origin.into(),
				name_canonical.into(),
				name_original.into(),
				version_canonical.into(),
				toolchain_json.into(),
			])
			.on_conflict(
				sea_query::OnConflict::column(Packages::Id)
					.update_columns([
						Packages::NameOriginal,
						Packages::Toolchain,
					])
					.value(Packages::UpdatedAt, Expr::current_timestamp())
					.to_owned(),
			)
			.returning_col(Packages::Id)
			.build_sqlx(PG);
		Ok((sql, values))
	}

	/// `INSERT INTO parse_status (...) ON CONFLICT (package_id) DO UPDATE ...` —
	/// serialize a [`ResolutionState`] into its discriminant + phase/hash/failure
	/// columns and upsert the single lifecycle row. The state machine's mutator.
	pub fn set_state(
		package: PackageId,
		state: &ResolutionState,
	) -> Result<(String, SqlxValues), codec::CodecError> {
		let pkg = codec::package_id_to_uuid(package);
		let StateColumns { state, phase, content_hash, attempts, failure, needed } =
			codec::state_to_columns(state)?;

		let phase_val: SimpleExpr = match phase {
			Some(p) => p.into(),
			None => Expr::val(Option::<String>::None).into(),
		};
		let hash_val: SimpleExpr = match content_hash {
			Some(bytes) => bytes.into(),
			None => Expr::val(Option::<Vec<u8>>::None).into(),
		};
		let failure_val: SimpleExpr = match failure {
			Some(json) => json.into(),
			None => Expr::val(Option::<serde_json::Value>::None).into(),
		};

		let (sql, values) = Query::insert()
			.into_table(ParseStatus::Table)
			.columns([
				ParseStatus::PackageId,
				ParseStatus::State,
				ParseStatus::Phase,
				ParseStatus::ContentHash,
				ParseStatus::Attempts,
				ParseStatus::Failure,
				ParseStatus::Needed,
			])
			.values_panic([
				pkg.into(),
				state.into(),
				phase_val,
				hash_val,
				attempts.into(),
				failure_val,
				needed.into(),
			])
			.on_conflict(
				sea_query::OnConflict::column(ParseStatus::PackageId)
					.update_columns([
						ParseStatus::State,
						ParseStatus::Phase,
						ParseStatus::ContentHash,
						ParseStatus::Attempts,
						ParseStatus::Failure,
						ParseStatus::Needed,
					])
					.value(ParseStatus::UpdatedAt, Expr::current_timestamp())
					.to_owned(),
			)
			.build_sqlx(PG);
		Ok((sql, values))
	}

	/// `UPDATE parse_status SET facets = $1, updated_at = now() WHERE package_id
	/// = $2` — persist (or clear) the derived [`crate::metadata::SearchFacets`]
	/// on an existing lifecycle row. The facets analog of the `failure` write,
	/// but a standalone `UPDATE` rather than part of the `set_state` upsert: it
	/// runs in the *same* transaction as the `Stored` transition (see
	/// [`crate::coordination::Outbox::record_stored`]) so state and facets commit
	/// atomically, yet it never disturbs the state/phase/hash columns the
	/// [`set_state`] upsert owns. `facets` is nullable jsonb (mirrors `failure`);
	/// `None` writes `NULL`.
	pub fn set_facets(
		package: PackageId,
		facets: Option<&crate::metadata::SearchFacets>,
	) -> Result<(String, SqlxValues), codec::CodecError> {
		let facets_val: SimpleExpr = match facets {
			Some(f) => codec::facets_to_json(f)?.into(),
			None => Expr::val(Option::<serde_json::Value>::None).into(),
		};
		let (sql, values) = Query::update()
			.table(ParseStatus::Table)
			.value(ParseStatus::Facets, facets_val)
			.value(ParseStatus::UpdatedAt, Expr::current_timestamp())
			.and_where(Expr::col(ParseStatus::PackageId).eq(codec::package_id_to_uuid(package)))
			.build_sqlx(PG);
		Ok((sql, values))
	}

	/// `SELECT state, phase, content_hash, needed, failure FROM parse_status
	/// WHERE package_id = $1` — the columns [`codec::state_from_columns`] reassembles.
	pub fn get_state(package: PackageId) -> (String, SqlxValues) {
		Query::select()
			.columns([
				ParseStatus::State,
				ParseStatus::Phase,
				ParseStatus::ContentHash,
				ParseStatus::Needed,
				ParseStatus::Failure,
			])
			.from(ParseStatus::Table)
			.and_where(Expr::col(ParseStatus::PackageId).eq(codec::package_id_to_uuid(package)))
			.build_sqlx(PG)
	}

	/// `SELECT content_hash FROM parse_status WHERE package_id = $1 AND state =
	/// 'stored'` — the recorded [`Generation`], for freshness comparison. A row in
	/// any non-`stored` state has no generation.
	pub fn get_generation(package: PackageId) -> (String, SqlxValues) {
		Query::select()
			.column(ParseStatus::ContentHash)
			.from(ParseStatus::Table)
			.and_where(Expr::col(ParseStatus::PackageId).eq(codec::package_id_to_uuid(package)))
			.and_where(Expr::col(ParseStatus::State).eq("stored"))
			.build_sqlx(PG)
	}

	/// `SELECT p.<identity+toolchain>, ps.facets FROM packages p LEFT JOIN
	/// parse_status ps ON ps.package_id = p.id WHERE p.id = $1` — the full
	/// identity row for rebuilding a `GlobalPackage`, plus the (possibly-`NULL`)
	/// derived search facets from the lifecycle row. The left join keeps a
	/// package with no `parse_status` row visible (facets simply come back
	/// `NULL`). Facets sits at column index 10 — [`super::super::index`]'s
	/// `row_to_package`/`get` read it positionally.
	pub fn get_package(package: PackageId) -> (String, SqlxValues) {
		Query::select()
			.columns([
				(Packages::Table, Packages::Id),
				(Packages::Table, Packages::Language),
				(Packages::Table, Packages::OriginToken),
				(Packages::Table, Packages::NameCanonical),
				(Packages::Table, Packages::NameOriginal),
				(Packages::Table, Packages::VersionCanonical),
				(Packages::Table, Packages::Visibility),
				(Packages::Table, Packages::OwnerTenant),
				(Packages::Table, Packages::OwnerKind),
				(Packages::Table, Packages::Toolchain),
			])
			.column((ParseStatus::Table, ParseStatus::Facets))
			.from(Packages::Table)
			.left_join(
				ParseStatus::Table,
				Expr::col((Packages::Table, Packages::Id))
					.equals((ParseStatus::Table, ParseStatus::PackageId)),
			)
			.and_where(Expr::col((Packages::Table, Packages::Id)).eq(codec::package_id_to_uuid(package)))
			.build_sqlx(PG)
	}

	/// `SELECT package_id FROM parse_status WHERE state = 'progressing'` — the
	/// packages a restart must reconcile (see [`super::persist`]).
	pub fn select_progressing() -> (String, SqlxValues) {
		Query::select()
			.column(ParseStatus::PackageId)
			.from(ParseStatus::Table)
			.and_where(Expr::col(ParseStatus::State).eq("progressing"))
			.build_sqlx(PG)
	}

	/// `INSERT INTO symbols (...) ON CONFLICT (id) DO UPDATE ...` — upsert a
	/// serving-projection symbol row, keyed on its deterministic global id.
	pub fn upsert_symbol(
		id: SymbolId,
		package: PackageId,
		fq_name: &str,
		kind: SymbolKind,
		generation: ContentHash,
	) -> (String, SqlxValues) {
		Query::insert()
			.into_table(Symbols::Table)
			.columns([
				Symbols::Id,
				Symbols::PackageId,
				Symbols::FqName,
				Symbols::Kind,
				Symbols::Generation,
			])
			.values_panic([
				codec::symbol_id_to_uuid(id).into(),
				codec::package_id_to_uuid(package).into(),
				fq_name.into(),
				kind.to_string().into(),
				codec::generation_to_bytes(generation).into(),
			])
			.on_conflict(
				sea_query::OnConflict::column(Symbols::Id)
					.update_columns([
						Symbols::PackageId,
						Symbols::FqName,
						Symbols::Kind,
						Symbols::Generation,
					])
					.to_owned(),
			)
			.build_sqlx(PG)
	}

	/// `SELECT s.id, s.package_id, s.fq_name, s.kind, p.language FROM symbols s
	/// JOIN packages p ON p.id = s.package_id WHERE s.package_id = $1` — read a
	/// single package's serving-projection symbols back out.
	///
	/// The symmetric read of [`upsert_symbol`]: it returns exactly the columns
	/// upsert wrote (id, package, fq_name, kind), joined with the owning
	/// package's `language` so a full [`heart::Symbol`] (which carries the
	/// ecosystem) can be rebuilt. Same shape as the text poller's `SYMBOLS_SQL`,
	/// scoped to one package rather than a batch.
	pub fn symbols_for(package: PackageId) -> (String, SqlxValues) {
		Query::select()
			.columns([
				(Symbols::Table, Symbols::Id),
				(Symbols::Table, Symbols::PackageId),
				(Symbols::Table, Symbols::FqName),
				(Symbols::Table, Symbols::Kind),
			])
			.column((Packages::Table, Packages::Language))
			.from(Symbols::Table)
			.inner_join(
				Packages::Table,
				Expr::col((Packages::Table, Packages::Id))
					.equals((Symbols::Table, Symbols::PackageId)),
			)
			.and_where(
				Expr::col((Symbols::Table, Symbols::PackageId))
					.eq(codec::package_id_to_uuid(package)),
			)
			.build_sqlx(PG)
	}
}

// ═════════════════════════════════════════════════════════════════════════════
// queue — jobs, including FOR UPDATE SKIP LOCKED dequeue
// ═════════════════════════════════════════════════════════════════════════════

/// Query builders over the `jobs` table.
pub mod queue {
	use super::*;
	use chrono::{DateTime, Utc};

	/// `INSERT INTO jobs (package_id, state, ...) ON CONFLICT (package_id) DO
	/// NOTHING RETURNING id` — idempotent enqueue; the unique `package_id`
	/// constraint makes a second enqueue of a live package a no-op.
	pub fn enqueue(package: PackageId, priority: i32) -> (String, SqlxValues) {
		Query::insert()
			.into_table(Jobs::Table)
			.columns([Jobs::PackageId, Jobs::State, Jobs::Priority])
			.values_panic([
				codec::package_id_to_uuid(package).into(),
				"unindexed".into(),
				priority.into(),
			])
			.on_conflict(sea_query::OnConflict::column(Jobs::PackageId).do_nothing().to_owned())
			.returning_col(Jobs::Id)
			.build_sqlx(PG)
	}

	/// The dequeue hot path, as a single atomic statement:
	///
	/// ```sql
	/// UPDATE jobs SET lease_until = $lease, attempts = attempts + 1
	/// WHERE id IN (
	///     SELECT id FROM jobs
	///     WHERE lease_until IS NULL OR lease_until < now()
	///     ORDER BY priority DESC, enqueued_at ASC
	///     LIMIT $limit
	///     FOR UPDATE SKIP LOCKED
	/// )
	/// RETURNING id, package_id, state, attempts, enqueued_at, lease_until;
	/// ```
	///
	/// The inner `SELECT ... FOR UPDATE SKIP LOCKED LIMIT n` claims a disjoint set
	/// of runnable rows per worker without blocking; the outer `UPDATE` stamps the
	/// lease and bumps `attempts` in the same statement, and `RETURNING` hands
	/// back the freshly-claimed jobs.
	pub fn dequeue_batch(limit: u64, lease_until: DateTime<Utc>) -> (String, SqlxValues) {
		// Inner runnable-selection with row locks.
		let mut inner = SelectStatement::new();
		inner
			.column(Jobs::Id)
			.from(Jobs::Table)
			.cond_where(
				sea_query::Cond::any()
					.add(Expr::col(Jobs::LeaseUntil).is_null())
					.add(Expr::col(Jobs::LeaseUntil).lt(Expr::current_timestamp())),
			)
			.order_by(Jobs::Priority, sea_query::Order::Desc)
			.order_by(Jobs::EnqueuedAt, sea_query::Order::Asc)
			.limit(limit)
			.lock_with_behavior(sea_query::LockType::Update, sea_query::LockBehavior::SkipLocked);

		Query::update()
			.table(Jobs::Table)
			.value(Jobs::LeaseUntil, lease_until)
			.value(Jobs::Attempts, Expr::col(Jobs::Attempts).add(1))
			.and_where(Expr::col(Jobs::Id).in_subquery(inner))
			.returning(
				Query::returning().columns([
					Jobs::Id,
					Jobs::PackageId,
					Jobs::State,
					Jobs::Attempts,
					Jobs::EnqueuedAt,
					Jobs::LeaseUntil,
				]),
			)
			.build_sqlx(PG)
	}

	/// A predicate asserting the job still holds a live, unexpired lease — the
	/// `lease_until IS NOT NULL AND lease_until > now()` guard shared by
	/// `complete`/`fail`. An empty `RETURNING` under this guard means the lease
	/// already expired and was (or could be) reclaimed → `LeaseLost`.
	fn lease_still_held() -> sea_query::SimpleExpr {
		Expr::col(Jobs::LeaseUntil)
			.is_not_null()
			.and(Expr::col(Jobs::LeaseUntil).gt(Expr::current_timestamp()))
	}

	/// `DELETE FROM jobs WHERE id = $1 AND <lease still held> RETURNING id` —
	/// settle a completed job, but only if the worker's lease is still valid.
	pub fn complete(job_id: i64) -> (String, SqlxValues) {
		Query::delete()
			.from_table(Jobs::Table)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.and_where(lease_still_held())
			.returning_col(Jobs::Id)
			.build_sqlx(PG)
	}

	/// `UPDATE jobs SET lease_until = $next, state = 'failed' WHERE id = $1 AND
	/// <lease still held> RETURNING id` — the *retry* branch: re-arm the lease so
	/// the row becomes runnable again after the policy backoff, guarding on the
	/// live lease so a reclaimed job cannot be resurrected.
	pub fn fail_retry(job_id: i64, next_visible_at: DateTime<Utc>) -> (String, SqlxValues) {
		Query::update()
			.table(Jobs::Table)
			.value(Jobs::State, "failed")
			.value(Jobs::LeaseUntil, next_visible_at)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.and_where(lease_still_held())
			.returning_col(Jobs::Id)
			.build_sqlx(PG)
	}

	/// `DELETE FROM jobs WHERE id = $1 AND <lease still held> RETURNING id` — the
	/// *dead-letter* branch: remove the poison job from the runnable set entirely
	/// (its terminal `DeadLettered` state is recorded in `parse_status`, not here).
	pub fn fail_deadletter(job_id: i64) -> (String, SqlxValues) {
		Query::delete()
			.from_table(Jobs::Table)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.and_where(lease_still_held())
			.returning_col(Jobs::Id)
			.build_sqlx(PG)
	}

	/// `SELECT attempts FROM jobs WHERE id = $1` — read the attempt count the
	/// retry policy branches on, before deciding retry-vs-dead-letter.
	pub fn get_attempts(job_id: i64) -> (String, SqlxValues) {
		Query::select()
			.column(Jobs::Attempts)
			.from(Jobs::Table)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.build_sqlx(PG)
	}

	/// `SELECT id FROM jobs WHERE package_id = $1` — recover a live job's id after
	/// an idempotent-enqueue conflict.
	pub fn get_job_id_for_package(package: PackageId) -> (String, SqlxValues) {
		Query::select()
			.column(Jobs::Id)
			.from(Jobs::Table)
			.and_where(Expr::col(Jobs::PackageId).eq(codec::package_id_to_uuid(package)))
			.build_sqlx(PG)
	}

	/// `SELECT package_id FROM jobs WHERE id = $1` — the package a job targets, for
	/// error provenance.
	pub fn get_package_for_job(job_id: i64) -> (String, SqlxValues) {
		Query::select()
			.column(Jobs::PackageId)
			.from(Jobs::Table)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.build_sqlx(PG)
	}

	/// `UPDATE jobs SET lease_until = $next WHERE id = $1 AND <lease still held>
	/// RETURNING id` — the lease heartbeat: push a live claim's deadline forward
	/// without touching `attempts` or `state`. Guarded on the lease still being
	/// held so a worker whose lease already lapsed and was reclaimed cannot
	/// resurrect its claim; an empty `RETURNING` is `LeaseLost`.
	pub fn renew_lease(job_id: i64, next_lease_until: DateTime<Utc>) -> (String, SqlxValues) {
		Query::update()
			.table(Jobs::Table)
			.value(Jobs::LeaseUntil, next_lease_until)
			.and_where(Expr::col(Jobs::Id).eq(job_id))
			.and_where(lease_still_held())
			.returning_col(Jobs::Id)
			.build_sqlx(PG)
	}

	/// `UPDATE jobs SET lease_until = NULL WHERE lease_until < now()` — return
	/// jobs whose owning worker died mid-flight to the runnable set. Run by the
	/// periodic sweeper.
	pub fn reclaim_expired_leases() -> (String, SqlxValues) {
		Query::update()
			.table(Jobs::Table)
			.value(Jobs::LeaseUntil, Option::<DateTime<Utc>>::None)
			.and_where(Expr::col(Jobs::LeaseUntil).lt(Expr::current_timestamp()))
			.build_sqlx(PG)
	}
}

// ═════════════════════════════════════════════════════════════════════════════
// outbox — outbox + sink_watermarks
// ═════════════════════════════════════════════════════════════════════════════

/// Query builders over the `outbox` and `sink_watermarks` tables.
pub mod outbox {
	use super::*;

	/// `INSERT INTO outbox (package_id, generation, sink_kind) VALUES ... ON
	/// CONFLICT (package_id, generation, sink_kind) DO NOTHING` — append one
	/// idempotent fan-out intent. The unique dedupe index makes a re-emit of the
	/// same generation a silent no-op.
	///
	/// Meant to run **inside the same transaction** as the `parse_status` →
	/// `Stored` transition (see `coordination::Outbox::record_stored`), so
	/// "manifest committed but the vector index never heard about it" is
	/// impossible without a distributed transaction.
	pub fn append_one(
		package: PackageId,
		generation: ContentHash,
		kind: SinkKind,
	) -> (String, SqlxValues) {
		Query::insert()
			.into_table(Outbox::Table)
			.columns([Outbox::PackageId, Outbox::Generation, Outbox::SinkKind])
			.values_panic([
				codec::package_id_to_uuid(package).into(),
				codec::generation_to_bytes(generation).into(),
				codec::sink_kind_token(kind).into(),
			])
			.on_conflict(
				sea_query::OnConflict::columns([
					Outbox::PackageId,
					Outbox::Generation,
					Outbox::SinkKind,
				])
				.do_nothing()
				.to_owned(),
			)
			.build_sqlx(PG)
	}

	/// Append a fan-out intent for *every* [`SinkKind`] in one multi-row insert,
	/// deduped per `(package, generation, kind)`.
	pub fn append_all(package: PackageId, generation: ContentHash) -> (String, SqlxValues) {
		let pkg = codec::package_id_to_uuid(package);
		let gen_bytes = codec::generation_to_bytes(generation);

		let mut stmt = Query::insert();
		stmt.into_table(Outbox::Table).columns([
			Outbox::PackageId,
			Outbox::Generation,
			Outbox::SinkKind,
		]);
		for kind in SinkKind::iter() {
			stmt.values_panic([
				pkg.into(),
				gen_bytes.clone().into(),
				codec::sink_kind_token(kind).into(),
			]);
		}
		stmt.on_conflict(
			sea_query::OnConflict::columns([
				Outbox::PackageId,
				Outbox::Generation,
				Outbox::SinkKind,
			])
			.do_nothing()
			.to_owned(),
		)
		.build_sqlx(PG)
	}

	/// `SELECT seq, package_id, generation, sink_kind, created_at FROM outbox
	/// WHERE sink_kind = $1 AND seq > $2 ORDER BY seq ASC LIMIT $3` — the poll one
	/// derived store performs against its watermark.
	pub fn read_since(kind: SinkKind, after_seq: i64, limit: u64) -> (String, SqlxValues) {
		Query::select()
			.columns([
				Outbox::Seq,
				Outbox::PackageId,
				Outbox::Generation,
				Outbox::SinkKind,
				Outbox::CreatedAt,
			])
			.from(Outbox::Table)
			.and_where(Expr::col(Outbox::SinkKind).eq(codec::sink_kind_token(kind)))
			.and_where(Expr::col(Outbox::Seq).gt(after_seq))
			.order_by(Outbox::Seq, sea_query::Order::Asc)
			.limit(limit)
			.build_sqlx(PG)
	}

	/// `SELECT COALESCE(MAX(seq), 0) FROM outbox WHERE sink_kind = $1` — the head
	/// a poller measures its lag against.
	pub fn head(kind: SinkKind) -> (String, SqlxValues) {
		Query::select()
			.expr(Expr::expr(Expr::col(Outbox::Seq).max()).cast_as(Alias::new("bigint")))
			.from(Outbox::Table)
			.and_where(Expr::col(Outbox::SinkKind).eq(codec::sink_kind_token(kind)))
			.build_sqlx(PG)
	}

	/// `INSERT INTO sink_watermarks (sink_kind, last_seq) VALUES ($1, $2) ON
	/// CONFLICT (sink_kind) DO UPDATE SET last_seq = GREATEST(...), updated_at =
	/// now()` — monotonically advance a consumer's cursor. `GREATEST` makes the
	/// advance idempotent under out-of-order or replayed acks.
	pub fn advance_watermark(kind: SinkKind, seq: i64) -> (String, SqlxValues) {
		Query::insert()
			.into_table(SinkWatermarks::Table)
			.columns([SinkWatermarks::SinkKind, SinkWatermarks::LastSeq])
			.values_panic([codec::sink_kind_token(kind).into(), seq.into()])
			.on_conflict(
				sea_query::OnConflict::column(SinkWatermarks::SinkKind)
					.value(
						SinkWatermarks::LastSeq,
						Expr::cust_with_exprs(
							"GREATEST($1, $2)",
							[
								Expr::col((SinkWatermarks::Table, SinkWatermarks::LastSeq)).into(),
								Expr::val(seq).into(),
							],
						),
					)
					.value(SinkWatermarks::UpdatedAt, Expr::current_timestamp())
					.to_owned(),
			)
			.build_sqlx(PG)
	}

	/// A stable, collision-resistant advisory-lock key for one sink. Two-int form
	/// (`pg_try_advisory_lock(classid, objid)`): a fixed namespace `classid`
	/// scopes these locks away from any other advisory-lock user in the database,
	/// and the sink's iteration index is the `objid`. Using the ordinal (not a
	/// hash) keeps the key set tiny and human-auditable.
	pub const ADVISORY_LOCK_CLASS_OUTBOX_SINK: i32 = 0x0B0B_0001u32 as i32;

	fn sink_lock_objid(kind: SinkKind) -> i32 {
		// Position in the canonical `SinkKind` iteration order — small, stable.
		SinkKind::iter().position(|k| k == kind).unwrap_or(0) as i32
	}

	/// `SELECT pg_try_advisory_lock($class, $objid)` — a **non-blocking** attempt
	/// to claim the session-scoped advisory lock for one sink. Returns a single
	/// `bool` row: `true` iff this session now holds the lock (no other replica is
	/// draining this sink). The lock is held on the *connection* until
	/// [`advisory_unlock`] runs (or the connection drops), so the caller must run
	/// the lock, the drain, and the unlock on one pinned connection.
	pub fn try_advisory_lock(kind: SinkKind) -> (String, SqlxValues) {
		let sql = "SELECT pg_try_advisory_lock($1, $2)".to_string();
		let values = SqlxValues(sea_query::Values(vec![
			sea_query::Value::Int(Some(ADVISORY_LOCK_CLASS_OUTBOX_SINK)),
			sea_query::Value::Int(Some(sink_lock_objid(kind))),
		]));
		(sql, values)
	}

	/// `SELECT pg_advisory_unlock($class, $objid)` — release the per-sink lock
	/// claimed by [`try_advisory_lock`] on this same connection.
	pub fn advisory_unlock(kind: SinkKind) -> (String, SqlxValues) {
		let sql = "SELECT pg_advisory_unlock($1, $2)".to_string();
		let values = SqlxValues(sea_query::Values(vec![
			sea_query::Value::Int(Some(ADVISORY_LOCK_CLASS_OUTBOX_SINK)),
			sea_query::Value::Int(Some(sink_lock_objid(kind))),
		]));
		(sql, values)
	}

	/// `SELECT last_seq FROM sink_watermarks WHERE sink_kind = $1` — read a
	/// consumer's current cursor (0 if never advanced / absent).
	pub fn read_watermark(kind: SinkKind) -> (String, SqlxValues) {
		Query::select()
			.column(SinkWatermarks::LastSeq)
			.from(SinkWatermarks::Table)
			.and_where(Expr::col(SinkWatermarks::SinkKind).eq(codec::sink_kind_token(kind)))
			.build_sqlx(PG)
	}

	/// The number of distinct sinks the fan-out records an intent for — one per
	/// [`SinkKind`]. The GC's safety hinges on this: an outbox row is only
	/// reclaimable once **every** sink has consumed it, so a min-watermark taken
	/// over fewer than this many watermark rows is *not* a safe floor (a sink
	/// that has never advanced has no row yet and must be treated as `0`).
	pub fn sink_count() -> usize {
		SinkKind::iter().count()
	}

	/// The conservative outbox-GC floor: the minimum durable watermark across
	/// **all** sinks, but only once every sink has a watermark row — otherwise the
	/// floor is `0` (a sink that has never advanced could still need any row, so
	/// nothing is reclaimable yet).
	///
	/// ```sql
	/// SELECT CASE WHEN count(*) = $sink_count THEN COALESCE(min(last_seq),0) ELSE 0 END
	/// FROM sink_watermarks
	/// ```
	///
	/// The `CASE` collapses to `0` whenever a sink is missing its row, so a caller
	/// can delete `outbox` rows with `seq <= floor` and never outrun a sink that
	/// has not yet been created. Returns a single `bigint` row.
	pub fn min_consumed_watermark() -> (String, SqlxValues) {
		// count(*) over the watermark table, compared to the sink count; when they
		// match, every sink has acked and min(last_seq) is a true floor.
		let sql = format!(
			"SELECT CASE WHEN count(*) = {n} THEN COALESCE(min(last_seq), 0) ELSE 0 END \
			 FROM sink_watermarks",
			n = sink_count()
		);
		(sql, SqlxValues(sea_query::Values(vec![])))
	}

	/// `DELETE FROM outbox WHERE seq <= $1` — reclaim outbox rows that every sink
	/// has already consumed. The caller passes the floor computed by
	/// [`min_consumed_watermark`]; a floor of `0` deletes nothing (`seq` is a
	/// `bigserial` starting at 1), so an un-acked sink is never outrun.
	pub fn delete_consumed_below(floor: i64) -> (String, SqlxValues) {
		Query::delete()
			.from_table(Outbox::Table)
			.and_where(Expr::col(Outbox::Seq).lte(floor))
			.build_sqlx(PG)
	}

	/// The pure decision the `CASE` in [`min_consumed_watermark`] encodes, factored
	/// out so it can be unit-tested without postgres: the safe GC floor given the
	/// per-sink watermarks that currently have rows.
	///
	/// - Fewer watermark rows than sinks ⇒ some sink has never acked ⇒ floor `0`
	///   (reclaim nothing).
	/// - Otherwise ⇒ the minimum watermark across all sinks (every row at or below
	///   it is consumed by every sink).
	///
	/// `total_sinks` is [`sink_count`]; `watermarks` are the `last_seq` values of
	/// the sinks that have a `sink_watermarks` row.
	pub fn gc_floor(total_sinks: usize, watermarks: &[i64]) -> i64 {
		if watermarks.len() < total_sinks {
			return 0;
		}
		watermarks.iter().copied().min().unwrap_or(0).max(0)
	}
}

// ═════════════════════════════════════════════════════════════════════════════
// search — the tantivy replica's poll of changed packages
// ═════════════════════════════════════════════════════════════════════════════

/// Query builders for the replica-local search sync.
pub mod search {
	use super::*;
	use chrono::{DateTime, Utc};

	/// `SELECT p.<identity+toolchain+updated_at>, ps.<lifecycle> FROM packages p
	/// LEFT JOIN parse_status ps ON ps.package_id = p.id WHERE p.updated_at > $1
	/// ORDER BY p.updated_at ASC LIMIT $2` — the poll the tantivy replica
	/// performs against its watermark. The left join keeps packages with no
	/// lifecycle row visible (they surface as `Unindexed`). The trailing
	/// `ps.facets` column (index 13) carries the derived search facets so the
	/// replica index picks up keywords + quality on every sync.
	pub fn changed_since(after: DateTime<Utc>, limit: u64) -> (String, SqlxValues) {
		Query::select()
			.columns([
				(Packages::Table, Packages::Id),
				(Packages::Table, Packages::Language),
				(Packages::Table, Packages::OriginToken),
				(Packages::Table, Packages::NameCanonical),
				(Packages::Table, Packages::NameOriginal),
				(Packages::Table, Packages::VersionCanonical),
				(Packages::Table, Packages::Toolchain),
				(Packages::Table, Packages::UpdatedAt),
			])
			.columns([
				(ParseStatus::Table, ParseStatus::State),
				(ParseStatus::Table, ParseStatus::Phase),
				(ParseStatus::Table, ParseStatus::ContentHash),
				(ParseStatus::Table, ParseStatus::Needed),
				(ParseStatus::Table, ParseStatus::Failure),
				(ParseStatus::Table, ParseStatus::Facets),
			])
			.from(Packages::Table)
			.left_join(
				ParseStatus::Table,
				Expr::col((Packages::Table, Packages::Id))
					.equals((ParseStatus::Table, ParseStatus::PackageId)),
			)
			.and_where(Expr::col((Packages::Table, Packages::UpdatedAt)).gt(after))
			.order_by((Packages::Table, Packages::UpdatedAt), sea_query::Order::Asc)
			.limit(limit)
			.build_sqlx(PG)
	}
}

// ═════════════════════════════════════════════════════════════════════════════
// persist — restart reconciliation
// ═════════════════════════════════════════════════════════════════════════════

/// Query builders for the restart-reconciliation invariant.
pub mod persist {
	use super::*;

	/// `UPDATE parse_status SET state = 'unindexed', phase = NULL, needed = false,
	/// updated_at = now() WHERE state = 'progressing'` — the restart-reconciliation
	/// invariant: a process that died mid-sync left rows in the transient
	/// `progressing` state with nobody working them, so on boot every such row is
	/// moved back to the clean, re-enqueueable `unindexed` state. Terminal states
	/// (`stored`, `deadlettered`) are untouched.
	pub fn reset_transient_parse_status() -> (String, SqlxValues) {
		build_reset()
	}

	fn build_reset() -> (String, SqlxValues) {
		let mut stmt = UpdateStatement::new();
		stmt.table(ParseStatus::Table)
			.value(ParseStatus::State, "unindexed")
			.value(ParseStatus::Phase, Option::<String>::None)
			.value(ParseStatus::Needed, false)
			.value(ParseStatus::UpdatedAt, Expr::current_timestamp())
			.and_where(Expr::col(ParseStatus::State).eq("progressing"))
			.build_sqlx(PG)
	}

	/// `UPDATE jobs SET lease_until = NULL, attempts = 0 WHERE lease_until IS NOT
	/// NULL` — the companion reset that drops every stale lease so reconciled work
	/// is immediately runnable again after a crash.
	pub fn clear_all_leases() -> (String, SqlxValues) {
		Query::update()
			.table(Jobs::Table)
			.value(Jobs::LeaseUntil, Option::<chrono::DateTime<chrono::Utc>>::None)
			.and_where(Expr::col(Jobs::LeaseUntil).is_not_null())
			.build_sqlx(PG)
	}
}

// Keep the `Asterisk` import meaningful for potential `SELECT *` callers without
// forcing every builder to enumerate columns; referenced here so the import is
// not flagged unused if a future builder needs it.
#[allow(dead_code)]
fn _select_all() -> SelectStatement {
	Query::select().column(Asterisk).from(Packages::Table).take()
}

#[cfg(test)]
mod gc_tests {
	use super::outbox::{gc_floor, sink_count};

	#[test]
	fn floor_is_zero_when_a_sink_has_no_watermark_row() {
		let n = sink_count();
		assert!(n >= 2, "there is more than one derived sink");
		// One sink missing its row: nothing is reclaimable, however high the others.
		let missing_one = vec![100i64; n - 1];
		assert_eq!(gc_floor(n, &missing_one), 0);
		// No rows at all: floor 0.
		assert_eq!(gc_floor(n, &[]), 0);
	}

	#[test]
	fn floor_is_min_watermark_when_every_sink_acked() {
		let n = sink_count();
		let mut wms: Vec<i64> = (0..n as i64).map(|i| 10 + i).collect();
		// The min is the safe floor: every row <= it is consumed by all sinks.
		assert_eq!(gc_floor(n, &wms), 10);
		wms[0] = 3;
		assert_eq!(gc_floor(n, &wms), 3);
	}

	#[test]
	fn floor_never_goes_negative() {
		let n = sink_count();
		let wms = vec![-5i64; n];
		assert_eq!(gc_floor(n, &wms), 0, "a negative watermark clamps to 0");
	}
}
