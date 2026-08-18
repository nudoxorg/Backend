//! The production embedder adapter — the host runtime behind
//! [`crate::semantic::Embedder`].
//!
//! # Why this crate exists, and why it is separate from both sides
//!
//! `nudox-engine` owns the *port* ([`crate::semantic::Embedder`], an
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
//!
//! So the bridge lives here, *beside* the engine on `lindsey`'s argument list
//! rather than beneath it.
//!
//! # There is no `onnx` cargo feature
//!
//! `registry` (with its `onnx` feature) is a plain, non-optional dependency of
//! this crate: `registry` + `onnx` resolves to **483 crates**, and `ort-sys`
//! fetches a prebuilt C runtime at build time, on *every* build of this crate
//! — including a bare `cargo check`. There is no way to opt out. This is a
//! deliberate product decision (semantic search must always be one environment
//! variable away, never a rebuild), and it means a build environment with no
//! network access cannot build this crate at all; see `docs/LIMITATIONS.md`
//! for what that costs the workspace's hermetic Nix checks.
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
//! The runtime is always compiled in, so [`load_from_env`] returns `None` in
//! exactly one case: [`MODEL_DIR_ENV`] is unset (no weights were
//! provisioned) — the reader must **set one environment variable**; no
//! rebuild is ever involved, because there is no feature left to rebuild
//! with. [`unavailable_reason`] names that case as
//! [`Unavailable::NoModelConfigured`], which is what
//! [`SectionState::Unavailable`](crate::semantic::SectionState) reports and
//! what the GUI and MCP surface render, with the matching
//! [`remedy`](crate::semantic::Unavailable::remedy).
//!
//! This is also why [`Unavailable`](crate::semantic::Unavailable)'s old
//! `NoRuntime` variant does not exist any more: it named "this build has no
//! embedding runtime compiled in", a state that became unreachable the day
//! `onnx` stopped being a cargo feature. Keeping a variant no code path can
//! ever produce would
//! have forced every consumer to carry a permanently-dead match arm; removing
//! it instead means the compiler breaks every one of those matches once, here,
//! rather than leaving a reader to wonder whether it was actually still
//! possible. See `docs/LIMITATIONS.md` for the build-time cost of always
//! compiling the runtime in.
//!
//! Not collapsed into [`Unavailable::NoModelConfigured`] is a model that is
//! present but broken: a configured-but-unloadable model yields a live
//! embedder whose first call fails, which the engine renders as
//! [`Unavailable::ModelFailed`](crate::semantic::Unavailable::ModelFailed)
//! — a different place to send the reader (a missing file, a bad hash, an
//! out-of-memory runtime) than "no model at all".

use crate::semantic::{SharedEmbedder, Unavailable};

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
/// environment — or `None` if [`MODEL_DIR_ENV`] is unset.
///
/// One unconditional line for the caller (`lindsey`'s `main.rs`, or any other
/// host): the runtime is always compiled in, so this always reads
/// [`MODEL_DIR_ENV`] and either loads the pinned model or returns `None`. The
/// returned value is handed straight to
/// [`EngineConfig::embedder`](crate::runtime::EngineConfig::embedder).
pub fn load_from_env() -> SharedEmbedder {
    onnx::load_from_env()
}

/// Why [`load_from_env`] would return (or did return) `None` — `None` when an
/// embedder is available.
///
/// This is the split [`load_from_env`] itself does not make: that function
/// answers *whether* this process has a model, and callers that only need a
/// working embedder never had to care why not. A reader staring at an empty
/// semantic section does care. This function makes the distinction the caller
/// (`search::mod`, for what to put on
/// [`SectionState::Unavailable`](crate::semantic::SectionState)) actually
/// needs — today that is always [`Unavailable::NoModelConfigured`], because
/// the runtime is never absent.
///
/// Cheap and side-effect-free like [`load_from_env`]'s own decision: it reads,
/// at most, one environment variable, never the filesystem.
pub fn unavailable_reason() -> Option<Unavailable> {
    if std::env::var_os(MODEL_DIR_ENV).is_some() {
        None
    } else {
        Some(Unavailable::NoModelConfigured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build with no model directory configured is representable and
    /// honest, never a panic.
    ///
    /// Reads the ambient environment rather than mutating it: `MODEL_DIR_ENV`
    /// is process-global (see `embed::onnx`'s own tests for why that rules out
    /// `std::env::set_var` here), and `cargo test`'s default parallelism runs
    /// this alongside other tests in the same process — `search::mod`'s unit
    /// tests among them, which read this same variable while computing a
    /// semantic section's `Unavailable` reason.
    #[test]
    fn no_model_directory_installs_no_embedder() {
        if std::env::var_os(MODEL_DIR_ENV).is_some() {
            eprintln!("SKIP: {MODEL_DIR_ENV} is set in this environment");
            return;
        }

        assert!(
            load_from_env().is_none(),
            "with no model directory configured, there is no model to install"
        );
        assert_eq!(
            unavailable_reason(),
            Some(Unavailable::NoModelConfigured),
            "the reason must name the one remaining configuration gap"
        );
    }
}
