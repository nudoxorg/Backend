//! The shard bakery — the INDEX-side worker of 09-vector §20.3.
//!
//! For every stored package the bakery turns the symbol projection into a
//! **packed, quantized qdrant-edge shard artifact**: embed (via the same
//! embedder + content-addressed cache the vector fan-out consumer uses), build
//! a fresh Edge shard in a tempdir under the frozen QP1 int8 profile, pack it,
//! and publish the bytes content-addressed in the blob store's CAS namespace.
//! Clients then install the artifact locally as a read-only dep shard.
//!
//! # Identity and idempotence
//!
//! An artifact is identified by its [`EdgepackKey`] — `(package, version,
//! model_id, recipe_id, quant_profile, edge_format_version)` — whose blake3
//! digest keys the `edgepack_artifacts` postgres table. The **single-claim
//! rule** (§20.3) is enforced by `INSERT ... ON CONFLICT DO NOTHING` on that
//! primary key: exactly one worker wins the claim and bakes; everyone else
//! observes the row and moves on. A claim is only released by a terminal
//! status (`ready` | `failed`) — or, for a crashed claimer, by the stale-claim
//! reaper deleting `claimed` rows older than [`STALE_CLAIM_AGE`].
//!
//! # Embedding reuse
//!
//! The bake re-embeds through [`EmbeddingCache::get_or_embed`] over the same
//! [`HttpEmbedder`] the outbox vector consumer drives ([`crate::poll`]'s
//! `materialize_vector`). Qdrant does not expose a cheap raw-vector read-back
//! through the `Semantic` store surface, and the content-addressed cache
//! already dedupes: a bake that follows a materialization re-uses the cached
//! vectors rather than re-running the model. This is the cleanest reuse of the
//! existing code path — one canonical embed pipeline, two consumers.
//!
//! # Enqueue model
//!
//! Rather than threading a channel through the outbox consumer, the bakery is
//! a **follow-on poller** ([`bakery_worker`]): it scans for packages that have
//! a symbol projection but no `edgepack_artifacts` row under the *current*
//! recipe fingerprint, claims them, and bakes. This keeps `poll.rs` untouched,
//! is naturally idempotent, and self-heals after a model/recipe rotation (the
//! fingerprint changes, every package becomes a candidate again).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use heart::{ContentHash, PackageId};
use registry::blob::creation::PendingSection;
use registry::runtime::vector::{
	EmbedRole, EmbeddingCache, EmbeddingKey, EmbeddingModel, EmbeddingPurpose,
};
use vector_core::model::{Metric, ModelId};
use vector_core::quant::QuantProfile;
use vector_core::shard::{EDGE_FORMAT_VERSION, EdgepackKey, ShardSchema};
use vector_core::store::{PayloadValue, PointId};

use crate::search::semantic::embedder::HttpEmbedder;
use crate::{Server, SourceStores};

/// The embed-text recipe this bakery actually runs: the symbol's
/// **fully-qualified name**, exactly like the outbox vector consumer
/// (`materialize_vector`) — reuse of the one canonical fleet embed pipeline.
///
/// This is deliberately *not* `vector_core::recipe::RECIPE_ID` (EmbedText v2):
/// baked shards are only score-comparable with a client whose EmbedStage used
/// the *same* recipe, and labeling this text as v2 would silently break that
/// parity (I2/I16 — an untrue label is a bug). When the server projection
/// grows sig/doc/body facets and moves to EmbedText v2, bump this to the
/// vector-core id and every artifact re-bakes under the new key.
pub const RECIPE_ID: &str = "nudox.fqn.v1";

/// Bridge the serving plane's registry model brand to the vector plane's
/// [`ModelId`] — same id string, two sealed type systems (convergence is
/// tracked; the string is the wire truth either way).
fn core_model_id<M: EmbeddingModel>() -> ModelId {
	ModelId::new(M::id().to_string())
}

/// How long a `claimed` row may sit without a terminal status before the
/// reaper deletes it (a crashed claimer's abandoned claim). Comfortably above
/// any plausible bake duration.
const STALE_CLAIM_AGE: Duration = Duration::from_secs(3600);

