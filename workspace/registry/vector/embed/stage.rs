//! `EmbedStage` — the durable-vector write path (09b §4.1, §16).
//!
//! A pure Stage consumer of `SymbolDelta`: it **never walks the project** and
//! **never touches unchanged symbols** (09b §16.1 acceptance). Layered early
//! cutoffs, all keyed by the frozen `embed_key` (09b §3.3):
//!
//! ```text
//! removed            → store.delete(point_id)
//! added ∪ changed    → k = embed_key(sym)
//!   L1 stage_traces (stage_id, k, tool_digest) hit → skip entirely
//!   L2 vector CAS has blob for k                   → upsert from CAS, no infer
//!   else build_embed_text → scheduler (Background) → CAS.put → upsert → trace.put
//! ```
//!
//! Trace-put runs **last**, after the vector is durable in CAS + store, so a
//! crash mid-run re-does only the missing tail on the next seal (09b §16.9 —
//! every step is idempotent under its key).
//!
//! Acceptance (09b §16.4): a doc-only edit of one symbol in any-size corpus
//! is exactly `infer_count == 1 && upsert_count == 1`.

use crate::vector::core::key::{self, SymbolDelta, SymbolPartHashes};
use crate::vector::core::model::{EmbeddingModel, Metric, ModelId};
use crate::vector::core::recipe::{
    EmbedFacetsBuf, RECIPE_ID, TokenCounter, VectorName, build_embed_text,
};
use crate::vector::core::store::{Payload, PointId, VectorPoint, VectorStore};
use crate::vector::core::StoreError;
use crate::vector::core::{EmbedRole, Embedding, JinaCodeV2};
use async_trait::async_trait;
use heart::{ContentHash, SymbolId};

use super::MAX_BATCH;
use super::scheduler::{self, CancelGroup, EmbedHandle, Priority};

/// Stage identity for stage-trace rows. v1 embeds the `sym` facet only.
/// The recipe revision itself is [`RECIPE_ID`] (vector-core), folded into
/// every embed key and the tool digest — bumping it invalidates every trace
/// by design (09b §16.2).
pub const STAGE_ID: &str = "embed.v2.sym";

/// L1 cutoff: the stage-trace store. A row `(stage_id, input_digest,
/// tool_digest)` asserts "this exact work is already durable" — hit means the
/// stage skips the symbol entirely, not even an upsert.
#[async_trait]
pub trait TraceStore: Send + Sync {
    async fn has(
        &self,
        stage_id: &str,
        input_digest: &ContentHash,
        tool_digest: &ContentHash,
    ) -> bool;

    async fn put(&self, stage_id: &str, input_digest: &ContentHash, tool_digest: &ContentHash);
}

/// L2 cutoff: the raw-vector CAS, keyed by `embed_key`. Blobs are raw f32 —
/// store-side quantization is a store property, never baked into the blob
/// (09b §16.2).
#[async_trait]
pub trait VectorCas: Send + Sync {
    async fn get(&self, key: &ContentHash) -> Option<Vec<f32>>;
    async fn put(&self, key: &ContentHash, vector: &[f32]);
}

/// Stage configuration: the identity values folded into keys and payloads.
#[derive(Debug, Clone)]
pub struct StageConfig {
    /// Part of `embed_key` — a model bump is a global invalidation (I2).
    pub model_id: ModelId,
    /// ort/ONNX package identity, part of `tool_digest` (09c I12).
    pub ort_package_id: String,
    /// The pinned weights sha, once operationally frozen; part of
    /// `tool_digest` (I12) — unpinned digests differently from any pin.
    pub weights_sha256: Option<[u8; 32]>,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            model_id: JinaCodeV2::id(),
            ort_package_id: super::ORT_PACKAGE_ID.to_owned(),
            weights_sha256: None,
        }
    }
}

/// Counters exposed for acceptance tests (09b §16.4) and telemetry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageReport {
    /// Texts that actually went through the model.
    pub infer_count: usize,
    /// Points upserted (freshly inferred + CAS rehydrations).
    pub upsert_count: usize,
    /// Symbols skipped by an L1 stage-trace hit.
    pub skip_trace: usize,
    /// Symbols upserted from an L2 CAS hit without inference.
    pub skip_cas: usize,
    /// Points deleted for removed symbols.
    pub deleted: usize,
    /// Delta entries whose facets could not be produced (logged + skipped).
    pub missing_facets: usize,
}

