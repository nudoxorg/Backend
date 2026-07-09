//! The global index — postgres, the orchestration source of truth.
//!
//! Every parsed package is recorded here with its deterministic [`PackageId`],
//! its current [`heart::ResolutionState`] (which phase it is in, what dependents
//! need it, whether it is stored), and the cross-store links the read plane
//! joins on. This is the relational spine; the object store holds the bytes, the
//! queue holds the work, and this holds the *truth* about what exists and where
//! it sits.
//!
//! Identity is never minted here — it is delegated to heart's deterministic
//! derivers ([`PackageCoordinates::id`], [`SymbolId::derive`]) so the same
//! id is recomputable offline against the same [`TerminusInstance`].

use heart::{
	BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, Probeable, ResolutionState,
	content::ContentHash,
	identity::{EntryUri, SymbolId, PackageId},
	timed_probe,
};
use crate::package::Coordinates as PackageCoordinates;
use sqlx::{Row, postgres::PgRow};

use std::io;

use crate::{
	GlobalPackage,
	error::IndexError,
	schema::{codec, queries},
};

/// The `{org}/{db}` TerminusDB instance every deterministic global id is salted
/// with, so a [`SymbolId`] is recomputable offline from the same instance.
///
/// The wrapped string is validated to the `org/db` shape on construction — an
/// invalid instance token would silently fork the id space.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminusInstance(String);

impl TerminusInstance {
	/// Validate and wrap an `{org}/{db}` instance token. Rejects anything that is
	/// not exactly two non-empty, slash-separated segments.
	pub fn new(token: impl Into<String>) -> Result<Self, IndexError> {
		let token = token.into();
		match token.split('/').collect::<Vec<_>>()[..] {
			[organization, database] if !organization.is_empty() && !database.is_empty() => {
				Ok(Self(token))
			}
			_ => Err(IndexError::InvalidInstance { token }),
		}
	}

	/// The validated `org/db` token, used as the salt for id derivation.
	pub fn token(&self) -> &str { &self.0 }
}

/// Our global store / connective tissue (postgres). `S` is the connection
/// typestate ([`Cold`] until [`Connect::connect`], then [`Live`]).
pub struct GlobalStore<S = Live> {
	/// The connection pool to the postgres instance that owns the global index.
	pool: sqlx::PgPool,

	/// The instance every global symbol id in this store is derived against.
	instance: TerminusInstance,

	_state: std::marker::PhantomData<S>,
}

impl GlobalStore<Cold> {
	/// Configure (but do not yet verify) a global store over a pool + instance.
	pub fn new(pool: sqlx::PgPool, instance: TerminusInstance) -> Self {
		Self { pool, instance, _state: std::marker::PhantomData }
	}
}

impl Connect for GlobalStore<Cold> {
	type Live = GlobalStore<Live>;

	/// Verify the pool is reachable and apply the (sea-query-defined) schema, then
	/// go [`Live`].
	async fn connect(self) -> Result<Self::Live, ConnectError> {
		// Reachability probe.
		sqlx::query("SELECT 1")
			.execute(&self.pool)
			.await
			.map_err(|e| ConnectError::new(BackendKind::Postgres, connect_failure(e)))?;

		// Apply the schema. There is no hand-written SQL: every `CREATE TABLE` and
		// `CREATE INDEX` is a `sea_query` statement rendered to Postgres DDL and
		// executed here. All are `IF NOT EXISTS`, so this is idempotent and safe to
		// run on every boot.
		let mut tx = self
			.pool
			.begin()
			.await
			.map_err(|e| ConnectError::new(BackendKind::Postgres, connect_failure(e)))?;
		for ddl in crate::schema::schema_ddl() {
			sqlx::query(&ddl).execute(&mut *tx).await.map_err(|_| {
				ConnectError::new(BackendKind::Postgres, ConnectFailure::SchemaMismatch)
			})?;
		}
		tx.commit()
			.await
			.map_err(|e| ConnectError::new(BackendKind::Postgres, connect_failure(e)))?;

		Ok(GlobalStore { pool: self.pool, instance: self.instance, _state: std::marker::PhantomData })
	}
}

impl GlobalStore<Live> {
	/// The instance this store salts ids with.
	pub fn instance(&self) -> &TerminusInstance { &self.instance }

	/// The shared connection pool, for cross-module transactional writes (e.g.
	/// the outbox pairing a state transition with fan-out in one transaction, or
	/// restart reconciliation).
	pub(crate) fn pool(&self) -> &sqlx::PgPool { &self.pool }

	/// Mint the deterministic [`PackageId`] for coordinates. Pure delegation to
	/// heart — kept here so callers have one minting choke point.
	pub fn package_id(coordinates: &PackageCoordinates) -> PackageId { coordinates.id() }

