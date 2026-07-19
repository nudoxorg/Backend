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
//! identifier is recomputable offline against the same [`TerminusInstance`].

use heart::{
    BackendKind, Cold, Connect, ConnectError, ConnectFailure, Live, Probeable, ResolutionState,
    content::ContentHash,
    identity::{EntryUri, PackageId, SymbolId},
    timed_probe,
};
use std::str::FromStr;

use crate::package::Coordinates as PackageCoordinates;
use sqlx::{Row, postgres::PgRow};

use sea_query_binder::SqlxValues;

use crate::{
    GlobalPackage,
    error::IndexError,
    schema::{codec, queries},
};

/// The `{organization}/{database}` TerminusDB instance every deterministic global
/// identifier is salted with, so a [`SymbolId`] is recomputable offline from the same instance.
///
/// The wrapped string is validated to the `organization/database` shape on construction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TerminusInstance(String);

impl TerminusInstance {
    /// Validate and wrap an `{organization}/{database}` instance token.
    /// Rejects anything that is not exactly two non-empty, slash-separated segments.
    pub fn new(token: impl Into<String>) -> Result<Self, IndexError> {
        let token = token.into();

        match token.split_once('/') {
            Some((organization, database)) if !organization.is_empty() && !database.is_empty() => {
                Ok(Self(token))
            }
            _ => Err(IndexError::InvalidInstance { token }),
        }
    }

    /// The validated `organization/database` token, used as the salt for identifier derivation.
    pub fn token(&self) -> &str {
        &self.0
    }
}

/// Our global store / connective tissue (postgres). `S` is the connection
/// typestate ([`Cold`] until [`Connect::connect`], then [`Live`]).
pub struct GlobalStore<S = Live> {
    /// The connection pool to the postgres instance that owns the global index.
    pool: sqlx::PgPool,

    /// The instance every global symbol identifier in this store is derived against.
    instance: TerminusInstance,

    _state: std::marker::PhantomData<S>,
}

impl GlobalStore<Cold> {
    /// Configure (but do not yet verify) a global store over a pool and instance.
    pub fn new(pool: sqlx::PgPool, instance: TerminusInstance) -> Self {
        Self {
            pool,
            instance,
            _state: std::marker::PhantomData,
        }
    }
}

impl Connect for GlobalStore<Cold> {
    type Live = GlobalStore<Live>;

    /// Verify the pool is reachable and apply the schema, then go [`Live`].
    async fn connect(self) -> Result<Self::Live, ConnectError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|error| ConnectError::new(BackendKind::Postgres, connect_failure(error)))?;

        let mut transaction =
            self.pool.begin().await.map_err(|error| {
                ConnectError::new(BackendKind::Postgres, connect_failure(error))
            })?;

        for schema_statement in crate::schema::schema_ddl() {
            sqlx::query(&schema_statement)
                .execute(&mut *transaction)
                .await
                .map_err(|_| {
                    ConnectError::new(BackendKind::Postgres, ConnectFailure::SchemaMismatch)
                })?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| ConnectError::new(BackendKind::Postgres, connect_failure(error)))?;

        Ok(GlobalStore {
            pool: self.pool,
            instance: self.instance,
            _state: std::marker::PhantomData,
        })
    }
}

impl GlobalStore<Live> {
    /// The instance this store salts identifiers with.
    pub fn instance(&self) -> &TerminusInstance {
        &self.instance
    }

