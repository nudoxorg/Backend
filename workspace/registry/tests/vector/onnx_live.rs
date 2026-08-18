//! Live-model tests (feature `onnx`). These need real weights on disk and are
//! `#[ignore]`-gated: opt in with
//!
//! ```sh
//! NUDOX_EMBED_MODEL_DIR=/path/to/jina-code-v2 \
//!     cargo test -p registry --features onnx --test vector_onnx_live -- --ignored
//! ```
//!
//! The directory must contain `model_quantized.onnx` plus the tokenizer
//! sidecars (`tokenizer.json`, `config.json`, `special_tokens_map.json`,
//! `tokenizer_config.json`) from the pinned
//! `jinaai/jina-embeddings-v2-base-code` artifact, all flat in one directory
//! (on HuggingFace the ONNX file lives under `onnx/` while the sidecars sit at
//! the repo root, so whatever provisions this has to flatten them).
//!
//! # Why this file did not compile for so long
//!
//! Until 2026-08-08 the `[[test]]` target carried no `required-features`, so
//! with `onnx` off — which is every build in this repo — the inner
//! `#![cfg(feature = "onnx")]` emptied the crate and the target reported
//! `0 tests; 0 passed`. A green line that had never compiled its own contents,
//! in a manifest whose own comment says the explicit `[[test]]` blocks exist to
//! stop exactly that. It had duplicate `#![cfg]` attributes and a missing trait
//! import, and the invocation above named `-p vector-embed`, a package that no
//! longer exists — three rots that a single compile would have caught.
//!
//! `required-features` now makes cargo refuse the target up front instead of
//! compiling it to nothing, so silence is no longer the failure mode.

use registry::vector::embed::runtime::{FastembedOrt, RuntimeConfig};
use registry::vector::embed::tokens::HfTokenCounter;
use registry::vector::embed::weights::WeightsSpec;
use registry::vector::recipe::TokenCounter;
use registry::vector::{EmbedRole, Embedder, EmbeddingModel as _, JinaCodeV2};

const ENV_DIR: &str = "NUDOX_EMBED_MODEL_DIR";

fn model_dir() -> std::path::PathBuf {
    std::env::var_os(ENV_DIR)
        .map(Into::into)
        .unwrap_or_else(|| panic!("set {ENV_DIR} to the pinned model directory to run this test"))
}

#[tokio::test]
#[ignore = "needs real model weights: set NUDOX_EMBED_MODEL_DIR and run with --ignored"]
async fn live_model_embeds_unit_vectors() {
    let spec = WeightsSpec::jina_code_v2(model_dir());
    let embedder = FastembedOrt::load(spec, RuntimeConfig::default())
        .await
        .expect("load pinned model");

    let info = embedder.runtime_info();
    assert!(
        info.durable_canonical,
        "CPU EP is the durable-canonical plane"
    );
    assert!(info.weights_sha256.is_some(), "verified sha reported");

    let embedding = embedder
        .embed(
            "fn parse(input: &str) -> Result<Ast, Error>",
            EmbedRole::Document,
        )
        .await
        .expect("embed");
    let floats: &[f32] = embedding.as_ref();
    assert_eq!(floats.len(), JinaCodeV2::DIMENSIONS);

    let norm: f32 = floats.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "L2-normalized output, got norm {norm}"
    );
}

#[tokio::test]
#[ignore = "needs real model weights: set NUDOX_EMBED_MODEL_DIR and run with --ignored"]
async fn live_model_role_is_symmetric() {
    // Jina v2 code has no query/doc asymmetry — both roles must produce the
    // identical vector (09b §3.1b).
    let spec = WeightsSpec::jina_code_v2(model_dir());
    let embedder = FastembedOrt::load(spec, RuntimeConfig::default())
        .await
        .expect("load");

    let text = "impl Iterator for Lexer";
    let doc = embedder
        .embed(text, EmbedRole::Document)
        .await
        .expect("doc embed");
    let query = embedder
        .embed(text, EmbedRole::Query)
        .await
        .expect("query embed");
    let (doc_floats, query_floats): (&[f32], &[f32]) = (doc.as_ref(), query.as_ref());
    assert_eq!(doc_floats, query_floats);
}