	/// Mint the deterministic [`SymbolId`] for an entry, salted with this
	/// store's instance. Delegates to [`EntryUri::symbol_id`].
	pub fn symbol_id(&self, uri: &EntryUri) -> SymbolId {
		uri.symbol_id(self.instance.token())
	}

	/// Upsert a package's global record (identity + state + generation). The
	/// idempotent write the pipeline uses to publish and to advance state.
	///
	/// Two writes — the `packages` identity upsert and the `parse_status`
	/// lifecycle upsert — are wrapped in one transaction so a published package
	/// never has identity without a lifecycle row (or vice versa).
	pub async fn upsert(
		&self,
		package: &GlobalPackage,
	) -> Result<(), IndexError> {
		let (id_sql, id_vals) = queries::index::upsert_package(
			&package.package.coordinates,
			&package.package.toolchain,
		)
		.map_err(codec_to_index)?;
		let (st_sql, st_vals) =
			queries::index::set_state(package.id, &package.state).map_err(codec_to_index)?;

		let mut tx = self.pool.begin().await.map_err(IndexError::BeginTx)?;
		sqlx::query_with(&id_sql, id_vals)
			.execute(&mut *tx)
			.await
			.map_err(IndexError::Database)?;
		sqlx::query_with(&st_sql, st_vals)
			.execute(&mut *tx)
			.await
			.map_err(IndexError::Database)?;
		tx.commit().await.map_err(IndexError::Commit)?;
		Ok(())
	}

	/// Advance a package's lifecycle state (e.g. `Progressing(Compiling)` →
	/// `Stored`). The state machine's only mutator.
	pub async fn set_state(
		&self,
		package: PackageId,
		state: &ResolutionState,
	) -> Result<(), IndexError> {
		let (sql, vals) = queries::index::set_state(package, state).map_err(codec_to_index)?;
		sqlx::query_with(&sql, vals)
			.execute(&self.pool)
			.await
			.map_err(IndexError::Database)?;
		Ok(())
	}

	/// Advance a package's lifecycle state within an existing transaction — the
	/// half of the state+outbox atomic write the outbox pairs with (see
	/// [`crate::coordination::Outbox::record_stored`]).
	pub async fn set_state_tx(
		tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
		package: PackageId,
		state: &ResolutionState,
	) -> Result<(), IndexError> {
		let (sql, vals) = queries::index::set_state(package, state).map_err(codec_to_index)?;
		sqlx::query_with(&sql, vals)
			.execute(&mut **tx)
			.await
			.map_err(IndexError::Database)?;
		Ok(())
	}

	/// Fetch a package's current lifecycle [`ResolutionState`] from `parse_status`.
	pub async fn get_state(
		&self,
		package: PackageId,
	) -> Result<ResolutionState, IndexError> {
		let (sql, vals) = queries::index::get_state(package);
		let row = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(IndexError::Database)?
			.ok_or(IndexError::NotFound { package })?;
		row_to_state(&row).map_err(codec_to_index)
	}

	/// Fetch a package's current global record: the identity half rebuilt (and
	/// re-validated) from the `packages` row, the lifecycle half from
	/// `parse_status`.
	pub async fn get(
		&self,
		package: PackageId,
	) -> Result<GlobalPackage, IndexError> {
		let state = self.get_state(package).await?;

		let (sql, vals) = queries::index::get_package(package);
		let row = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(IndexError::Database)?
			.ok_or(IndexError::NotFound { package })?;
		let package = row_to_package(&row).map_err(codec_to_index)?;
		let facets = row_to_facets(&row).map_err(codec_to_index)?;

		// The `get_package` query left-joins `parse_status`, so its final column
		// (index 10) carries the derived search facets — nullable jsonb, mirroring
		// `failure`. `None` when metadata was never extracted for this generation.
		Ok(GlobalPackage { id: package.id(), package, state, facets })
	}

	/// The recorded snapshot [`ContentHash`] for a package, for freshness
	/// comparison against a freshly-computed hash. `None` unless the package is
	/// `Stored`.
	pub async fn generation(
		&self,
		package: PackageId,
	) -> Result<Option<ContentHash>, IndexError> {
		let (sql, vals) = queries::index::get_generation(package);
		let row = sqlx::query_with(&sql, vals)
			.fetch_optional(&self.pool)
			.await
			.map_err(IndexError::Database)?;
		match row {
			None => Ok(None),
			Some(r) => {
				let bytes: Option<Vec<u8>> = r.try_get(0).map_err(IndexError::Database)?;
				match bytes {
					None => Ok(None),
					Some(b) => codec::generation_from_bytes(&b).map(Some).map_err(codec_to_index),
				}
			}
		}
	}

