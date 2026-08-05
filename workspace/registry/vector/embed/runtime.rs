//! `FastembedOrt` — the production local embedder (feature `onnx`).
//!
//! fastembed 5.17.3 over ort 2.0.0-rc.12, wired for the frozen invariants:
//!
//! - **Pinned artifact, no auto-download (I11/I16).** We load through
//!   fastembed's user-defined-model path — [`fastembed::TextEmbedding::
//!   try_new_from_user_defined`] with [`fastembed::UserDefinedEmbeddingModel`]
//!   — which takes raw bytes we read from an explicit, sha256-verified
//!   directory. The catalog path (`EmbeddingModel::JinaEmbeddingsV2BaseCode` +
//!   hf-hub download, `FASTEMBED_CACHE_DIR`) is deliberately not used: it
//!   fetches `onnx/model.onnx` at first use, and an unpinned artifact is a bug.
//! - **CPU execution provider only (I11).** The execution-provider list is
//!   left empty, which in ort means the default CPU EP — no accelerated EP is
//!   ever registered here, so every vector is durable-canonical. (Also the
//!   pragmatic choice: int8-quantized ONNX on GPU EPs is a known failure mode,
//!   09c §3.2.)
//! - **Batch hard cap 32 (I16).** fastembed's `embed(texts, None)` defaults to
//!   batch 256; we always pass `Some(32)` and fastembed chunks internally.
//! - **Sync API off the async runtime (09c §3.2).** `TextEmbedding::embed`
//!   takes `&mut self` and blocks; calls go through `spawn_blocking` around a
//!   `Mutex` — one session, serialized `Run`.

use std::sync::{Arc, Mutex};

use crate::vector::core::{
    AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel as _,
    JinaCodeV2, l2_normalize,
};
use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use super::weights::{WeightsError, WeightsSpec};
use super::{MAX_BATCH, MAX_SEQ_LEN};

/// The ort package identity folded into `tool_digest` (09c I12): the exact
/// dependency fastembed 5.17.3 pins. Bump in lockstep with the `fastembed`
/// dependency — a silent runtime swap must invalidate traces.
/// Re-exported from [`super::ORT_PACKAGE_ID`] for callers that import via
/// `vector_embed::runtime::ORT_PACKAGE_ID`.
pub use super::ORT_PACKAGE_ID;

/// Tuning knobs (09c §4.2). Threads default to `min(4, cores/2)` so the
/// embedder coexists with the GUI instead of oversubscribing it.
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    /// ORT intra-op threads; `None` = `min(4, available_parallelism / 2)`.
    pub intra_threads: Option<usize>,
}

impl RuntimeConfig {
    fn resolved_intra_threads(&self) -> usize {
        self.intra_threads.unwrap_or_else(|| {
            let cores = std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(4);
            (cores / 2).clamp(1, 4)
        })
    }
}

/// Why the runtime could not come up. Callers hitting `Weights(Missing…)`
/// disable semantic search gracefully rather than failing the app.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeInitError {
    #[error(transparent)]
    Weights(#[from] WeightsError),

    #[error("reading tokenizer sidecar {path}: {source}")]
    Sidecar {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("fastembed session init: {0}")]
    Fastembed(String),
}

/// The CPU-canonical local embedder for [`JinaCodeV2`].
pub struct FastembedOrt {
    /// One ORT session; `TextEmbedding::embed` takes `&mut self`, and
    /// concurrent `Run` on a session is a footgun anyway (09c §4.3) — so a
    /// plain mutex, locked inside `spawn_blocking`.
    inner: Arc<Mutex<TextEmbedding>>,
    /// The actual sha256 of the loaded ONNX file (always known: we hash while
    /// verifying, pinned or not).
    weights_sha256: [u8; 32],
    intra_threads: usize,
}

impl FastembedOrt {
    /// Load from an explicit, verified model directory. Blocking (file reads
    /// + session build); prefer [`FastembedOrt::load`] on the runtime.
    pub fn load_blocking(
        spec: &WeightsSpec,
        config: &RuntimeConfig,
    ) -> Result<Self, RuntimeInitError> {
        // Pinned-artifact policy first: hash before load; a mismatch never
        // reaches ort (I11).
        let verified = spec.verify()?;
        let onnx_bytes = std::fs::read(&verified.path).map_err(|source| WeightsError::Io {
            path: verified.path.clone(),
            source,
        })?;

        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_sidecar(spec, "tokenizer.json")?,
            config_file: read_sidecar(spec, "config.json")?,
            special_tokens_map_file: read_sidecar(spec, "special_tokens_map.json")?,
            tokenizer_config_file: read_sidecar(spec, "tokenizer_config.json")?,
        };

        // Mean pooling over the attention mask is required by the Jina v2
        // model card (09b §3.1b) — the user-defined path does not infer it.
        let model =
            UserDefinedEmbeddingModel::new(onnx_bytes, tokenizer_files).with_pooling(Pooling::Mean);

