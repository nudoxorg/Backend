//! The ONNX backend: `FastembedOrt` behind `crate::semantic::Embedder`.
//!
//! Compiled unconditionally (there is no `onnx` cargo feature). Everything
//! `registry`-shaped is confined to this file so the crate's `lib.rs` — and
//! therefore its public surface — never names it.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use crate::semantic::{EmbedRole, Embedder, EmbedderInfo, Error, SharedEmbedder};
use registry::vector::embed::runtime::{FastembedOrt, RuntimeConfig};
use registry::vector::embed::weights::WeightsSpec;
use registry::vector::{EmbedRole as RegistryRole, Embedder as _, EmbeddingModel as _, JinaCodeV2};
use tokio::sync::OnceCell;

/// Largest batch the runtime wants at once.
///
/// Mirrors `registry`'s private `runtime::MAX_BATCH`, which is not re-exported.
/// Duplicating a constant risks drift, so it does not go unwatched:
/// [`tests::static_info_matches_the_loaded_runtime`] loads the real session and
/// asserts this equals the runtime's own `max_batch`, turning a silent drift
/// into a red test the moment `registry` changes it.
const MAX_BATCH: usize = 32;

/// Read the environment and, if a model is configured, build the adapter.
///
/// See the crate docs for the outcome mapping. The model is *not* loaded here —
/// only the directory is captured — so this returns in microseconds and engine
/// startup never waits on a 641 MB read.
pub(crate) fn load_from_env() -> SharedEmbedder {
    let Some(dir) = std::env::var_os(crate::embed::MODEL_DIR_ENV) else {
        tracing::info!(
            env = crate::embed::MODEL_DIR_ENV,
            "semantic: no model directory configured; the section will report \
             Unavailable(NoModelConfigured)"
        );
        return None;
    };
    let dir = PathBuf::from(dir);
    tracing::info!(
        dir = %dir.display(),
        "semantic: ONNX embedder configured; the model loads lazily on the \
         first embedding call, off the engine load path"
    );
    Some(Arc::new(OrtEmbedder::new(dir)))
}

/// `FastembedOrt`, loaded on demand, behind the engine's port.
struct OrtEmbedder {
    spec: WeightsSpec,
    config: RuntimeConfig,
    /// Computed once, from static model facts, so [`Embedder::info`] can answer
    /// before the session exists — the engine calls `info()` (for
    /// `dimensions`) *before* the first `embed_batch`, which is exactly what
    /// makes lazy loading legal here.
    info: EmbedderInfo,
    /// The ORT session, initialised at most once on the first embedding call.
    ///
    /// `OnceCell` rather than an `RwLock<Option<_>>` because the initialiser is
    /// async (the load happens on `spawn_blocking`) and must run exactly once
    /// even under concurrent first calls — the incremental indexer and a live
    /// query can both arrive before the session is warm.
    session: OnceCell<FastembedOrt>,
}

impl OrtEmbedder {
    fn new(dir: PathBuf) -> Self {
        // `dimensions` and the model id are static facts of the model type,
        // not of a loaded session. `durable_canonical` is `true` because the
        // pinned artifact is fp32 (`Quantization::Float32`, batch-invariant);
        // the guard test cross-checks it against the loaded runtime so this
        // cannot silently disagree with what the model actually is.
        let info = EmbedderInfo {
            model_id: JinaCodeV2::id().as_str().into(),
            dimensions: JinaCodeV2::DIMENSIONS,
            max_batch: MAX_BATCH,
            durable_canonical: true,
        };
        Self {
            spec: WeightsSpec::jina_code_v2(dir),
            config: RuntimeConfig::default(),
            info,
            session: OnceCell::new(),
        }
    }

    /// The loaded session, initialising it if this is the first call.
    ///
    /// A load failure (missing weights, sha mismatch, ort init) surfaces as
    /// [`Error::Backend`], which the engine turns into
    /// `Unavailable::ModelFailed` — distinct from `NoModelConfigured`, because
    /// a configured-but-broken model sends the reader somewhere a missing
    /// directory does not. The failure is *not* cached: `get_or_try_init`
    /// leaves the cell empty on `Err`, so a transient failure (a model still
    /// being copied into place) can succeed on a later query rather than
    /// wedging the section for the life of the process.
    async fn session(&self) -> Result<&FastembedOrt, Error> {
        self.session
            .get_or_try_init(|| async {
                FastembedOrt::load(self.spec.clone(), self.config.clone())
                    .await
                    .map_err(|error| {
                        tracing::warn!(%error, "semantic: model load failed");
                        Error::Backend(error.to_string())
                    })
            })
            .await
    }
}

impl Embedder for OrtEmbedder {
    fn info(&self) -> EmbedderInfo {
        self.info.clone()
    }

