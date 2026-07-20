//! The concrete query embedder: an OpenAI-compatible `/v1/embeddings` HTTP
//! client, branded with the compiled-in model `M` so its vectors can only feed
//! a store of the same model.

#[allow(unused_imports)]
use crate::{registry};
use std::marker::PhantomData;
use std::time::Duration;

use registry::vector::{
	AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel, ModelId,
};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

/// How long one embedding round-trip may take before it is a timeout.
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(30);

/// How many texts this client will accept in one `embed_batch` round-trip. The
/// OpenAI-compatible wire has no hard ceiling; this is a self-imposed batching
/// hint surfaced through [`EmbedRuntimeInfo`].
const MAX_BATCH: usize = 512;

/// A conservative input-window hint for the runtime self-description. The HTTP
/// backend enforces its own real limit server-side; this is only advisory.
const MAX_SEQ_LEN: usize = 8192;

/// An embedder speaking the ubiquitous OpenAI embeddings wire shape
/// (`{"model": ..., "input": [...]}` → `{"data": [{"embedding": [...]}, ...]}`),
/// which local servers (ollama, vllm, TEI) and the hosted APIs all serve.
pub struct HttpEmbedder<M: EmbeddingModel> {
	client: reqwest::Client,
	endpoint: Url,
	api_key: Option<SecretString>,
	model: ModelId,
	_brand: PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> HttpEmbedder<M> {
	/// Configure an embedder against an endpoint (and optional bearer token).
	pub fn new(endpoint: Url, api_key: Option<SecretString>) -> Self {
		Self {
			client: reqwest::Client::new(),
			endpoint,
			api_key,
			model: M::id(),
			_brand: PhantomData,
		}
	}

	/// One wire round-trip for a batch of inputs, positionally aligned.
	///
	/// The new vector plane's [`EmbedError`] carries a single `Backend(String)`
	/// escape hatch rather than a rich rejection taxonomy, so transport, HTTP,
	/// and decode failures are all surfaced through it with a message that names
	/// what went wrong (the detail is preserved in the string, never lost).
	async fn request(&self, texts: &[&str]) -> Result<Vec<Embedding<M>>, EmbedError> {
		let payload = serde_json::json!({
			"model": self.model.as_str(),
			"input": texts,
		});
		let body = serde_json::to_vec(&payload)
			.map_err(|error| EmbedError::Backend(format!("request serialization failed: {error}")))?;

		let mut request = self
			.client
			.post(self.endpoint.clone())
			.header("content-type", "application/json")
			.timeout(EMBEDDING_TIMEOUT)
			.body(body);
		if let Some(api_key) = &self.api_key {
			request = request.bearer_auth(api_key.expose_secret());
		}

		let response = request
			.send()
			.await
			.map_err(|error| EmbedError::Backend(format!("embedding transport error: {error}")))?;
		let status = response.status();
		if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
			let retry_after = response
				.headers()
				.get("retry-after")
				.and_then(|value| value.to_str().ok())
				.unwrap_or("unspecified")
				.to_owned();
			return Err(EmbedError::Backend(format!(
				"embedding backend rate-limited (retry-after: {retry_after})"
			)));
		}
		if !status.is_success() {
			let message = response.text().await.unwrap_or_default();
			let body: String = message.chars().take(512).collect();
			return Err(EmbedError::Backend(format!(
				"embedding provider returned {status}: {body}"
			)));
		}

		let bytes = response
			.bytes()
			.await
			.map_err(|error| EmbedError::Backend(format!("embedding transport error: {error}")))?;
		let decoded: WireResponse = serde_json::from_slice(&bytes)
			.map_err(|error| EmbedError::Backend(format!("malformed embedding response: {error}")))?;
		if decoded.data.len() != texts.len() {
			return Err(EmbedError::Backend(format!(
				"embedding batch size mismatch: expected {}, received {}",
				texts.len(),
				decoded.data.len()
			)));
		}
		// `Embedding::from_vec` validates exact dimension + finiteness against the
		// brand `M`, so a wrong-length or NaN row is rejected at this boundary.
		decoded
			.data
			.into_iter()
			.map(|row| Embedding::from_vec(row.embedding))
			.collect()
	}
}

#[async_trait::async_trait]
impl<M: EmbeddingModel> Embedder for HttpEmbedder<M> {
	type Model = M;

	async fn embed(&self, text: &str, role: EmbedRole) -> Result<Embedding<M>, EmbedError> {
		let mut vectors = self.embed_batch(&[text], role).await?;
		vectors
			.pop()
			.ok_or_else(|| EmbedError::Backend("empty embedding response batch".to_owned()))
	}

	async fn embed_batch(
		&self,
		texts: &[&str],
		role: EmbedRole,
	) -> Result<Vec<Embedding<M>>, EmbedError> {
		// OpenAI-compatible endpoints do not distinguish query vs document role;
		// `role` is accepted for interface uniformity but not sent on the wire.
		// It is traced here so callers can audit what role was intended.
		tracing::trace!(count = texts.len(), ?role, model = %self.model, "embedding batch");
		if texts.is_empty() {
			return Ok(Vec::new());
		}
		self.request(texts).await
	}

	/// This is a *network* embedder: its output is not the bit-canonical int8
	/// ONNX artifact, so it is query-side only (never durable-canonical, I12).
	fn runtime(&self) -> EmbedRuntimeInfo {
		EmbedRuntimeInfo {
			model_id: self.model.clone(),
			accel: AccelKind::Other,
			durable_canonical: false,
			max_batch: MAX_BATCH,
			max_seq_len: MAX_SEQ_LEN,
			weights_sha256: None,
			ort_package_id: "http-openai-embeddings".into(),
		}
	}
}

/// The subset of the OpenAI embeddings response the server reads.
#[derive(Deserialize)]
struct WireResponse {
	data: Vec<WireEmbedding>,
}

#[derive(Deserialize)]
struct WireEmbedding {
	embedding: Vec<f32>,
}