        // Empty EP list = ort's default CPU execution provider, and nothing
        // else. Durable-canonical by construction.
        let intra_threads = config.resolved_intra_threads();
        let options = InitOptionsUserDefined::default()
            .with_execution_providers(Vec::new())
            .with_max_length(MAX_SEQ_LEN)
            .with_intra_threads(intra_threads);

        let session = TextEmbedding::try_new_from_user_defined(model, options)
            .map_err(|error| RuntimeInitError::Fastembed(error.to_string()))?;

        tracing::info!(
            path = %verified.path.display(),
            bytes = verified.bytes,
            intra_threads,
            "fastembed session loaded (CPU EP)"
        );

        Ok(Self {
            inner: Arc::new(Mutex::new(session)),
            weights_sha256: verified.sha256,
            intra_threads,
        })
    }

    /// Async wrapper for [`Self::load_blocking`].
    pub async fn load(spec: WeightsSpec, config: RuntimeConfig) -> Result<Self, RuntimeInitError> {
        tokio::task::spawn_blocking(move || Self::load_blocking(&spec, &config))
            .await
            .map_err(|join| RuntimeInitError::Fastembed(join.to_string()))?
    }

    /// A factory closure for [`super::gate::EmbedGate`], so the gate can
    /// idle-unload the session and lazily reload it.
    pub fn factory(spec: WeightsSpec, config: RuntimeConfig) -> super::gate::EmbedderFactory<Self> {
        Box::new(move || {
            let (spec, config) = (spec.clone(), config.clone());
            Box::pin(async move {
                Self::load(spec, config)
                    .await
                    .map_err(|error| super::backend_error(error))
            })
        })
    }

    /// Runtime info for *this* instance — includes the verified weights sha,
    /// which the static trait-level [`Embedder::runtime`] cannot know.
    pub fn runtime_info(&self) -> EmbedRuntimeInfo {
        EmbedRuntimeInfo {
            weights_sha256: Some(self.weights_sha256),
            ..self.runtime()
        }
    }

    /// Post-process one raw fastembed row into a validated [`Embedding`]:
    /// reject non-finite values, then L2-normalize defensively — the model
    /// pipeline should already emit normalized mean-pooled vectors, but
    /// cosine correctness is too important to assume (belt-and-braces,
    /// 09b §3.1b: "L2-normalize at write and at query").
    fn finish(mut row: Vec<f32>) -> Result<Embedding<JinaCodeV2>, EmbedError> {
        if !row.iter().copied().all(f32::is_finite) {
            return Err(super::backend_error(
                "model produced a non-finite embedding",
            ));
        }
        l2_normalize(&mut row);
        Embedding::from_vec(row)
    }
}

fn read_sidecar(spec: &WeightsSpec, name: &str) -> Result<Vec<u8>, RuntimeInitError> {
    let path = spec.sidecar(name);
    std::fs::read(&path).map_err(|source| RuntimeInitError::Sidecar { path, source })
}

#[async_trait::async_trait]
impl Embedder for FastembedOrt {
    type Model = JinaCodeV2;

    async fn embed(
        &self,
        text: &str,
        role: EmbedRole,
    ) -> Result<Embedding<JinaCodeV2>, EmbedError> {
        let mut batch = self.embed_batch(&[text], role).await?;
        Ok(batch.pop().expect("one vector for one text"))
    }

    /// `role` is accepted for trait parity but intentionally unused: Jina v2
    /// code is a *symmetric* encoder — the model card defines no instruction
    /// prefixes and no query/document asymmetry, so both sides go through the
    /// identical encoder (09b §3.1b). Voyage (`input_type`) and E5-family
    /// (`"query: "` / `"passage: "` prefixes) adapters do use it.
    async fn embed_batch(
        &self,
        texts: &[&str],
        _role: EmbedRole,
    ) -> Result<Vec<Embedding<JinaCodeV2>>, EmbedError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        let inner = Arc::clone(&self.inner);

        // fastembed is sync and `embed` takes `&mut self`: lock + run on the
        // blocking pool. `Some(MAX_BATCH)` overrides the 256 default; for
        // larger inputs fastembed chunks internally at 32 per forward pass.
        let rows = tokio::task::spawn_blocking(move || {
            let mut session = inner.lock().expect("embedder session poisoned");
            session.embed(&owned, Some(MAX_BATCH))
        })
        .await
        .map_err(|join| super::backend_error(join))?
        .map_err(|error| super::backend_error(error))?;

        rows.into_iter().map(Self::finish).collect()
    }

    fn runtime(&self) -> EmbedRuntimeInfo {
        EmbedRuntimeInfo {
            model_id: JinaCodeV2::id(),
            accel: AccelKind::Cpu,
            durable_canonical: true,
            max_batch: MAX_BATCH,
            max_seq_len: MAX_SEQ_LEN,
            // Instance-independent view; the verified per-instance sha is on
            // `FastembedOrt::runtime_info`.
            weights_sha256: None,
            ort_package_id: ORT_PACKAGE_ID.into(),
        }
    }
}