/// Why a stage run failed. A failed stage must not seal the generation
/// (09b §16.9).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("embed stage cancelled")]
    Cancelled,
    #[error("vector store: {0}")]
    Store(#[from] StoreError),
    #[error("embedding: {0}")]
    Embed(crate::vector::core::EmbedError),
}

impl From<scheduler::Error> for Error {
    fn from(error: scheduler::Error) -> Self {
        match error {
            scheduler::Error::Cancelled => Error::Cancelled,
            scheduler::Error::Closed => {
                Error::Embed(super::backend_error("embed scheduler shut down"))
            }
            scheduler::Error::Embed(inner) => Error::Embed(inner),
        }
    }
}

/// The stage itself. Generic over the store; traces/CAS/token-counter are
/// trait objects so tests run fully offline.
pub struct EmbedStage<S: VectorStore<JinaCodeV2>> {
    store: S,
    traces: Box<dyn TraceStore>,
    cas: Box<dyn VectorCas>,
    scheduler: EmbedHandle<JinaCodeV2>,
    tokens: Box<dyn TokenCounter + Send + Sync>,
    config: StageConfig,
    tool_digest: ContentHash,
}

impl<S: VectorStore<JinaCodeV2>> EmbedStage<S> {
    pub fn new(
        store: S,
        traces: Box<dyn TraceStore>,
        cas: Box<dyn VectorCas>,
        scheduler: EmbedHandle<JinaCodeV2>,
        tokens: Box<dyn TokenCounter + Send + Sync>,
        config: StageConfig,
    ) -> Self {
        // tool_digest fingerprints recipe + model + runtime package + weights
        // pin + geometry (09b §16.2, 09c I12) — never ambient hardware.
        let tool_digest = key::tool_digest(
            &config.model_id,
            &config.ort_package_id,
            config.weights_sha256.as_ref(),
            JinaCodeV2::DIMENSIONS,
        );
        Self {
            store,
            traces,
            cas,
            scheduler,
            tokens,
            config,
            tool_digest,
        }
    }

    /// The store, for read-side wiring.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Run the stage over one sealed-generation delta.
    ///
    /// `facets_for` produces the embed surface for a symbol id; a `None`
    /// (symbol vanished between delta and stage) is logged and skipped.
    pub async fn run(
        &self,
        delta: &SymbolDelta,
        facets_for: &dyn Fn(&SymbolId) -> Option<EmbedFacetsBuf>,
        cancel: CancelGroup,
    ) -> Result<StageReport, Error> {
        let mut report = StageReport::default();

        self.delete_removed(&delta.removed, &mut report).await?;

        // added ∪ changed — and *nothing else*: unchanged symbols never enter
        // this loop (09b §16.1).
        let work: Vec<(&SymbolId, &SymbolPartHashes)> = delta
            .added
            .iter()
            .map(|(id, parts)| (id, parts))
            .chain(delta.changed.iter().map(|change| (&change.id, &change.new)))
            .collect();

        for chunk in work.chunks(MAX_BATCH) {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            self.run_chunk(chunk, facets_for, &cancel, &mut report)
                .await?;
        }

        tracing::info!(?report, stage = STAGE_ID, "embed stage complete");
        Ok(report)
    }

    async fn delete_removed(
        &self,
        removed: &[SymbolId],
        report: &mut StageReport,
    ) -> Result<(), Error> {
        if removed.is_empty() {
            return Ok(());
        }
        // Point ids are stable uuid-v5 of SymbolId, so delete needs no key
        // computation. Orphaned trace rows are left for GC (09b §16.5).
        let ids: Vec<PointId> = removed.iter().map(PointId::from_symbol).collect();
        self.store.delete(&ids).await?;
        report.deleted += ids.len();
        Ok(())
    }

