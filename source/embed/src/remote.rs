//! Remote HTTP embedder — OpenAI-compatible `/v1/embeddings` API.
//!
//! Any server that speaks the OpenAI embeddings protocol works here:
//! OpenAI itself, Ollama (`/api/embeddings` is NOT compatible — use the
//! OpenAI-compat endpoint `/v1/embeddings`), vLLM, LiteLLM, etc.
//!
//! # Usage
//!
//! ```no_run
//! use embed::RemoteEmbedder;
//! use url::Url;
//!
//! let embedder = RemoteEmbedder::builder(
//!     Url::parse("https://api.openai.com/v1/embeddings").unwrap(),
//!     "text-embedding-3-small",
//! )
//! .api_key("sk-…")
//! .build();
//! ```

use async_trait::async_trait;
use nudox_core::{Embedder, EmbedderError, Embedding, EmbeddingPurpose, ModelId, ModelType, Result, SourceChunk};
use url::Url;

/// Embedder that calls an OpenAI-compatible `/v1/embeddings` endpoint.
pub struct RemoteEmbedder {
	client:     reqwest::Client,
	endpoint:   Url,
	model_id:   ModelId,
	api_key:    Option<String>,
	model_type: ModelType,
}

/// Builder for [`RemoteEmbedder`].
pub struct RemoteEmbedderBuilder {
	endpoint:   Url,
	model_id:   ModelId,
	api_key:    Option<String>,
	model_type: ModelType,
}

impl RemoteEmbedderBuilder {
	fn new(endpoint: Url, model_id: impl Into<String>) -> Self {
		Self { endpoint, model_id: ModelId::new(model_id), api_key: None, model_type: ModelType::SelfHosted }
	}

	/// Set a Bearer API key sent in the `Authorization` header.
	pub fn api_key(mut self, key: impl Into<String>) -> Self {
		self.api_key = Some(key.into());
		self
	}

	/// Override the [`ModelType`] tag (default: `SelfHosted`).
	///
	/// Use `ModelType::Openai` when pointing at the official OpenAI API so
	/// downstream consumers can distinguish vector origins.
	pub fn model_type(mut self, mt: ModelType) -> Self {
		self.model_type = mt;
		self
	}

	/// Construct the embedder.
	pub fn build(self) -> RemoteEmbedder {
		RemoteEmbedder {
			client:     reqwest::Client::new(),
			endpoint:   self.endpoint,
			model_id:   self.model_id,
			api_key:    self.api_key,
			model_type: self.model_type,
		}
	}
}

impl RemoteEmbedder {
	/// Start building a [`RemoteEmbedder`].
	pub fn builder(endpoint: Url, model_id: impl Into<String>) -> RemoteEmbedderBuilder {
		RemoteEmbedderBuilder::new(endpoint, model_id)
	}
}

fn input_text<'a>(chunk: &'a SourceChunk, _purpose: &EmbeddingPurpose) -> &'a str {
	// Use raw_code for all purposes for now.
	// A future implementation could prepend a task prefix per purpose
	// (e.g. "represent this code: ") when the model supports it.
	&chunk.raw_code
}

#[async_trait]
impl Embedder for RemoteEmbedder {
	fn model_id(&self) -> &ModelId { &self.model_id }

	fn model_type(&self) -> ModelType { self.model_type.clone() }

	#[tracing::instrument(skip(self, chunk), fields(model = %self.model_id, purpose = ?purpose))]
	async fn embed(&self, chunk: &SourceChunk, purpose: EmbeddingPurpose) -> Result<Embedding> {
		let input = input_text(chunk, &purpose);

		let body = serde_json::json!({
				"model": self.model_id.as_str(),
				"input": input,
		});

		let mut req = self.client.post(self.endpoint.as_str()).json(&body);
		if let Some(key) = &self.api_key {
			req = req.bearer_auth(key);
		}

		let resp = req.send().await.map_err(|e| EmbedderError::Send(Box::new(e)))?;

		if !resp.status().is_success() {
			let status = resp.status().as_u16();
			let body = resp.text().await.unwrap_or_default();
			return Err(EmbedderError::Http { status, body }.into());
		}

		let json: serde_json::Value =
			resp.json().await.map_err(|e| EmbedderError::Decode(Box::new(e)))?;

		let raw = json["data"][0]["embedding"]
			.as_array()
			.ok_or(EmbedderError::InvalidResponse { field: "data[0].embedding" })?;

		let mut floats = Vec::with_capacity(raw.len());
		for (i, v) in raw.iter().enumerate() {
			let f = v.as_f64().ok_or_else(|| EmbedderError::InvalidResponse {
				field: "data[0].embedding[i] is not a number",
			})? as f32;
			let _ = i;
			floats.push(f);
		}

		Embedding::new(floats).ok_or_else(|| EmbedderError::InvalidResponse {
			field: "data[0].embedding is empty",
		}.into())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn builder_sets_fields() {
		let e = RemoteEmbedder::builder(
			Url::parse("http://localhost:11434/v1/embeddings").unwrap(),
			"nomic-embed-text",
		)
		.api_key("test-key")
		.model_type(ModelType::SelfHosted)
		.build();

		assert_eq!(e.model_id().as_str(), "nomic-embed-text");
		assert_eq!(e.model_type(), ModelType::SelfHosted);
	}
}
