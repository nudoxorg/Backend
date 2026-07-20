//! The shard-bakery **ledger** — the index-native half of 09-vector §20.3,
//! folded out of the dissolved `server::bakery` (§8).
//!
//! The bakery turns a stored package's symbol projection into a packed,
//! quantized qdrant-edge shard artifact and publishes it content-addressed.
//! That *compute* (embed → build Edge shard → pack → publish) is the vector
//! plane's concern and is composed by the client/GUI over `registry::vector`;
//! it is **STAGED** out of `index` (see the note at the bottom of this file).
//!
//! What lands here is the durable **ledger**: the `edgepack_artifacts` catalog
//! table access — the single-claim protocol, the artifact row, the candidate
//! scan, and the stale-claim reaper. This is index storage state (it lives in
//! the versioned catalog next to `versions`/`symbols_proj`), so it belongs in
//! the data layer, keyed on the [`EdgepackKey`] the vector plane defines.
//!
//! # Identity and idempotence
//!
//! An artifact is identified by its [`EdgepackKey`] — `(package, version,
//! model_id, recipe_id, quant_profile, edge_format_version)` — whose blake3
//! digest keys the `edgepack_artifacts` table. The **single-claim rule**
//! (§20.3) is `INSERT ... ON CONFLICT DO NOTHING` on that primary key: exactly
//! one worker wins the claim and bakes; everyone else observes the row and
//! moves on. A claim is released only by a terminal status (`ready`/`failed`)
//! or, for a crashed claimer, by the stale-claim reaper.

use std::future::Future;
use std::time::Duration;

use heart::{content::ContentHash, PackageId};
use registry::vector::{EdgepackKey, ModelId, QuantProfile, EDGE_FORMAT_VERSION, QP1};

use crate::engine::{CatalogEngine, EngineError, VersioningEngine, Value};

/// The embed-text recipe this bakery runs: the symbol's **fully-qualified
/// name**, exactly like the outbox vector consumer — reuse of the one canonical
/// fleet embed pipeline. Deliberately *not* `registry::vector::RECIPE_ID`
/// (EmbedText v2): baked shards are only score-comparable with a client whose
/// EmbedStage used the *same* recipe (I2/I16).
pub const RECIPE_ID: &str = "nudox.fqn.v1";

/// How long a `claimed` row may sit without a terminal status before the reaper
/// deletes it (a crashed claimer's abandoned claim).
pub const STALE_CLAIM_AGE: Duration = Duration::from_secs(3600);

/// One unit of bakery work: bake `package@version` under `edgepack_key`.
#[derive(Debug, Clone)]
pub struct BakeRequest {
    /// The package whose symbol projection is baked.
    pub package: PackageId,
    /// The package's canonical version string.
    pub version: String,
    /// The full content identity of the artifact to produce.
    pub edgepack_key: EdgepackKey,
}

impl BakeRequest {
    /// The recipe half of the key — everything *except* package/version — as a
    /// stable text token, stored per row so the candidate scan can find
    /// packages whose artifact predates a model/recipe/format rotation.
    pub fn recipe_fingerprint(&self) -> String {
        recipe_fingerprint_parts(
            &self.edgepack_key.model_id,
            self.edgepack_key.recipe_id.as_str(),
            &self.edgepack_key.quant_profile,
            self.edgepack_key.edge_format_version,
        )
    }
}

/// The current edgepack key for one package generation under model id `model`
/// and the frozen recipe/quant/format constants.
pub fn edgepack_key(package: PackageId, version: &str, model: ModelId) -> EdgepackKey {
    EdgepackKey {
        package,
        version: version.into(),
        model_id: model,
        recipe_id: RECIPE_ID.into(),
        quant_profile: QP1,
        edge_format_version: EDGE_FORMAT_VERSION,
    }
}

/// The process-wide recipe fingerprint for model id `model`.
pub fn recipe_fingerprint(model: &ModelId) -> String {
    recipe_fingerprint_parts(model, RECIPE_ID, &QP1, EDGE_FORMAT_VERSION)
}

