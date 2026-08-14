//! The production embedder adapter — the host runtime behind
//! [`nudox_engine::semantic::Embedder`].
//!
//! # Why this crate exists, and why it is separate from both sides
//!
//! `nudox-engine` owns the *port* ([`nudox_engine::semantic::Embedder`], an
//! object-safe trait that speaks only `Vec<f32>` / `usize` / `bool`) and the
//! decision of *when* to embed. `registry` owns the *runtime* (`FastembedOrt`,
//! fastembed 5.17.3 over ort 2.0.0-rc.12, running the fp32 Jina code model on
//! CPU). Between them there is a ten-line adapter that belongs to neither, for
//! reasons `docs/AGENTS-DOCTRINE.md` §1 ("Capability ports") records in full:
//!
//! * `registry::vector::core::embed::Embedder` is **not** object-safe (it
//!   carries `type Model: EmbeddingModel`), so the engine cannot hold it without
//!   becoming generic over the model — which would leak a `registry` type
//!   parameter into `EngineConfig`, `EngineHandle`, and therefore `lindsey`.
//! * `registry` + `onnx` resolves to **483 crates**, versus `nudox-engine`
//!   being the single crate `lindsey` depends on; and `ort-sys` fetches a
//!   prebuilt C runtime at build time. Neither belongs under the engine.
//!
//! So the bridge lives here, *beside* the engine on `lindsey`'s argument list
//! rather than beneath it. The whole graph enters a build only when someone
//! turns on this crate's `onnx` feature; a default build compiles the two lines
//! below and nothing else.
//!
//! # The public surface is the port's vocabulary and nothing else
//!
//! Everything this crate exposes is a [`SharedEmbedder`] (i.e.
//! `Option<Arc<dyn Embedder>>`) or a `&str`. No `Embedding<M>`, no `ModelId`, no
//! `registry` type appears in a signature here. That is deliberate and it is
//! what keeps `lindsey`'s dependency on this crate legitimate under §1: a view
//! that depends on `nudox-embed` still cannot name the shape of the IR, or the
//! shape of the vector plane, because neither crosses this seam.
//!
//! # No model is a supported outcome, and it is honest
//!
//! [`load_from_env`] returns `None` — mapping to
//! [`SectionState::Unavailable { NoEmbedder }`](nudox_engine::semantic::SectionState)
//! and the GUI's "not configured in this build" notice — in exactly two cases,
//! and they are indistinguishable to a reader *on purpose*, because both mean
//! "this application, as assembled and configured, has no model":
//!
//! * the `onnx` feature is off (no runtime was compiled in), or
//! * the feature is on but [`MODEL_DIR_ENV`] is unset (no weights were
//!   provisioned).
//!
//! What is *not* collapsed into `None` is a model that is present but broken: a
//! configured-but-unloadable model yields a live embedder whose first call
//! fails, which the engine renders as
//! [`Unavailable::ModelFailed`](nudox_engine::semantic::Unavailable::ModelFailed)
//! — a different place to send the reader (a missing file, a bad hash, an
//! out-of-memory runtime) than "no model at all".

use nudox_engine::semantic::SharedEmbedder;

#[cfg(feature = "onnx")]
mod onnx;

/// The environment variable naming the directory that holds the pinned model.
///
/// The directory must contain `model.onnx` (the fp32
/// `jinaai/jina-embeddings-v2-base-code` artifact, 641,517,466 bytes, whose
/// sha256 is pinned and verified by `registry`'s `WeightsSpec` before a single
/// byte reaches ort) alongside its four tokenizer sidecars (`tokenizer.json`,
/// `config.json`, `special_tokens_map.json`, `tokenizer_config.json`).
///
/// This is the same variable the `registry` relevance test reads, so a model
/// directory provisioned for one works unchanged for the other.
pub const MODEL_DIR_ENV: &str = "NUDOX_EMBED_MODEL_DIR";

/// Build the embedder this process should install on its engine, from the
/// environment — or `None` if this build/configuration has no model.
///
/// Always present regardless of features, so the caller (`lindsey`'s `main.rs`,
/// or any other host) writes one unconditional line and lets the crate decide
/// what is possible. The returned value is handed straight to
/// [`EngineConfig::embedder`](nudox_engine::runtime::EngineConfig::embedder).
///
/// With the `onnx` feature off this is a compile-time `None`. With it on it
/// reads [`MODEL_DIR_ENV`]; see the module docs for the exact mapping of
/// outcomes onto the section state a reader will see.
pub fn load_from_env() -> SharedEmbedder {
    #[cfg(feature = "onnx")]
    {
        onnx::load_from_env()
    }
    #[cfg(not(feature = "onnx"))]
    {
        None
    }
}

#[cfg(all(test, not(feature = "onnx")))]
mod tests {
    use super::*;

    /// The no-runtime build is representable and its answer is `None`.
    ///
    /// Under `--features onnx` this still holds whenever `MODEL_DIR_ENV` is
    /// unset, which is why it does not assert the negative branch specifically —
    /// it pins the one invariant true in every build: a host that calls this and
    /// gets `None` has a supported, honest "no model" configuration, never a
    /// panic.
    #[test]
    fn a_build_without_the_onnx_feature_installs_no_embedder() {
        assert!(
            load_from_env().is_none(),
            "with no ONNX runtime compiled in, there is no model to install"
        );
    }
}
