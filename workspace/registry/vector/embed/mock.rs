//! Deterministic offline embedder for tests — no model weights, no network.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::vector::core::{
	AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel,
	JinaCodeV2, ModelId,
};

use super::MAX_BATCH;

/// A deterministic [`Embedder`]: hashes the text (blake3) into a seed and
/// expands it into a unit vector of the correct dimensionality. Same text →
/// same vector, different texts → (almost surely) different vectors, so
/// CAS/trace logic is exercised realistically.
///
/// Records every batch it sees, so tests can assert inference counts,
/// batch coalescing, and priority ordering.
#[derive(Default)]
pub struct MockEmbedder {
	calls: AtomicUsize,
	texts: AtomicUsize,
	batches: Mutex<Vec<Vec<String>>>,
}

impl MockEmbedder {
	pub fn new() -> Self { Self::default() }

	/// Number of `embed`/`embed_batch` invocations (each = one "model run").
	pub fn call_count(&self) -> usize { self.calls.load(Ordering::SeqCst) }

	/// Total texts embedded across all calls.
	pub fn text_count(&self) -> usize { self.texts.load(Ordering::SeqCst) }

	/// Every batch, in arrival order.
	pub fn batches(&self) -> Vec<Vec<String>> {
		self.batches.lock().expect("batches poisoned").clone()
	}

	/// All embedded texts, flattened in arrival order.
	pub fn seen(&self) -> Vec<String> {
		self.batches().into_iter().flatten().collect()
	}

	fn record(&self, texts: &[&str]) {
		self.calls.fetch_add(1, Ordering::SeqCst);
		self.texts.fetch_add(texts.len(), Ordering::SeqCst);
		self.batches
			.lock()
			.expect("batches poisoned")
			.push(texts.iter().map(|t| (*t).to_owned()).collect());
	}
}

/// The deterministic vector for `text`: blake3-seeded xorshift, mapped into
/// `[-1, 1]`, then L2-normalized. Public so tests can predict store contents.
pub fn deterministic_unit_vector(text: &str) -> Vec<f32> {
	let seed = blake3::hash(text.as_bytes());
	let mut state = u64::from_le_bytes(seed.as_bytes()[..8].try_into().expect("8 bytes"));

	let mut vector: Vec<f32> = (0..JinaCodeV2::DIMENSIONS)
		.map(|_| {
			// xorshift64 — cheap, deterministic, platform-independent.
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
			(state as f64 / u64::MAX as f64) as f32 * 2.0 - 1.0
		})
		.collect();

	let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
	debug_assert!(norm > 0.0);
	for x in &mut vector {
		*x /= norm;
	}
	vector
}

#[async_trait::async_trait]
impl Embedder for MockEmbedder {
	type Model = JinaCodeV2;

	async fn embed(
		&self,
		text: &str,
		role: EmbedRole,
	) -> Result<Embedding<JinaCodeV2>, EmbedError> {
		let mut batch = self.embed_batch(&[text], role).await?;
		Ok(batch.pop().expect("one vector for one text"))
	}

	async fn embed_batch(
		&self,
		texts: &[&str],
		_role: EmbedRole,
	) -> Result<Vec<Embedding<JinaCodeV2>>, EmbedError> {
		self.record(texts);
		texts
			.iter()
			.map(|text| Embedding::from_vec(deterministic_unit_vector(text)))
			.collect()
	}

	fn runtime(&self) -> EmbedRuntimeInfo {
		EmbedRuntimeInfo {
			model_id: ModelId::new("mock/deterministic-jina-shape"),
			accel: AccelKind::Cpu,
			// The mock is deterministic but NOT the canonical model: nothing
			// it produces may be published as a parity/CAS vector.
			durable_canonical: false,
			max_batch: MAX_BATCH,
			max_seq_len: super::MAX_SEQ_LEN,
			weights_sha256: None,
			ort_package_id: "mock".into(),
		}
	}
}