    fn embed_batch<'a>(
        &'a self,
        texts: &'a [String],
        role: EmbedRole,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Vec<f32>>, Error>> + Send + 'a>> {
        Box::pin(async move {
            let session = self.session().await?;
            let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
            let role = match role {
                EmbedRole::Query => RegistryRole::Query,
                EmbedRole::Document => RegistryRole::Document,
            };
            let embeddings = session
                .embed_batch(&borrowed, role)
                .await
                .map_err(|error| Error::Backend(error.to_string()))?;
            // The engine validates width and finiteness on return; we hand back
            // one `Vec<f32>` per input, in input order, and nothing else.
            Ok(embeddings
                .into_iter()
                .map(|embedding| embedding.as_slice().to_vec())
                .collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model directory the tests use, or a skip.
    ///
    /// Real weights are large and provisioned out of band (docs/LIMITATIONS.md L41),
    /// so a machine without them skips rather than fails — but a machine *with*
    /// them runs a genuine embedding, which is the only thing that proves this
    /// adapter works. Set `NUDOX_EMBED_MODEL_DIR` to the pinned model directory.
    fn model_dir() -> Option<PathBuf> {
        std::env::var_os(crate::embed::MODEL_DIR_ENV).map(PathBuf::from)
    }

    /// The adapter's static `info()` is answerable *without* loading the
    /// session — the property the whole lazy design and the "info before first
    /// embed" ordering in the engine depend on.
    ///
    /// Constructs `OrtEmbedder` over a non-existent path on purpose: `info()`
    /// must not touch the filesystem, so a path that could never load is the
    /// strongest possible witness that it does not. (Deliberately avoids
    /// mutating `NUDOX_EMBED_MODEL_DIR`: the environment is process-global and
    /// the parallel model tests read it — clobbering it here would fail them.)
    #[test]
    fn info_is_answerable_without_loading_the_session() {
        let embedder = OrtEmbedder::new(PathBuf::from("/does/not/need/to/exist"));
        let info = embedder.info();
        assert_eq!(info.dimensions, 768, "jina-code-v2 is 768-dimensional");
        assert_eq!(info.max_batch, MAX_BATCH);
        assert!(
            info.durable_canonical,
            "the pinned fp32 artifact is batch-invariant"
        );
        assert_eq!(&*info.model_id, "jinaai/jina-embeddings-v2-base-code");
    }

    /// The real model embeds a query and a document to finite, unit-length,
    /// 768-wide vectors — and a semantically related pair scores higher than an
    /// unrelated one.
    ///
    /// This is the assertion that cannot pass against a stub (doctrine §4): it
    /// depends on the *content* of the vectors the model produces, not on a
    /// shape or a count. Ignored by default because it needs the ~641 MB model;
    /// run with `--ignored` after `NUDOX_EMBED_MODEL_DIR` points at it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the pinned model weights; run with --ignored"]
    async fn the_real_model_embeds_text_and_ranks_related_above_unrelated() {
        let Some(dir) = model_dir() else {
            eprintln!("SKIP: {} is unset", crate::embed::MODEL_DIR_ENV);
            return;
        };
        let embedder = OrtEmbedder::new(dir);

        let doc = "fn find the first occurrence of a byte within a slice";
        let related = "search a haystack for a single byte and return its index";
        let unrelated = "parse a TOML configuration file into a struct";

        let d = embedder
            .embed_batch(&[doc.to_owned()], EmbedRole::Document)
            .await
            .expect("the model must embed a document");
        assert_eq!(d[0].len(), 768, "the model returns 768-wide vectors");
        assert!(
            d[0].iter().all(|v| v.is_finite()),
            "no component may be NaN or infinite"
        );
        let norm: f32 = d[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-3,
            "the runtime L2-normalises; got norm {norm}"
        );

        let queries = [related.to_owned(), unrelated.to_owned()];
        let q = embedder
            .embed_batch(&queries, EmbedRole::Query)
            .await
            .expect("the model must embed queries");

        let cos = |a: &[f32], b: &[f32]| -> f32 {
            a.iter()
                .zip(b)
                .map(|(x, y)| f64::from(*x) * f64::from(*y))
                .sum::<f64>() as f32
        };
        let related_score = cos(&d[0], &q[0]);
        let unrelated_score = cos(&d[0], &q[1]);
        eprintln!(
            "cost case=embed_ranks_related_above_unrelated dir=. related={related_score:.4} unrelated={unrelated_score:.4}"
        );
        assert!(
            related_score > unrelated_score,
            "a related query ({related_score:.4}) must be closer to the document \
             than an unrelated one ({unrelated_score:.4}) — this is the claim only \
             a working model satisfies"
        );
    }

    /// The static `info()` the adapter answers before loading must equal what
    /// the loaded runtime reports — the guard against `MAX_BATCH` (and the
    /// model id / durability) drifting from `registry`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs the pinned model weights; run with --ignored"]
    async fn static_info_matches_the_loaded_runtime() {
        let Some(dir) = model_dir() else {
            eprintln!("SKIP: {} is unset", crate::embed::MODEL_DIR_ENV);
            return;
        };
        let embedder = OrtEmbedder::new(dir);
        // Force a load.
        let _ = embedder
            .embed_batch(&["warm".to_owned()], EmbedRole::Query)
            .await
            .expect("model loads");
        let runtime = embedder.session().await.unwrap().runtime_info();
        let info = embedder.info();

        assert_eq!(
            info.max_batch, runtime.max_batch,
            "MAX_BATCH here has drifted from registry's runtime"
        );
        assert_eq!(
            info.durable_canonical, runtime.durable_canonical,
            "durability claim has drifted from the loaded model"
        );
        assert_eq!(
            &*info.model_id,
            runtime.model_id.as_str(),
            "model id has drifted from the loaded model"
        );
    }
}