    async fn run_chunk(
        &self,
        chunk: &[(&SymbolId, &SymbolPartHashes)],
        facets_for: &dyn Fn(&SymbolId) -> Option<EmbedFacetsBuf>,
        cancel: &CancelGroup,
        report: &mut StageReport,
    ) -> Result<(), Error> {
        // Phase 1 — classify each symbol through the cutoff ladder.
        let mut from_cas: Vec<(PointId, EmbedFacetsBuf, ContentHash, Vec<f32>)> = Vec::new();
        let mut need_infer: Vec<(PointId, EmbedFacetsBuf, ContentHash)> = Vec::new();

        for (id, parts) in chunk.iter().copied() {
            let Some(facets) = facets_for(id) else {
                tracing::warn!(symbol = %id, "no facets for delta symbol; skipping");
                report.missing_facets += 1;
                continue;
            };

            let embed_key = key::embed_key(
                &self.config.model_id,
                VectorName::Sym,
                JinaCodeV2::DIMENSIONS,
                Metric::Cosine,
                parts,
                facets.moniker.as_bytes(),
                &facets.kind,
            );

            // L1: this exact (input, tool) pair is already durable.
            if self
                .traces
                .has(STAGE_ID, &embed_key, &self.tool_digest)
                .await
            {
                report.skip_trace += 1;
                continue;
            }

            let point_id = PointId::from_symbol(id);

            // L2: the vector blob exists — rehydrate without inference.
            if let Some(vector) = self.cas.get(&embed_key).await {
                report.skip_cas += 1;
                from_cas.push((point_id, facets, embed_key, vector));
                continue;
            }

            need_infer.push((point_id, facets, embed_key));
        }

        // Phase 2 — submit all inferences concurrently so the scheduler can
        // coalesce them into real batches (Background priority: this is the
        // commit path, not a user query).
        let mut inferred: Vec<(PointId, EmbedFacetsBuf, ContentHash, Embedding<JinaCodeV2>)> =
            Vec::new();
        let jobs: Vec<_> = need_infer
            .into_iter()
            .map(|(point_id, facets, embed_key)| {
                let text =
                    build_embed_text(&facets.as_facets(), VectorName::Sym, self.tokens.as_ref());
                let scheduler = self.scheduler.clone();
                let cancel = cancel.clone();
                let task = tokio::spawn(async move {
                    scheduler
                        .embed(
                            embed_key,
                            text.into_inner(),
                            EmbedRole::Document,
                            Priority::Background,
                            cancel,
                        )
                        .await
                });
                (point_id, facets, embed_key, task)
            })
            .collect();

        for (point_id, facets, embed_key, task) in jobs {
            let embedding = task
                .await
                .map_err(|join| Error::Embed(super::backend_error(join)))??;
            self.cas
                .put(&embed_key, super::embedding_floats(&embedding))
                .await;
            report.infer_count += 1;
            inferred.push((point_id, facets, embed_key, embedding));
        }

        // Phase 3 — upsert everything, then write traces (durability order:
        // CAS → store → trace, so a trace row always implies durable work).
        let mut points = Vec::new();
        let mut trace_keys = Vec::new();

        for (point_id, facets, embed_key, vector) in from_cas {
            let embedding = Embedding::from_vec(vector).map_err(Error::Embed)?;
            points.push(self.point(point_id, embedding, &facets, &embed_key));
            trace_keys.push(embed_key);
        }
        for (point_id, facets, embed_key, embedding) in inferred {
            points.push(self.point(point_id, embedding, &facets, &embed_key));
            trace_keys.push(embed_key);
        }

        if !points.is_empty() {
            report.upsert_count += points.len();
            self.store.upsert(points).await?;
            for embed_key in &trace_keys {
                self.traces
                    .put(STAGE_ID, embed_key, &self.tool_digest)
                    .await;
            }
        }
        Ok(())
    }

    /// Assemble the point: stable id, vector, and the defense-in-depth
    /// payload (09c §6.4) used for filtering, display, and skip-without-
    /// recompute (`embed_key` mirrored onto the point).
    fn point(
        &self,
        id: PointId,
        vector: Embedding<JinaCodeV2>,
        facets: &EmbedFacetsBuf,
        embed_key: &ContentHash,
    ) -> VectorPoint<JinaCodeV2> {
        let mut payload = Payload::default();
        payload.insert("language".into(), facets.language.as_token().into());
        payload.insert("package".into(), facets.package_stem.as_str().into());
        payload.insert("kind".into(), facets.kind.as_str().into());
        payload.insert("moniker".into(), facets.moniker.as_str().into());
        payload.insert("embed_key".into(), embed_key.to_string().into());
        payload.insert("model_id".into(), self.config.model_id.as_str().into());
        payload.insert("recipe_id".into(), RECIPE_ID.into());
        VectorPoint {
            id,
            vector,
            payload,
        }
    }
}