/// A vector must not depend on what it was batched with.
///
/// # Why this test exists
///
/// `EmbedRuntimeInfo::durable_canonical: true` is a promise that a vector is
/// reproducible — it is what lets vectors be *published* and compared against
/// vectors computed by a different process at a different time. Everything
/// downstream (the CAS blobs, the edgepack keys, cosine ranking across shards)
/// assumes it.
///
/// The artifact we load is `model_quantized.onnx`, and quantization is exactly
/// where that promise can break. fastembed distinguishes `QuantizationMode::
/// Dynamic` from `Static` and *refuses* a batch size smaller than the input for
/// dynamic models, on the stated grounds that dynamic quantization "adjust[s]
/// the data range to fit each batch, making the embeddings incompatible across
/// batches". `runtime.rs` builds its model through the user-defined path, which
/// carries no `QuantizationMode` at all — so nothing in the load path can tell
/// us which kind we have, and no error would be raised if it were the dangerous
/// kind.
///
/// The other three tests in this file cannot see this: they each embed a single
/// string, and a batch of one is identical under either quantization mode. They
/// would pass just as happily against a model whose vectors silently varied
/// with batch composition — which would corrupt the index in a way that shows
/// up months later as bad search results, never as a failure at the call site.
///
/// So this asserts the property directly: the same text, embedded alone and
/// embedded alongside unrelated neighbours, must produce the same vector. It is
/// deliberately stricter than "cosine similarity is high" — near-identical is
/// the symptom of batch dependence, not a refutation of it.
#[tokio::test]
#[ignore = "needs real model weights: set NUDOX_EMBED_MODEL_DIR and run with --ignored"]
async fn live_model_vectors_do_not_depend_on_batch_composition() {
    let spec = WeightsSpec::jina_code_v2(model_dir());
    let embedder = FastembedOrt::load(spec, RuntimeConfig::default())
        .await
        .expect("load");

    let subject = "fn parse(input: &str) -> Result<Ast, Error>";

    // Alone.
    let solo = embedder
        .embed(subject, EmbedRole::Document)
        .await
        .expect("solo embed");

    // Same text, first in a batch, surrounded by unrelated neighbours. If the
    // data range is refit per batch, these neighbours move it.
    let neighbours = [
        subject,
        "class HttpServer { listen(port) { /* ... */ } }",
        "SELECT id, name FROM users WHERE created_at > ?",
        "def train(model, epochs=10): return model.fit(epochs)",
        "a very short string",
        "█▓▒░ not code at all ░▒▓█",
    ];
    let batched = embedder
        .embed_batch(&neighbours, EmbedRole::Document)
        .await
        .expect("batch embed");

    let (solo_f, batched_f): (&[f32], &[f32]) = (solo.as_ref(), batched[0].as_ref());
    assert_eq!(
        solo_f.len(),
        batched_f.len(),
        "dimension changed between solo and batch"
    );

    // Exact equality is the honest bar for a *static* quantization: the same
    // bytes through the same graph. A tiny tolerance is allowed only for
    // non-determinism in threaded reduction order, not for range refitting —
    // dynamic requantization moves values far more than this.
    let worst = solo_f
        .iter()
        .zip(batched_f)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst < 1e-5,
        "the same text embedded alone and in a batch differs by {worst} in some \
		 component. That means the vector depends on its batch neighbours, so \
		 `durable_canonical: true` (vector/core/embed.rs) is a false claim for \
		 this artifact and nothing computed from it may be published: two runs \
		 that batch differently would disagree. Most likely `model_quantized.onnx` \
		 is DYNAMICALLY quantized, in which case the fix is a statically-quantized \
		 or fp32 artifact — not a looser tolerance here."
    );

    // A batch larger than the frozen cap must chunk internally without changing
    // results: MAX_BATCH is 32, so 40 forces at least two internal chunks, and a
    // chunk boundary is precisely where a per-batch range refit would show.
    let many: Vec<String> = (0..40)
        .map(|i| format!("fn f{i}(x: u{i}) -> u{i} {{ x }}"))
        .collect();
    let refs: Vec<&str> = std::iter::once(subject)
        .chain(many.iter().map(String::as_str))
        .collect();
    let big = embedder
        .embed_batch(&refs, EmbedRole::Document)
        .await
        .expect("oversized batch");
    assert_eq!(
        big.len(),
        refs.len(),
        "batch results must be positionally aligned with input"
    );

    let big_f: &[f32] = big[0].as_ref();
    let worst_big = solo_f
        .iter()
        .zip(big_f)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0_f32, f32::max);
    assert!(
        worst_big < 1e-5,
        "embedding the same text in a 41-item batch (which chunks across the \
		 frozen MAX_BATCH of 32) differs from the solo vector by {worst_big}. \
		 Same conclusion as above, and this variant additionally implicates the \
		 chunk boundary itself."
    );
}

#[tokio::test]
#[ignore = "needs real model weights: set NUDOX_EMBED_MODEL_DIR and run with --ignored"]
async fn live_tokenizer_counts_and_truncates() {
    let spec = WeightsSpec::jina_code_v2(model_dir());
    let counter =
        HfTokenCounter::from_file(spec.sidecar("tokenizer.json")).expect("load tokenizer");

    let text = "pub fn very_long_function_name(argument: usize) -> impl Future<Output = ()>";
    let total = counter.count(text);
    assert!(total > 0);

    let truncated = counter.truncate_to(text, total / 2);
    assert!(
        counter.count(&truncated) <= total / 2,
        "cut lands on a token boundary"
    );

    // Under budget → verbatim passthrough.
    assert_eq!(counter.truncate_to(text, total + 10), text);
}
