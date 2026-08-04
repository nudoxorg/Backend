#![cfg(feature = "onnx")]
//! Live-model tests (feature `onnx`). These need real weights on disk and are
//! `#[ignore]`-gated: opt in with
//!
//! ```sh
//! NUDOX_EMBED_MODEL_DIR=/path/to/jina-code-v2 \
//!     cargo test -p vector-embed --test onnx_live -- --ignored
//! ```
//!
//! The directory must contain `model_quantized.onnx` plus the tokenizer
//! sidecars (`tokenizer.json`, `config.json`, `special_tokens_map.json`,
//! `tokenizer_config.json`) from the pinned
//! `jinaai/jina-embeddings-v2-base-code` artifact.

#![cfg(feature = "onnx")]

use vector::recipe::TokenCounter;
use vector::{EmbedRole, Embedder, JinaCodeV2};
use vector::embed::runtime::{FastembedOrt, RuntimeConfig};
use vector::embed::tokens::HfTokenCounter;
use vector::embed::weights::WeightsSpec;

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
	let embedder =
		FastembedOrt::load(spec, RuntimeConfig::default()).await.expect("load pinned model");

	let info = embedder.runtime_info();
	assert!(info.durable_canonical, "CPU EP is the durable-canonical plane");
	assert!(info.weights_sha256.is_some(), "verified sha reported");

	let embedding = embedder
		.embed("fn parse(input: &str) -> Result<Ast, Error>", EmbedRole::Document)
		.await
		.expect("embed");
	let floats: &[f32] = embedding.as_ref();
	assert_eq!(floats.len(), JinaCodeV2::DIMENSIONS);

	let norm: f32 = floats.iter().map(|x| x * x).sum::<f32>().sqrt();
	assert!((norm - 1.0).abs() < 1e-3, "L2-normalized output, got norm {norm}");
}

#[tokio::test]
#[ignore = "needs real model weights: set NUDOX_EMBED_MODEL_DIR and run with --ignored"]
async fn live_model_role_is_symmetric() {
	// Jina v2 code has no query/doc asymmetry — both roles must produce the
	// identical vector (09b §3.1b).
	let spec = WeightsSpec::jina_code_v2(model_dir());
	let embedder = FastembedOrt::load(spec, RuntimeConfig::default()).await.expect("load");

	let text = "impl Iterator for Lexer";
	let doc = embedder.embed(text, EmbedRole::Document).await.expect("doc embed");
	let query = embedder.embed(text, EmbedRole::Query).await.expect("query embed");
	let (doc_floats, query_floats): (&[f32], &[f32]) = (doc.as_ref(), query.as_ref());
	assert_eq!(doc_floats, query_floats);
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
	assert!(counter.count(&truncated) <= total / 2, "cut lands on a token boundary");

	// Under budget → verbatim passthrough.
	assert_eq!(counter.truncate_to(text, total + 10), text);
}
