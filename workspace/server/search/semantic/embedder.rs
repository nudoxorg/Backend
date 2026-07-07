//! The concrete query embedder: an OpenAI-compatible `/v1/embeddings` HTTP
//! client, branded with the compiled-in model `M` so its vectors can only feed
//! a store of the same model.

use std::marker::PhantomData;
use std::time::Duration;

use runtime::error::EmbedError;
use runtime::vector::{Embedder, Embedding, EmbeddingModel, EmbeddingPurpose, ModelId};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use url::Url;

/// How long one embedding round-trip may take before it is a timeout.
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(30);

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
	async fn request(&self, texts: &[&str]) -> Result<Vec<Embedding<M>>, EmbedError> {
		let payload = serde_json::json!({
			"model": self.model.as_ref(),
			"input": texts,
		});
		let body = serde_json::to_vec(&payload)
			.map_err(|error| EmbedError::Rejected { message: error.to_string() })?;

		let mut request = self
			.client
			.post(self.endpoint.clone())
			.header("content-type", "application/json")
			.timeout(EMBEDDING_TIMEOUT)
			.body(body);
		if let Some(api_key) = &self.api_key {
			request = request.bearer_auth(api_key.expose_secret());
		}

		let response = request.send().await.map_err(EmbedError::Transport)?;
		let status = response.status();
		if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
			let retry_after = response
				.headers()
				.get("retry-after")
				.and_then(|value| value.to_str().ok())
				.and_then(|seconds| seconds.parse::<u64>().ok())
				.map(Duration::from_secs);
			return Err(EmbedError::RateLimited { retry_after });
		}
		if !status.is_success() {
			let message = response.text().await.unwrap_or_default();
			return Err(EmbedError::Rejected {
				message: format!("{status}: {}", message.chars().take(512).collect::<String>()),
			});
		}

		let bytes = response.bytes().await.map_err(EmbedError::Transport)?;
		let decoded: WireResponse = serde_json::from_slice(&bytes)
			.map_err(|error| EmbedError::Rejected { message: format!("malformed response: {error}") })?;
		if decoded.data.len() != texts.len() {
			return Err(EmbedError::Rejected {
				message: format!("expected {} vectors, got {}", texts.len(), decoded.data.len()),
			});
		}
		decoded
			.data
			.into_iter()
			.map(|row| Embedding::from_vec(row.embedding))
			.collect()
	}
}

impl<M: EmbeddingModel> Embedder for HttpEmbedder<M> {
	type Model = M;

	fn model(&self) -> &ModelId { &self.model }

	async fn embed(
		&self,
		text: &str,
		purpose: EmbeddingPurpose,
	) -> Result<Embedding<M>, EmbedError> {
		let mut vectors = self.embed_batch(&[text], purpose).await?;
		vectors.pop().ok_or(EmbedError::Rejected { message: "empty embedding batch".into() })
	}

	async fn embed_batch(
		&self,
		texts: &[&str],
		purpose: EmbeddingPurpose,
	) -> Result<Vec<Embedding<M>>, EmbedError> {
		// The wire shape carries no purpose; it is honoured upstream by the
		// cache key and payload stamping, and traced here for audit.
		tracing::trace!(count = texts.len(), ?purpose, model = %self.model, "embedding batch");
		if texts.is_empty() {
			return Ok(Vec::new());
		}
		self.request(texts).await
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