	/// Upsert a serving-projection symbol row, keyed on its deterministic global
	/// id.
	pub async fn upsert_symbol(
		&self,
		id: SymbolId,
		package: PackageId,
		fq_name: &str,
		kind: heart::SymbolKind,
		generation: ContentHash,
	) -> Result<(), IndexError> {
		let (sql, vals) = queries::index::upsert_symbol(id, package, fq_name, kind, generation);
		sqlx::query_with(&sql, vals)
			.execute(&self.pool)
			.await
			.map_err(IndexError::Database)?;
		Ok(())
	}
}

/// Map a serialization/codec failure into an index error. Now uses the
/// dedicated Codec variant so the original CodecError + its sources chain.
fn codec_to_index(e: codec::CodecError) -> IndexError {
	IndexError::Codec(e)
}

/// Classify a connect-time sqlx error into a [`ConnectFailure`].
fn connect_failure(e: sqlx::Error) -> ConnectFailure {
	match e {
		sqlx::Error::PoolTimedOut => ConnectFailure::Timeout,
		sqlx::Error::Io(_) => ConnectFailure::Unreachable,
		other => ConnectFailure::Other(other.into()),
	}
}

/// Reassemble a [`crate::Package`] from a `packages` result row. The column
/// order matches [`queries::index::get_package`]: id, language, origin_token,
/// name_canonical, name_original, version_canonical, visibility, owner_tenant,
/// owner_kind, toolchain (then the left-joined `parse_status.facets` at index
/// 10, read separately by [`row_to_facets`]).
fn row_to_package(row: &PgRow) -> Result<crate::Package, codec::CodecError> {
	let language: String = row.try_get(1).map_err(row_decode)?;
	let origin_token: String = row.try_get(2).map_err(row_decode)?;
	let name_original: String = row.try_get(4).map_err(row_decode)?;
	let version_canonical: String = row.try_get(5).map_err(row_decode)?;
	let toolchain_json: serde_json::Value = row.try_get(9).map_err(row_decode)?;

	let coordinates = codec::coordinates_from_columns(
		&language,
		&origin_token,
		&name_original,
		&version_canonical,
	)?;
	let toolchain = codec::toolchain_from_json(&toolchain_json)?;
	Ok(crate::Package { coordinates, toolchain })
}

/// Read the (possibly-`NULL`) derived search facets from a [`get_package`]
/// result row. The left join places `parse_status.facets` at column index 10,
/// right after the ten `packages` identity/toolchain columns. Decodes via the
/// codec's nullable-jsonb helper, exactly as `failure` is decoded.
///
/// [`get_package`]: queries::index::get_package
fn row_to_facets(row: &PgRow) -> Result<Option<crate::metadata::SearchFacets>, codec::CodecError> {
	let facets_json: Option<serde_json::Value> = row.try_get(10).map_err(row_decode)?;
	codec::facets_from_json(facets_json.as_ref())
}

/// Reassemble a [`ResolutionState`] from a `parse_status` result row. The row
/// column order matches [`queries::index::get_state`]: state, phase,
/// content_hash, needed, failure.
fn row_to_state(row: &PgRow) -> Result<ResolutionState, codec::CodecError> {
	let state: String = row.try_get(0).map_err(row_decode)?;
	let phase: Option<String> = row.try_get(1).map_err(row_decode)?;
	let content_hash: Option<Vec<u8>> = row.try_get(2).map_err(row_decode)?;
	let needed: bool = row.try_get(3).map_err(row_decode)?;
	let failure: Option<serde_json::Value> = row.try_get(4).map_err(row_decode)?;
	codec::state_from_columns(
		&state,
		phase.as_deref(),
		content_hash.as_deref(),
		needed,
		failure.as_ref(),
	)
}

/// Bridge an sqlx row-decode error into a codec error (sqlx domain) so
/// [`row_to_state`] stays total. Uses SqlxDecode to preserve source.
fn row_decode(e: sqlx::Error) -> codec::CodecError {
	codec::CodecError::SqlxDecode { domain: "parse_status row", source: e }
}

impl Probeable for GlobalStore<Live> {
	fn backend(&self) -> BackendKind {
		BackendKind::Postgres
	}

	async fn probe(&self) -> heart::Probe {
		timed_probe(BackendKind::Postgres, async {
			match self.get_state(PackageId::from_uuid(heart::Guid::nil())).await {
				Ok(_) | Err(IndexError::NotFound { .. }) => None,
				Err(error) => Some(error.to_string()),
			}
		})
		.await
	}
}