/// How many bake candidates one scan tick picks up.
const SCAN_BATCH: i64 = 16;

/// One unit of bakery work: bake `package@version` under `edgepack_key`.
#[derive(Debug, Clone)]
pub struct BakeRequest {
	/// The package whose symbol projection is baked.
	pub package: PackageId,
	/// The package's canonical version string (from `packages.version_canonical`).
	pub version: String,
	/// The full content identity of the artifact to produce.
	pub edgepack_key: EdgepackKey,
}

impl BakeRequest {
	/// The recipe half of the key — everything *except* package/version — as a
	/// stable text token. Stored per row so the candidate scan can find
	/// packages whose artifact predates a model/recipe/format rotation without
	/// recomputing digests in SQL.
	pub fn recipe_fingerprint(&self) -> String {
		recipe_fingerprint_parts(
			&self.edgepack_key.model_id,
			self.edgepack_key.recipe_id.as_str(),
			&self.edgepack_key.quant_profile,
			self.edgepack_key.edge_format_version,
		)
	}
}

/// The current edgepack key for one package generation under the compiled-in
/// model `M` and the frozen recipe/quant/format constants.
pub fn edgepack_key<M: EmbeddingModel>(package: PackageId, version: &str) -> EdgepackKey {
	EdgepackKey {
		package,
		version: version.into(),
		model_id: core_model_id::<M>(),
		recipe_id: RECIPE_ID.into(),
		quant_profile: vector_core::quant::QP1,
		edge_format_version: EDGE_FORMAT_VERSION,
	}
}

/// The process-wide recipe fingerprint for model `M` (see
/// [`BakeRequest::recipe_fingerprint`]).
pub fn recipe_fingerprint<M: EmbeddingModel>() -> String {
	recipe_fingerprint_parts(
		&core_model_id::<M>(),
		RECIPE_ID,
		&vector_core::quant::QP1,
		EDGE_FORMAT_VERSION,
	)
}

fn recipe_fingerprint_parts(
	model_id: &ModelId,
	recipe_id: &str,
	quant_profile: &QuantProfile,
	edge_format_version: u32,
) -> String {
	// Stable string representation of the quant profile for the fingerprint.
	let quant_str = match quant_profile {
		QuantProfile::None => "none".to_owned(),
		QuantProfile::ScalarInt8 { quantile, always_ram } => {
			format!("scalar-int8/q{quantile}/ram{}", u8::from(*always_ram))
		}
	};
	format!("{}/{}/{}/{}", model_id.as_str(), recipe_id, quant_str, edge_format_version)
}

