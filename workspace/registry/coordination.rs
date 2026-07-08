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

use crate::{
	error::{IndexError, OutboxError},
	index::GlobalStore,
	schema::{codec, queries},
};

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
/// created_at.
fn row_to_entry(row: &PgRow) -> Result<OutboxEntry, OutboxError> {
	let seq: i64 = row.try_get(0).map_err(OutboxError::Database)?;
	let package_uuid: uuid::Uuid = row.try_get(1).map_err(OutboxError::Database)?;
	let kind_tok: String = row.try_get(3).map_err(OutboxError::Database)?;
	let created_at: DateTime<Utc> = row.try_get(4).map_err(OutboxError::Database)?;

	let kind = codec::sink_kind_from_token(&kind_tok).map_err(codec_to_outbox)?;

	Ok(OutboxEntry {
		id: OutboxSeq(seq),
		package: codec::package_id_from_uuid(package_uuid),
		kind,
		created_at,
	})
}

/// A codec failure while decoding an outbox row is a corrupt-row / internal
/// invariant break, surfaced as a database decode error. (We keep Database for
/// outbox surface; full Codec would be added if we expand OutboxError more.)
fn codec_to_outbox(e: codec::CodecError) -> OutboxError {
	OutboxError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
		std::io::ErrorKind::InvalidData,
		format!("codec: {e}"),
	))))
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
		GlobalStore::set_state_tx(&mut tx, package, &stored)
			.await
			.map_err(index_to_outbox)?;

		// 2. Persist the derived search facets on the same lifecycle row, in the
		//    same txn — the facets analog of the `failure` jsonb write. Runs after
		//    the `set_state` upsert has guaranteed the row exists, and never
		//    disturbs the state/phase/hash columns.
		let (facets_sql, facets_vals) =
			queries::index::set_facets(package, facets).map_err(codec_to_outbox)?;
		sqlx::query_with(&facets_sql, facets_vals)
			.execute(&mut *tx)
			.await
			.map_err(OutboxError::Database)?;

		// 3. Fan out one idempotent intent per sink in the *same* txn.
		let (sql, vals) = queries::outbox::append_all(package, snapshot);
		sqlx::query_with(&sql, vals)
			.execute(&mut *tx)
			.await
			.map_err(OutboxError::Database)?;

		tx.commit().await.map_err(OutboxError::Database)?;
		let _ = index; // pool is shared via `self.pool`; `index` documents the invariant.
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

/// An index error surfacing inside an outbox transaction (the state-transition
/// half of [`Outbox::record_stored`]).
fn index_to_outbox(e: IndexError) -> OutboxError {
	match e {
		IndexError::Database(db) => OutboxError::Database(db),
		IndexError::Codec(c) => OutboxError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
			std::io::ErrorKind::Other,
			format!("codec: {c}"),
		)))),
		other => OutboxError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
			std::io::ErrorKind::Other,
			format!("index: {other}"),
		)))),
	}
}