    /// The shared connection pool, for cross-module transactional writes.
    pub(crate) fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }

    /// Mint the deterministic [`PackageId`] for coordinates.
    pub fn package_id(coordinates: &PackageCoordinates) -> PackageId {
        coordinates.id()
    }

    /// Mint the deterministic [`SymbolId`] for an entry, salted with this store's instance.
    pub fn symbol_id(&self, uri: &EntryUri) -> SymbolId {
        uri.symbol_id(self.instance.token())
    }

    /// Upsert a package's global record (identity + state + generation).
    pub async fn upsert(&self, package: &GlobalPackage) -> Result<(), IndexError> {
        let (identity_query, identity_parameters) = queries::index::upsert_package(
            &package.package.coordinates,
            &package.package.toolchain,
        )
        .map_err(convert_codec_error)?;

        let (state_query, state_parameters) =
            queries::index::set_state(package.id, &package.state).map_err(convert_codec_error)?;

        let mut transaction = self.pool.begin().await.map_err(IndexError::BeginTx)?;

        execute_query(&mut transaction, &identity_query, identity_parameters).await?;
        execute_query(&mut transaction, &state_query, state_parameters).await?;

        transaction.commit().await.map_err(IndexError::Commit)?;
        Ok(())
    }

    /// Advance a package's lifecycle state (e.g. `Progressing(Compiling)` → `Stored`).
    pub async fn set_state(
        &self,
        package: PackageId,
        state: &ResolutionState,
    ) -> Result<(), IndexError> {
        let (query, parameters) =
            queries::index::set_state(package, state).map_err(convert_codec_error)?;

        sqlx::query_with(&query, parameters)
            .execute(&self.pool)
            .await
            .map_err(IndexError::Database)?;

        Ok(())
    }

    /// Advance a package's lifecycle state within an existing transaction.
    pub async fn set_state_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        package: PackageId,
        state: &ResolutionState,
    ) -> Result<(), IndexError> {
        let (query, parameters) =
            queries::index::set_state(package, state).map_err(convert_codec_error)?;

        execute_query(transaction, &query, parameters).await
    }

    /// Fetch a package's current lifecycle [`ResolutionState`] from `parse_status`.
    pub async fn get_state(&self, package: PackageId) -> Result<ResolutionState, IndexError> {
        let (query, parameters) = queries::index::get_state(package);

        let row = sqlx::query_with(&query, parameters)
            .fetch_optional(&self.pool)
            .await
            .map_err(IndexError::Database)?
            .ok_or(IndexError::NotFound { package })?;

        row_to_state(&row).map_err(convert_codec_error)
    }

    /// Persist the last-observed registry listing status for a package.
    /// `None` clears the column (status unknown). Follows the same nullable-jsonb
    /// pattern as `set_facets`. The listing status is later read by the resolve
    /// path to filter withdrawn versions from Latest/Constraint resolution.
    pub async fn set_listing(
        &self,
        package: PackageId,
        listing: Option<&ecosystem::upstream::ListingStatus>,
    ) -> Result<(), IndexError> {
        let (query, parameters) =
            queries::index::set_listing(package, listing).map_err(convert_codec_error)?;
        sqlx::query_with(&query, parameters)
            .execute(&self.pool)
            .await
            .map_err(IndexError::Database)?;
        Ok(())
    }

    /// Read the last-observed registry listing status for a package.
    pub async fn get_listing(
        &self,
        package: PackageId,
    ) -> Result<Option<ecosystem::upstream::ListingStatus>, IndexError> {
        let (query, parameters) = queries::index::get_listing(package);
        let row = sqlx::query_with(&query, parameters)
            .fetch_optional(&self.pool)
            .await
            .map_err(IndexError::Database)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let val: Option<serde_json::Value> = row.try_get(0).map_err(|e| IndexError::RowDecode {
            column: "listing",
            source: e,
        })?;
        match val {
            None => Ok(None),
            Some(v) => serde_json::from_value(v)
                .map(Some)
                .map_err(|e| IndexError::Codec(crate::schema::codec::CodecError::Json {
                    domain: "ListingStatus",
                    source: e,
                })),
        }
    }

    /// Fetch a package's current global record.
    pub async fn get(&self, package: PackageId) -> Result<GlobalPackage, IndexError> {
        let state = self.get_state(package).await?;

        let (query, parameters) = queries::index::get_package(package);
        let row = sqlx::query_with(&query, parameters)
            .fetch_optional(&self.pool)
            .await
            .map_err(IndexError::Database)?
            .ok_or(IndexError::NotFound { package })?;

        let package_data = row_to_package(&row).map_err(convert_codec_error)?;
        let facets = row_to_facets(&row).map_err(convert_codec_error)?;

        Ok(GlobalPackage {
            id: package_data.id(),
            package: package_data,
            state,
            facets,
        })
    }

    /// The recorded snapshot [`ContentHash`] for a package.
    pub async fn generation(&self, package: PackageId) -> Result<Option<ContentHash>, IndexError> {
        let (query, parameters) = queries::index::get_generation(package);

        let row_option = sqlx::query_with(&query, parameters)
            .fetch_optional(&self.pool)
            .await
            .map_err(IndexError::Database)?;

        let Some(row) = row_option else {
            return Ok(None);
        };

        let Some(bytes): Option<Vec<u8>> = row.try_get(0).map_err(IndexError::Database)? else {
            return Ok(None);
        };

        codec::generation_from_bytes(&bytes)
            .map(Some)
            .map_err(convert_codec_error)
    }

    /// Upsert a serving-projection symbol row, keyed on its deterministic global identifier.
    pub async fn upsert_symbol(
        &self,
        identifier: SymbolId,
        package: PackageId,
        fully_qualified_name: &str,
        kind: heart::SymbolKind,
        generation: ContentHash,
    ) -> Result<(), IndexError> {
        let (query, parameters) = queries::index::upsert_symbol(
            identifier,
            package,
            fully_qualified_name,
            kind,
            generation,
        );

        sqlx::query_with(&query, parameters)
            .execute(&self.pool)
            .await
            .map_err(IndexError::Database)?;

        Ok(())
    }

    /// Recompute the corpus-wide reverse-dependency counts and persist each
    /// package's `dependents` into its stored facets. Returns packages updated.
    /// Runs in pages; safe to re-run (idempotent overwrite). Writes the count
    /// for every package that appears as a dependency target; packages with no
    /// dependents receive a `0` write so stale counts from prior sweeps decay.
    pub async fn refresh_dependents(&self) -> Result<u64, IndexError> {
        use crate::search::dependents::{DependencyRow, count_dependents};

        const PAGE_SIZE: u64 = 1_000;

        // Collect all rows from the paginated scan.
        let mut rows: Vec<DependencyRow> = Vec::new();
        let mut after: Option<uuid::Uuid> = None;
        loop {
            let (sql, params) = queries::search::all_current_facets(PAGE_SIZE, after);
            let page = sqlx::query_with(&sql, params)
                .fetch_all(&self.pool)
                .await
                .map_err(IndexError::Database)?;

            let done = page.len() < PAGE_SIZE as usize;
            let mut last_id: Option<uuid::Uuid> = None;

            for row in &page {
                let id: uuid::Uuid = row.try_get(0).map_err(IndexError::Database)?;
                let language: String = row.try_get(1).map_err(IndexError::Database)?;
                let name_canonical: String = row.try_get(2).map_err(IndexError::Database)?;
                let facets_json: Option<serde_json::Value> =
                    row.try_get(3).map_err(IndexError::Database)?;

                last_id = Some(id);

                let ecosystem = codec::ecosystem_from_token(&language)
                    .map_err(convert_codec_error)?;

                let dependencies = facets_json
                    .as_ref()
                    .and_then(|v| {
                        codec::facets_from_json(Some(v))
                            .ok()
                            .flatten()
                            .map(|f| f.dependencies)
                    })
                    .unwrap_or_default();

                rows.push(DependencyRow {
                    ecosystem,
                    name: smol_str::SmolStr::from(name_canonical),
                    dependencies,
                });
            }

            after = last_id;
            if done {
                break;
            }
        }

        // Build corpus-wide dependents counts from the collected rows.
        let counts = count_dependents(rows);

        // Write counts back — but only where the stored value actually changed.
        // Every write also bumps `packages.updated_at` (the tantivy sync's
        // `changed_since` poll filters on it, not on `parse_status.updated_at`),
        // so an unconditional rewrite would force a full replica re-fold of the
        // corpus on every sweep. Packages that stopped being depended on decay
        // to `0` the same way (stored `Some(n>0)` → counted `0` is a change).
        let mut updated: u64 = 0;
        let mut after: Option<uuid::Uuid> = None;
        loop {
            let (sql, params) = queries::search::all_current_facets(PAGE_SIZE, after);
            let page = sqlx::query_with(&sql, params)
                .fetch_all(&self.pool)
                .await
                .map_err(IndexError::Database)?;

            let done = page.len() < PAGE_SIZE as usize;
            let mut last_id: Option<uuid::Uuid> = None;

            for row in &page {
                let id: uuid::Uuid = row.try_get(0).map_err(IndexError::Database)?;
                let language: String = row.try_get(1).map_err(IndexError::Database)?;
                let name_canonical: String = row.try_get(2).map_err(IndexError::Database)?;
                let facets_json: Option<serde_json::Value> =
                    row.try_get(3).map_err(IndexError::Database)?;

                last_id = Some(id);

                let ecosystem = codec::ecosystem_from_token(&language)
                    .map_err(convert_codec_error)?;
                let name = smol_str::SmolStr::from(name_canonical);
                let dep_count = counts
                    .get(&(ecosystem, name))
                    .copied()
                    .unwrap_or(0);

                let stored = facets_json
                    .as_ref()
                    .and_then(|v| v.get("dependents"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|n| n as u32);
                if stored == Some(dep_count) {
                    continue;
                }

                let package = codec::package_id_from_uuid(id);
                let mut tx = self.pool.begin().await.map_err(IndexError::BeginTx)?;
                let (q, p) = queries::index::set_dependents(package, dep_count)
                    .map_err(convert_codec_error)?;
                execute_query(&mut tx, &q, p).await?;
                let (touch_q, touch_p) = queries::index::touch_package(package);
                execute_query(&mut tx, &touch_q, touch_p).await?;
                tx.commit().await.map_err(IndexError::Commit)?;
                updated += 1;
            }

            after = last_id;
            if done {
                break;
            }
        }

        Ok(updated)
    }

    /// Read a package's serving-projection symbols back out of the global index.
    pub async fn symbols_for(&self, package: PackageId) -> Result<Vec<heart::Symbol>, IndexError> {
        let (query, parameters) = queries::index::symbols_for(package);

        let rows = sqlx::query_with(&query, parameters)
            .fetch_all(&self.pool)
            .await
            .map_err(IndexError::Database)?;

        rows.iter().map(row_to_symbol).collect()
    }
}