fn recipe_fingerprint_parts(
    model_id: &ModelId,
    recipe_id: &str,
    quant_profile: &QuantProfile,
    edge_format_version: u32,
) -> String {
    let quant_str = match quant_profile {
        QuantProfile::None => "none".to_owned(),
        QuantProfile::ScalarInt8 { quantile, always_ram } => {
            format!("scalar-int8/q{quantile}/ram{}", u8::from(*always_ram))
        }
    };
    format!("{}/{}/{}/{}", model_id.as_str(), recipe_id, quant_str, edge_format_version)
}

/// Why a bakery **ledger** operation failed.
#[derive(Debug, thiserror::Error)]
pub enum BakeryError {
    /// The `edgepack_artifacts` table could not be read/written.
    #[error("bakery claim store failed")]
    Catalog(#[source] EngineError),

    /// Reading the symbol projection from the global index failed.
    #[error(transparent)]
    Index(#[from] crate::error::IndexError),

    /// Writing/reading the packed artifact against the blob store failed.
    #[error(transparent)]
    Store(#[from] crate::error::StoreError),
}

/// The claim protocol over `edgepack_artifacts`, abstracted so the single-claim
/// invariant is unit-testable without a live engine.
pub trait ClaimStore: Send + Sync {
    /// Attempt to claim the artifact identified by `request`'s key digest.
    /// Returns `true` iff *this* caller inserted the `claimed` row.
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

    /// Terminal failure: status `failed`. The claim stays terminal.
    fn mark_failed(
        &self,
        digest: ContentHash,
    ) -> impl Future<Output = Result<(), BakeryError>> + Send;
}

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

/// The catalog-backed claim store + artifact ledger over `edgepack_artifacts`
/// (the table is created by the catalog's own migrations — no migrate step).
/// Generic over the concrete [`VersioningEngine`] the catalog is opened with.
pub struct CatalogEdgepackStore<E: VersioningEngine> {
    writer: std::sync::Arc<crate::store::writer::CatalogWriter<E>>,
}

impl<E: VersioningEngine + Send + Sync> CatalogEdgepackStore<E> {
    /// Wrap the definitive base's catalog writer.
    pub fn new(writer: std::sync::Arc<crate::store::writer::CatalogWriter<E>>) -> Self {
        Self { writer }
    }

    fn engine(&self) -> &E {
        self.writer.engine()
    }

    /// Versions that have a symbol projection but no `edgepack_artifacts` row
    /// under the current recipe fingerprint — the bakery's work scan. `failed`
    /// rows suppress their version (no hot-loop on a poisoned bake).
    pub async fn candidates(
        &self,
        fingerprint: &str,
        limit: i64,
    ) -> Result<Vec<(PackageId, String)>, BakeryError> {
        let rows = self
            .engine()
            .query_rows(
                "SELECT v.id, v.version_canonical FROM versions v \
                 WHERE EXISTS (SELECT 1 FROM symbols_proj s WHERE s.version_id = v.id) \
                   AND NOT EXISTS (\
                     SELECT 1 FROM edgepack_artifacts e \
                     WHERE e.version_id = v.id AND e.recipe_fingerprint = ?1\
                   ) \
                 ORDER BY v.id LIMIT ?2",
                &[Value::Text(fingerprint.to_owned()), Value::Integer(limit)],
                &mut |row| Ok((row.get_blob(0)?, row.get_text(1)?)),
            )
            .map_err(BakeryError::Catalog)?;
        Ok(rows
            .into_iter()
            .filter_map(|(id, version)| {
                crate::ids::version_id::from_blob(&id).ok().map(|id| (id, version))
            })
            .collect())
    }

    /// Delete `claimed` rows whose worker evidently died (older than `age`
    /// without reaching a terminal status), releasing the claim for re-bake.
    pub async fn release_stale_claims(&self, age: Duration) -> Result<u64, BakeryError> {
        let cutoff = chrono::Utc::now().timestamp_millis() - age.as_millis() as i64;
        let removed = self
            .engine()
            .execute(
                "DELETE FROM edgepack_artifacts WHERE status = 'claimed' AND updated_at < ?1",
                &[Value::Integer(cutoff)],
            )
            .map_err(BakeryError::Catalog)?;
        Ok(removed as u64)
    }

    /// Read the row for one `(package, fingerprint)` pair — the manifest
    /// surface's lookup. The version id already encodes the version coordinate.
    pub async fn get(
        &self,
        package: PackageId,
        fingerprint: &str,
    ) -> Result<Option<EdgepackRow>, BakeryError> {
        let mut rows = self
            .engine()
            .query_rows(
                "SELECT edgepack_key_digest, status, artifact_id, ram_estimate \
                 FROM edgepack_artifacts WHERE version_id = ?1 AND recipe_fingerprint = ?2",
                &[
                    Value::Blob(crate::ids::version_id::to_blob(&package).to_vec()),
                    Value::Text(fingerprint.to_owned()),
                ],
                &mut |row| {
                    Ok((
                        row.get_blob(0)?,
                        row.get_text(1)?,
                        row.get_optional_blob(2)?,
                        row.get_optional_integer(3)?,
                    ))
                },
            )
            .map_err(BakeryError::Catalog)?;
        Ok(rows.pop().and_then(|(digest, status, artifact, ram_estimate)| {
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

/// Decode a 32-byte BLOB column back into a [`ContentHash`]; `None` on a
/// malformed width (a corrupt row is skipped, not a panic).
fn hash_from_column(bytes: &[u8]) -> Option<ContentHash> {
    <[u8; 32]>::try_from(bytes).ok().map(ContentHash::from_bytes)
}

impl<E: VersioningEngine + Send + Sync> ClaimStore for CatalogEdgepackStore<E> {
    async fn try_claim(&self, request: &BakeRequest) -> Result<bool, BakeryError> {
        let digest = request.edgepack_key.digest();
        let inserted = self
            .engine()
            .execute(
                "INSERT INTO edgepack_artifacts \
                   (edgepack_key_digest, version_id, recipe_fingerprint, status, updated_at) \
                 VALUES (?1, ?2, ?3, 'claimed', ?4) \
                 ON CONFLICT (edgepack_key_digest) DO NOTHING",
                &[
                    Value::Blob(digest.as_bytes().to_vec()),
                    Value::Blob(crate::ids::version_id::to_blob(&request.package).to_vec()),
                    Value::Text(request.recipe_fingerprint()),
                    Value::Integer(chrono::Utc::now().timestamp_millis()),
                ],
            )
            .map_err(BakeryError::Catalog)?;
        Ok(inserted == 1)
    }

    async fn mark_ready(
        &self,
        digest: ContentHash,
        artifact: ContentHash,
        ram_estimate: i64,
    ) -> Result<(), BakeryError> {
        let now = chrono::Utc::now().timestamp_millis();
        self.engine()
            .execute(
                "UPDATE edgepack_artifacts \
                 SET status = 'ready', artifact_id = ?2, ram_estimate = ?3, \
                     published_at = ?4, updated_at = ?4 \
                 WHERE edgepack_key_digest = ?1",
                &[
                    Value::Blob(digest.as_bytes().to_vec()),
                    Value::Blob(artifact.as_bytes().to_vec()),
                    Value::Integer(ram_estimate),
                    Value::Integer(now),
                ],
            )
            .map_err(BakeryError::Catalog)?;
        Ok(())
    }

    async fn mark_failed(&self, digest: ContentHash) -> Result<(), BakeryError> {
        self.engine()
            .execute(
                "UPDATE edgepack_artifacts SET status = 'failed', updated_at = ?2 \
                 WHERE edgepack_key_digest = ?1",
                &[
                    Value::Blob(digest.as_bytes().to_vec()),
                    Value::Integer(chrono::Utc::now().timestamp_millis()),
                ],
            )
            .map_err(BakeryError::Catalog)?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// STAGED: the vector-bake COMPUTE + worker loop.
//
// The former `server::bakery` also carried `run_bake`/`bake_package`/
// `symbol_payload`/`bakery_worker`: embed the symbol projection through the
// fleet `HttpEmbedder` + `registry::vector::EmbeddingCache`, build a fresh Edge
// shard under the frozen QP1 int8 profile, pack it, and publish the bytes in
// CAS. That compute is composed over the fully-assembled serving stack
// (`Server<M>`/`SourceStores<M>`, the `HttpEmbedder`, the semantic gate) and is
// pure `registry::vector` shard-building — it is index-free. Per §8 it belongs
// to `registry::vector` (or the client/GUI composition), NOT the data layer, so
// only the LEDGER above lands in `index`. The compute is STAGED there.
// ─────────────────────────────────────────────────────────────────────────────