/// Why a bakery operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BakeryError {
	/// The `edgepack_artifacts` table could not be read/written.
	#[error("bakery claim store failed")]
	Database(#[source] sqlx::Error),

	/// Reading the symbol projection from the global index failed.
	#[error(transparent)]
	Registry(#[from] crate::registry::RegistryError),

	/// Embedding a symbol's text failed.
	#[error(transparent)]
	Embed(#[from] registry::runtime::error::EmbedError),

	/// Writing/reading the packed artifact against the blob store failed.
	#[error(transparent)]
	Store(#[from] registry::StoreError),

	/// The tempdir for the shard build could not be created.
	#[error("bakery tempdir failed")]
	Io(#[source] std::io::Error),

	/// Building, compacting, or packing the Edge shard failed.
	#[error("edge shard build failed")]
	Edge(#[source] anyhow::Error),
}

/// The claim protocol over `edgepack_artifacts`, abstracted so the
/// single-claim invariant is unit-testable without postgres.
pub trait ClaimStore: Send + Sync {
	/// Attempt to claim the artifact identified by `request`'s key digest.
	/// Returns `true` iff *this* caller inserted the `claimed` row — the
	/// single-claim rule (`INSERT ... ON CONFLICT DO NOTHING`).
	fn try_claim(
		&self,
		request: &BakeRequest,
	) -> impl Future<Output = Result<bool, BakeryError>> + Send;

	/// Terminal success: record the artifact id + RAM estimate, status `ready`.
	fn mark_ready(
		&self,
		digest: ContentHash,
		artifact: ContentHash,
		ram_estimate: i64,
	) -> impl Future<Output = Result<(), BakeryError>> + Send;

	/// Terminal failure: status `failed`. The claim stays terminal — a failed
	/// bake is not retried until the row is deleted (operator action) or the
	/// recipe rotates.
	fn mark_failed(&self, digest: ContentHash) -> impl Future<Output = Result<(), BakeryError>> + Send;
}

/// What one bake attempt produced.
#[derive(Debug, Clone, Copy)]
pub struct BakedArtifact {
	/// The blake3 of the packed artifact bytes — the CAS key it is stored under.
	pub artifact: ContentHash,
	/// `vector_core::admission::ram_estimate_bytes(n_symbols)` (§20.4).
	pub ram_estimate: i64,
	/// How many symbol points the shard holds.
	pub symbols: usize,
}

/// The outcome of [`run_bake`] for one request.
#[derive(Debug)]
pub enum BakeOutcome {
	/// Another worker holds (or held) the claim; nothing was done here.
	AlreadyClaimed,
	/// This worker claimed, baked, and published the artifact.
	Baked(BakedArtifact),
	/// This worker claimed and the bake failed; the row is marked `failed`.
	Failed(BakeryError),
}

/// Claim-then-bake, upholding the single-claim invariant: the bake closure
/// runs **only** when this caller won the `INSERT ... ON CONFLICT DO NOTHING`
/// race, and the claim is released only by a terminal status.
pub async fn run_bake<C, F, Fut>(
	claims: &C,
	request: &BakeRequest,
	bake: F,
) -> Result<BakeOutcome, BakeryError>
where
	C: ClaimStore + ?Sized,
	F: FnOnce() -> Fut,
	Fut: Future<Output = Result<BakedArtifact, BakeryError>>,
{
	let digest = request.edgepack_key.digest();
	if !claims.try_claim(request).await? {
		return Ok(BakeOutcome::AlreadyClaimed);
	}
	match bake().await {
		Ok(artifact) => {
			claims.mark_ready(digest, artifact.artifact, artifact.ram_estimate).await?;
			Ok(BakeOutcome::Baked(artifact))
		}
		Err(error) => {
			claims.mark_failed(digest).await?;
			Ok(BakeOutcome::Failed(error))
		}
	}
}

/// The §20.3 bake procedure for one package.
///
/// 1. Read the symbol projection (the same `symbols_for` read the outbox
///    consumers materialize from).
/// 2. Embed each symbol's fully-qualified name as code, cache-first (see the
///    module docs on reuse).
/// 3. Build a fresh Edge shard in a tempdir under the frozen schema (model
///    brand, QP1 int8, current edge format), upsert all points, compact +
///    flush. The shard build runs in `spawn_blocking` because `EdgeShard` is
///    not `Send` and its I/O is synchronous.
/// 4. Pack the shard directory; the pack's blake3 is the artifact id.
/// 5. Store the bytes content-addressed (`cas/{artifact_id}`) via the same
///    [`PendingSection`] write every blob section uses — idempotent, verified.
pub async fn bake_package<M: EmbeddingModel>(
	stores: &SourceStores<M>,
	embedder: &HttpEmbedder<M>,
	cache: &EmbeddingCache<M>,
	request: &BakeRequest,
) -> Result<BakedArtifact, BakeryError> {
	let symbols = stores
		.global_store
		.symbols_for(request.package)
		.await
		.map_err(crate::registry::RegistryError::from)?;

	// Embed each symbol. Collect (PointId, raw f32 vec, payload) triples —
	// all are Send, so they cross the spawn_blocking boundary safely.
	// The Embedding<M> is dereference-sliceable; we clone its values out now.
	let mut raw_points: Vec<(PointId, Vec<f32>, vector_core::store::Payload)> =
		Vec::with_capacity(symbols.len());
	for symbol in &symbols {
		let text = symbol.name.fully_qualified.as_str();
		let embedding = cache
			.get_or_embed(
				EmbeddingKey::new(M::id(), EmbedRole::Document, text),
				embedder,
				text,
				EmbeddingPurpose::Code,
			)
			.await?;
		raw_points.push((PointId::from_symbol(&symbol.id), embedding.as_slice().to_vec(), symbol_payload(symbol)));
	}
	let n_symbols = symbols.len();

	// The shard schema follows the *serving* model brand `M` — its id string
	// and its dimensionality — so a fleet configured for E5/OpenAI bakes
	// shards whose schema (and Edge dimension check) matches the vectors it
	// actually produced. Hardcoding one brand here would corrupt any non-768
	// deployment at upsert time.
	let schema = ShardSchema {
		format_version: EDGE_FORMAT_VERSION,
		model_id: core_model_id::<M>(),
		dim: M::DIMENSIONS,
		distance: Metric::Cosine,
		quant_profile: vector_core::quant::QP1,
		recipe_id: RECIPE_ID.to_owned(),
	};

	// Build and pack the shard on a blocking thread: EdgeShard is not Send
	// and all its I/O is synchronous. `pack_shard` is also sync.
	let (artifact_bytes, artifact_hash) =
		tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, ContentHash), BakeryError> {
			let dir = tempfile::tempdir().map_err(BakeryError::Io)?;
			let shard = vector_local::shard::open_or_create(dir.path(), &schema)
				.map_err(|error| BakeryError::Edge(anyhow::Error::new(error)))?;

			// Upsert points directly via the sync EdgeShard API.
			for (id, vector, payload) in raw_points {
				vector_local::upsert_raw(&shard, id, vector, payload)
					.map_err(|error| BakeryError::Edge(anyhow::Error::new(error)))?;
			}
			// Compact to a fixed point (max 8 passes), then flush.
			for _ in 0..8 {
				let progress = shard
					.optimize()
					.map_err(|error| BakeryError::Edge(anyhow::Error::new(error)))?;
				if !progress {
					break;
				}
			}
			shard.flush();
			// Close before packing so the directory is quiescent.
			drop(shard);

			vector_local::pack_shard(dir.path())
				.map_err(|error| BakeryError::Edge(anyhow::Error::new(error)))
		})
		.await
		.map_err(|join_err| BakeryError::Io(std::io::Error::other(join_err)))??;

	stores
		.blobs
		.put_section(&PendingSection {
			hash: artifact_hash,
			bytes: bytes::Bytes::from(artifact_bytes),
		})
		.await?;

	let ram_estimate = vector_core::admission::ram_estimate_bytes(n_symbols as u64) as i64;
	Ok(BakedArtifact { artifact: artifact_hash, ram_estimate, symbols: n_symbols })
}


/// One symbol → payload map carrying the three §20.3 filterable keys.
fn symbol_payload(symbol: &heart::Symbol) -> vector_core::store::Payload {
	let mut payload = vector_core::store::Payload::new();
	payload.insert(
		smol_str::SmolStr::new("language"),
		PayloadValue::Str(smol_str::SmolStr::new(symbol.ecosystem.to_string())),
	);
	payload.insert(
		smol_str::SmolStr::new("package"),
		PayloadValue::Str(smol_str::SmolStr::new(symbol.package.as_uuid().to_string())),
	);
	payload.insert(
		smol_str::SmolStr::new("kind"),
		PayloadValue::Str(smol_str::SmolStr::new(symbol.kind.to_string())),
	);
	payload
}

// ─────────────────────────────────────────────────────────────────────────────
// Postgres claim store
// ─────────────────────────────────────────────────────────────────────────────

/// One row of `edgepack_artifacts`, as read back for the manifest surface.
#[derive(Debug, Clone)]
pub struct EdgepackRow {
	/// The blake3 digest of the artifact's [`EdgepackKey`] (the primary key).
	pub digest: ContentHash,
	/// `claimed` | `ready` | `failed`.
	pub status: EdgepackStatus,
	/// The packed artifact's CAS key, once `ready`.
	pub artifact: Option<ContentHash>,
	/// The client-side admission estimate in bytes, once `ready` (§20.4).
	pub ram_estimate: Option<i64>,
}

/// The lifecycle of one `edgepack_artifacts` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgepackStatus {
	/// A worker holds the single claim and is baking.
	Claimed,
	/// The artifact is published and servable.
	Ready,
	/// The bake failed terminally under this key.
	Failed,
}

impl EdgepackStatus {
	/// The wire/DB token.
	pub fn as_str(self) -> &'static str {
		match self {
			EdgepackStatus::Claimed => "claimed",
			EdgepackStatus::Ready => "ready",
			EdgepackStatus::Failed => "failed",
		}
	}

	fn parse(raw: &str) -> Option<Self> {
		match raw {
			"claimed" => Some(EdgepackStatus::Claimed),
			"ready" => Some(EdgepackStatus::Ready),
			"failed" => Some(EdgepackStatus::Failed),
			_ => None,
		}
	}
}

/// The postgres-backed claim store + artifact ledger over `edgepack_artifacts`.
///
/// Schema management follows the [`PgSessionStore::migrate`] pattern (inline
/// `CREATE TABLE IF NOT EXISTS`, applied once at assembly) rather than the
/// registry's sea-query `schema_ddl()` — the table is server-owned bakery
/// state, not part of the registry's spine, so it stays out of the registry
/// crate's FK-ordered create set.
///
/// [`PgSessionStore::migrate`]: registry::runtime::session::PgSessionStore::migrate
pub struct PgEdgepackStore {
	pool: sqlx::PgPool,
}

impl PgEdgepackStore {
	/// Wrap an existing pool (the definitive source's postgres).
	pub fn new(pool: sqlx::PgPool) -> Self { Self { pool } }

	/// Create the `edgepack_artifacts` table (`IF NOT EXISTS`, idempotent).
	pub async fn migrate(&self) -> Result<(), BakeryError> {
		sqlx::query(
			"CREATE TABLE IF NOT EXISTS edgepack_artifacts (\
			   edgepack_key_digest BYTEA PRIMARY KEY, \
			   package uuid NOT NULL, \
			   version text NOT NULL, \
			   recipe_fingerprint text NOT NULL, \
			   artifact_id BYTEA, \
			   ram_estimate BIGINT, \
			   status text NOT NULL CHECK (status IN ('claimed', 'ready', 'failed')), \
			   created_at timestamptz NOT NULL DEFAULT now(), \
			   updated_at timestamptz NOT NULL DEFAULT now()\
			 )",
		)
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		sqlx::query(
			"CREATE INDEX IF NOT EXISTS edgepack_artifacts_package \
			 ON edgepack_artifacts (package, version)",
		)
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(())
	}

	/// Packages that have a symbol projection but no `edgepack_artifacts` row
	/// under the current recipe fingerprint — the bakery's work scan. `failed`
	/// rows also suppress their package (no hot-loop on a poisoned bake); a
	/// retry requires deleting the row or rotating the recipe.
	pub async fn candidates(
		&self,
		fingerprint: &str,
		limit: i64,
	) -> Result<Vec<(PackageId, String)>, BakeryError> {
		let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
			"SELECT p.id, p.version_canonical FROM packages p \
			 WHERE EXISTS (SELECT 1 FROM symbols s WHERE s.package_id = p.id) \
			   AND NOT EXISTS (\
			     SELECT 1 FROM edgepack_artifacts e \
			     WHERE e.package = p.id \
			       AND e.version = p.version_canonical \
			       AND e.recipe_fingerprint = $1\
			   ) \
			 ORDER BY p.updated_at DESC \
			 LIMIT $2",
		)
		.bind(fingerprint)
		.bind(limit)
		.fetch_all(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(rows
			.into_iter()
			.map(|(id, version)| (PackageId::from_uuid(id), version))
			.collect())
	}

	/// Delete `claimed` rows whose worker evidently died (older than `age`
	/// without reaching a terminal status), releasing the claim for re-bake.
	pub async fn release_stale_claims(&self, age: Duration) -> Result<u64, BakeryError> {
		let result = sqlx::query(
			"DELETE FROM edgepack_artifacts \
			 WHERE status = 'claimed' AND updated_at < now() - $1::interval",
		)
		.bind(format!("{} seconds", age.as_secs()))
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(result.rows_affected())
	}

	/// Read the row for one `(package, version, fingerprint)` triple — the
	/// manifest surface's lookup. At most one row exists (the digest is a
	/// function of exactly these inputs plus the constants).
	pub async fn get(
		&self,
		package: PackageId,
		version: &str,
		fingerprint: &str,
	) -> Result<Option<EdgepackRow>, BakeryError> {
		let row: Option<(Vec<u8>, String, Option<Vec<u8>>, Option<i64>)> = sqlx::query_as(
			"SELECT edgepack_key_digest, status, artifact_id, ram_estimate \
			 FROM edgepack_artifacts \
			 WHERE package = $1 AND version = $2 AND recipe_fingerprint = $3",
		)
		.bind(package.as_uuid())
		.bind(version)
		.bind(fingerprint)
		.fetch_optional(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(row.and_then(|(digest, status, artifact, ram_estimate)| {
			Some(EdgepackRow {
				digest: hash_from_column(&digest)?,
				status: EdgepackStatus::parse(&status)?,
				artifact: match artifact {
					Some(bytes) => Some(hash_from_column(&bytes)?),
					None => None,
				},
				ram_estimate,
			})
		}))
	}
}

/// Decode a 32-byte BYTEA column back into a [`ContentHash`]; `None` on a
/// malformed width (a corrupt row is skipped, not a panic).
fn hash_from_column(bytes: &[u8]) -> Option<ContentHash> {
	<[u8; 32]>::try_from(bytes).ok().map(ContentHash::from_bytes)
}

impl ClaimStore for PgEdgepackStore {
	async fn try_claim(&self, request: &BakeRequest) -> Result<bool, BakeryError> {
		let digest = request.edgepack_key.digest();
		let result = sqlx::query(
			"INSERT INTO edgepack_artifacts \
			   (edgepack_key_digest, package, version, recipe_fingerprint, status) \
			 VALUES ($1, $2, $3, $4, 'claimed') \
			 ON CONFLICT (edgepack_key_digest) DO NOTHING",
		)
		.bind(digest.as_bytes().as_slice())
		.bind(request.package.as_uuid())
		.bind(&request.version)
		.bind(request.recipe_fingerprint())
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(result.rows_affected() == 1)
	}

	async fn mark_ready(
		&self,
		digest: ContentHash,
		artifact: ContentHash,
		ram_estimate: i64,
	) -> Result<(), BakeryError> {
		sqlx::query(
			"UPDATE edgepack_artifacts \
			 SET status = 'ready', artifact_id = $2, ram_estimate = $3, updated_at = now() \
			 WHERE edgepack_key_digest = $1",
		)
		.bind(digest.as_bytes().as_slice())
		.bind(artifact.as_bytes().as_slice())
		.bind(ram_estimate)
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(())
	}

	async fn mark_failed(&self, digest: ContentHash) -> Result<(), BakeryError> {
		sqlx::query(
			"UPDATE edgepack_artifacts \
			 SET status = 'failed', updated_at = now() \
			 WHERE edgepack_key_digest = $1",
		)
		.bind(digest.as_bytes().as_slice())
		.execute(&self.pool)
		.await
		.map_err(BakeryError::Database)?;
		Ok(())
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// Worker loop
// ─────────────────────────────────────────────────────────────────────────────

/// The bakery poller: scan → claim → bake → publish, forever. Spawned by
/// [`Server::serve`] on gateway roles when `config.bakery.enabled`. Follows
/// the same loop discipline as every poller in [`crate::poll`]: never returns
/// under normal operation, logs + retries transient faults at the next tick,
/// and every unit of work is idempotent (claims are single-winner; CAS writes
/// dedupe), so a task abort mid-bake is safe — the stale-claim reaper releases
/// the abandoned claim.
#[tracing::instrument(skip_all, name = "bakery_worker")]
pub(crate) async fn bakery_worker<M: EmbeddingModel>(server: Arc<Server<M>>) {
	let Some(claims) = server.edgepacks() else {
		tracing::warn!("bakery worker started without an edgepack store; exiting");
		return;
	};
	let interval = server.config().bakery.poll_interval;
	let fingerprint = recipe_fingerprint::<M>();
	tracing::info!(%fingerprint, "bakery worker started");

	loop {
		match claims.release_stale_claims(STALE_CLAIM_AGE).await {
			Ok(0) => {}
			Ok(released) => {
				tracing::warn!(released, "released stale bakery claims");
			}
			Err(error) => {
				tracing::warn!(error = %error, "stale-claim reap failed");
			}
		}

		let candidates = match claims.candidates(&fingerprint, SCAN_BATCH).await {
			Ok(candidates) => candidates,
			Err(error) => {
				tracing::warn!(error = %error, "bakery candidate scan failed");
				tokio::time::sleep(interval).await;
				continue;
			}
		};

		for (package, version) in candidates {
			let request = BakeRequest {
				package,
				version: version.clone(),
				edgepack_key: edgepack_key::<M>(package, &version),
			};
			let started = std::time::Instant::now();
			let outcome = run_bake(claims, &request, || {
				bake_package(server.base(), server.embedder(), server.embedding_cache(), &request)
			})
			.await;
			match outcome {
				Ok(BakeOutcome::Baked(artifact)) => {
					metrics::counter!("bakery_bakes").increment(1);
					metrics::histogram!("bakery_bake_duration_seconds")
						.record(started.elapsed().as_secs_f64());
					tracing::info!(
						%package,
						version = %version,
						artifact = %artifact.artifact,
						symbols = artifact.symbols,
						ram_estimate = artifact.ram_estimate,
						"edgepack artifact baked"
					);
				}
				Ok(BakeOutcome::AlreadyClaimed) => {
					tracing::debug!(%package, version = %version, "edgepack already claimed elsewhere");
				}
				Ok(BakeOutcome::Failed(error)) => {
					metrics::counter!("bakery_bake_failures").increment(1);
					tracing::error!(
						%package,
						version = %version,
						error = %error,
						"edgepack bake failed; row marked failed"
					);
				}
				Err(error) => {
					// The claim ledger itself failed — nothing terminal was
					// recorded; the next tick retries.
					metrics::counter!("bakery_bake_failures").increment(1);
					tracing::warn!(%package, version = %version, error = %error, "bakery claim protocol failed");
				}
			}
		}

		tokio::time::sleep(interval).await;
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::collections::HashSet;
	use std::sync::Mutex;
	use std::sync::atomic::{AtomicUsize, Ordering};

	/// An in-memory [`ClaimStore`]: a set of claimed digests plus terminal
	/// transitions, mirroring the `ON CONFLICT DO NOTHING` semantics.
	#[derive(Default)]
	struct MockClaims {
		claimed: Mutex<HashSet<[u8; 32]>>,
		ready: Mutex<Vec<[u8; 32]>>,
		failed: Mutex<Vec<[u8; 32]>>,
	}

	impl ClaimStore for MockClaims {
		async fn try_claim(&self, request: &BakeRequest) -> Result<bool, BakeryError> {
			let digest = *request.edgepack_key.digest().as_bytes();
			Ok(self.claimed.lock().expect("unpoisoned").insert(digest))
		}

		async fn mark_ready(
			&self,
			digest: ContentHash,
			_artifact: ContentHash,
			_ram_estimate: i64,
		) -> Result<(), BakeryError> {
			self.ready.lock().expect("unpoisoned").push(*digest.as_bytes());
			Ok(())
		}

		async fn mark_failed(&self, digest: ContentHash) -> Result<(), BakeryError> {
			self.failed.lock().expect("unpoisoned").push(*digest.as_bytes());
			Ok(())
		}
	}

	fn request() -> BakeRequest {
		let package = PackageId::from_uuid(uuid::Uuid::from_u128(7));
		BakeRequest {
			package,
			version: "1.2.3".to_owned(),
			edgepack_key: EdgepackKey {
				package,
				version: "1.2.3".into(),
				model_id: vector_core::model::JinaCodeV2::id(),
				recipe_id: RECIPE_ID.into(),
				quant_profile: vector_core::quant::QP1,
				edge_format_version: vector_core::shard::EDGE_FORMAT_VERSION,
			},
		}
	}

	fn baked() -> BakedArtifact {
		BakedArtifact {
			artifact: ContentHash::of_bytes(b"artifact"),
			ram_estimate: 4096,
			symbols: 3,
		}
	}

	/// Two concurrent claims on the same edgepack key → exactly one bake runs;
	/// the loser observes [`BakeOutcome::AlreadyClaimed`].
	#[tokio::test]
	async fn concurrent_claims_bake_once() {
		let claims = MockClaims::default();
		let bakes = AtomicUsize::new(0);
		let request = request();

		let (left, right) = tokio::join!(
			run_bake(&claims, &request, || async {
				bakes.fetch_add(1, Ordering::SeqCst);
				Ok(baked())
			}),
			run_bake(&claims, &request, || async {
				bakes.fetch_add(1, Ordering::SeqCst);
				Ok(baked())
			}),
		);

		assert_eq!(bakes.load(Ordering::SeqCst), 1, "single-claim rule: exactly one bake");
		let outcomes = [left.expect("claim protocol"), right.expect("claim protocol")];
		assert_eq!(
			outcomes.iter().filter(|o| matches!(o, BakeOutcome::Baked(_))).count(),
			1
		);
		assert_eq!(
			outcomes.iter().filter(|o| matches!(o, BakeOutcome::AlreadyClaimed)).count(),
			1
		);
		assert_eq!(claims.ready.lock().expect("unpoisoned").len(), 1);
	}

	/// A failed bake releases the claim only into the terminal `failed` state —
	/// it never silently re-opens, and a subsequent attempt on the same key is
	/// refused by the standing claim.
	#[tokio::test]
	async fn failed_bake_is_terminal() {
		let claims = MockClaims::default();
		let request = request();

		let first = run_bake(&claims, &request, || async {
			Err(BakeryError::Io(std::io::Error::other("disk full")))
		})
		.await
		.expect("claim protocol");
		assert!(matches!(first, BakeOutcome::Failed(_)));
		assert_eq!(claims.failed.lock().expect("unpoisoned").len(), 1);

		let second = run_bake(&claims, &request, || async { Ok(baked()) })
			.await
			.expect("claim protocol");
		assert!(matches!(second, BakeOutcome::AlreadyClaimed), "terminal row still holds the key");
	}

	/// The recipe fingerprint covers every non-package field of the key — a
	/// model/recipe/format rotation changes it (and so re-opens the scan).
	#[test]
	fn recipe_fingerprint_tracks_rotation() {
		let request = request();
		let fingerprint = request.recipe_fingerprint();
		assert!(fingerprint.contains(RECIPE_ID));
		assert!(fingerprint.contains("jinaai/jina-embeddings-v2-base-code"));
		let mut rotated = request;
		rotated.edgepack_key.recipe_id = "embed-text/v3".into();
		assert_ne!(fingerprint, rotated.recipe_fingerprint());
	}

	/// A claimed row that is older than STALE_CLAIM_AGE must be re-claimable:
	/// once the in-memory mock's `claimed` set is cleared (simulating the reaper
	/// deleting the stale row from postgres), a subsequent attempt wins the claim
	/// and bakes.
	#[tokio::test]
	async fn stale_claim_is_reclaimable_after_reaper() {
		// Simulate: first worker claimed and crashed (the reaper would delete
		// the row in postgres; our mock lets us clear the set to simulate that).
		let claims = MockClaims::default();
		let request = request();

		// Simulate crash: insert directly into claimed without calling mark_ready.
		{
			let digest = *request.edgepack_key.digest().as_bytes();
			claims.claimed.lock().expect("unpoisoned").insert(digest);
		}

		// Before the reaper: the claim is held; another attempt loses.
		let before_reap = run_bake(&claims, &request, || async { Ok(baked()) })
			.await
			.expect("claim protocol");
		assert!(matches!(before_reap, BakeOutcome::AlreadyClaimed));

		// Simulate the stale-claim reaper deleting the row (clearing the mock).
		claims.claimed.lock().expect("unpoisoned").clear();

		// After the reaper: the claim is free; a new attempt wins.
		let after_reap = run_bake(&claims, &request, || async { Ok(baked()) })
			.await
			.expect("claim protocol");
		assert!(
			matches!(after_reap, BakeOutcome::Baked(_)),
			"re-claim after reaper should bake"
		);
		assert_eq!(claims.ready.lock().expect("unpoisoned").len(), 1);
	}
}