// -----------------------------------------------------------------------------
// Database Query Helpers
// -----------------------------------------------------------------------------

/// Executes a database query inside a transaction, mapping to `IndexError::Database` on failure.
async fn execute_query(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    query: &str,
    parameters: SqlxValues,
) -> Result<(), IndexError> {
    sqlx::query_with(query, parameters)
        .execute(&mut **transaction)
        .await
        .map_err(IndexError::Database)?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Decoding Functions
// -----------------------------------------------------------------------------

fn row_to_symbol(row: &PgRow) -> Result<heart::Symbol, IndexError> {
    let identifier: uuid::Uuid = row.try_get(0).map_err(IndexError::Database)?;
    let package_uuid: uuid::Uuid = row.try_get(1).map_err(IndexError::Database)?;
    let fully_qualified_name: String = row.try_get(2).map_err(IndexError::Database)?;
    let kind_token: String = row.try_get(3).map_err(IndexError::Database)?;
    let language: String = row.try_get(4).map_err(IndexError::Database)?;

    let ecosystem = codec::ecosystem_from_token(&language).map_err(convert_codec_error)?;

    let kind = parse_symbol_kind(&kind_token)
        .ok_or(IndexError::UnknownSymbolKind { token: kind_token })?;

    Ok(heart::Symbol {
        id: heart::SymbolId::from_uuid(identifier),
        package: codec::package_id_from_uuid(package_uuid),
        ecosystem,
        name: heart::Name {
            plain: extract_plain_name(&fully_qualified_name).into(),
            fully_qualified: fully_qualified_name.into(),
        },
        kind,
    })
}

fn row_to_package(row: &PgRow) -> Result<crate::Package, codec::CodecError> {
    let language: String = row.try_get(1).map_err(decode_package_row)?;
    let origin_token: String = row.try_get(2).map_err(decode_package_row)?;
    let name_original: String = row.try_get(4).map_err(decode_package_row)?;
    let version_canonical: String = row.try_get(5).map_err(decode_package_row)?;
    let visibility_token: String = row.try_get(6).map_err(decode_package_row)?;
    let owner_kind_token: String = row.try_get(8).map_err(decode_package_row)?;
    let toolchain_json: serde_json::Value = row.try_get(9).map_err(decode_package_row)?;

    let coordinates = codec::coordinates_from_columns(
        &language,
        &origin_token,
        &name_original,
        &version_canonical,
    )?;

    // Utilizing ? for validation side-effects rather than binding to unused variables.
    codec::visibility_from_token(&visibility_token)?;
    codec::owner_kind_from_token(&owner_kind_token)?;

    let toolchain = codec::toolchain_from_json(&toolchain_json)?;

    Ok(crate::Package {
        coordinates,
        toolchain,
    })
}

fn row_to_facets(row: &PgRow) -> Result<Option<crate::metadata::SearchFacets>, codec::CodecError> {
    let facets_json: Option<serde_json::Value> =
        row.try_get(10)
            .map_err(|source| codec::CodecError::SqlxDecode {
                domain: "parse_status.facets",
                source,
            })?;

    codec::facets_from_json(facets_json.as_ref())
}

fn row_to_state(row: &PgRow) -> Result<ResolutionState, codec::CodecError> {
    let state: String = row.try_get(0).map_err(decode_state_row)?;
    let phase: Option<String> = row.try_get(1).map_err(decode_state_row)?;
    let content_hash: Option<Vec<u8>> = row.try_get(2).map_err(decode_state_row)?;
    let needed: bool = row.try_get(3).map_err(decode_state_row)?;
    let failure: Option<serde_json::Value> = row.try_get(4).map_err(decode_state_row)?;

    codec::state_from_columns(
        &state,
        phase.as_deref(),
        content_hash.as_deref(),
        needed,
        failure.as_ref(),
    )
}

// -----------------------------------------------------------------------------
// Utilities & Error Converters
// -----------------------------------------------------------------------------

fn parse_symbol_kind(raw: &str) -> Option<heart::SymbolKind> {
    heart::SymbolKind::from_str(raw).ok()
}

fn extract_plain_name(fully_qualified_name: &str) -> &str {
    fully_qualified_name
        .rsplit(['/', '.', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(fully_qualified_name)
}

fn convert_codec_error(error: codec::CodecError) -> IndexError {
    IndexError::Codec(error)
}

fn connect_failure(error: sqlx::Error) -> ConnectFailure {
    match error {
        sqlx::Error::PoolTimedOut => ConnectFailure::Timeout,
        sqlx::Error::Io(_) => ConnectFailure::Unreachable,
        other => ConnectFailure::Other(other.into()),
    }
}

fn decode_package_row(source: sqlx::Error) -> codec::CodecError {
    codec::CodecError::SqlxDecode {
        domain: "packages row",
        source,
    }
}

fn decode_state_row(source: sqlx::Error) -> codec::CodecError {
    codec::CodecError::SqlxDecode {
        domain: "parse_status row",
        source,
    }
}

// -----------------------------------------------------------------------------
// Probe
// -----------------------------------------------------------------------------

impl Probeable for GlobalStore<Live> {
    fn backend(&self) -> BackendKind {
        BackendKind::Postgres
    }

    async fn probe(&self) -> heart::Probe {
        timed_probe(BackendKind::Postgres, async {
            match self
                .get_state(PackageId::from_uuid(heart::Guid::nil()))
                .await
            {
                Ok(_) | Err(IndexError::NotFound { .. }) => None,
                Err(error) => Some(error.to_string()),
            }
        })
        .await
    }
}

const _: fn() = || {
    // Fail at this crate if probe futures stop being Send.
    heart::assert_probe_future_send::<GlobalStore<Live>>();
};
