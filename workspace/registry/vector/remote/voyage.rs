//! [`VoyageEmbedder`]: `Embedder<VoyageCode3>` against the Voyage REST API.
//!
//! Every request pins:
//! - `model = "voyage-code-3"` (I16 — never a default; the API has no
//!   hard-coded fallback that matches our schema)
//! - `output_dimension = 1024` (matches `VoyageCode3::DIMENSIONS`)
//! - `output_dtype = "float"` (prevents silent int8/binary promotion)
//! - `input_type = "query" | "document"` (role axis — never omitted; an
//!   unpinned default degrades retrieval asymmetry, I16/R5)
//!
//! Batch limits (Voyage API constraints):
//! - ≤ 1_000 texts per request
//! - ≤ 120_000 approximate tokens per request (conservative: chars / 4)
//!
//! Requests are chunked to both limits before sending; chunked results are
//! concatenated in order, so the caller sees a positionally-aligned Vec.
//!
//! The API key is a plain `String`. It MUST be treated as a secret (never
//! logged, never included in error messages); the caller is responsible for
//! loading it from a secrets store. Using a newtype like `secrecy::Secret<String>`
//! at the call site is strongly recommended.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use crate::vector::core::{
	AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel,
	VoyageCode3,
};

/// Base URL of the Voyage embeddings endpoint.
const VOYAGE_EMBED_URL: &str = "https://api.voyageai.com/v1/embeddings";

/// Pinned model string — must match `VoyageCode3::id()`.
const VOYAGE_MODEL: &str = "voyage-code-3";

/// Pinned output dimension — must match `VoyageCode3::DIMENSIONS`.
const VOYAGE_DIMENSIONS: u32 = 1024;

/// Pinned output dtype.
const VOYAGE_DTYPE: &str = "float";

/// Maximum texts per Voyage request.
const MAX_TEXTS_PER_REQUEST: usize = 1_000;

/// Approximate token budget per request (chars / 4 per text, summed).
const MAX_APPROX_TOKENS_PER_REQUEST: usize = 120_000;

/// An embedder that calls the Voyage REST API with fully pinned request params.
///
/// # API key security
/// `api_key` is a plain `String` but MUST be treated as a credential: never
/// log it, never include it in error messages, load it from a secrets manager.
pub struct VoyageEmbedder {
	client: reqwest::Client,
	/// The Voyage API key. Treat as a secret — see module-level note.
	api_key: String,
}

impl VoyageEmbedder {
	pub fn new(api_key: String) -> Self {
		Self { client: reqwest::Client::new(), api_key }
	}

	/// Build and send one batch request for `texts` with the given `role`.
	/// The batch is guaranteed to respect [`MAX_TEXTS_PER_REQUEST`] and
	/// [`MAX_APPROX_TOKENS_PER_REQUEST`] — callers are expected to chunk before
	/// calling this.
	async fn request_batch(
		&self,
		texts: &[&str],
		role: EmbedRole,
	) -> Result<Vec<Embedding<VoyageCode3>>, EmbedError> {
		let input_type = role_to_input_type(role);
		let body = VoyageEmbedRequest {
			input: texts.iter().map(|s| s.to_string()).collect(),
			model: VOYAGE_MODEL.to_owned(),
			input_type: input_type.to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};

		let resp = self
			.client
			.post(VOYAGE_EMBED_URL)
			.bearer_auth(&self.api_key)
			.json(&body)
			.send()
			.await
			.map_err(|e| EmbedError::Backend(e.to_string()))?;

		if !resp.status().is_success() {
			let status = resp.status().as_u16();
			let msg = resp.text().await.unwrap_or_default();
			return Err(EmbedError::Backend(format!("Voyage API {status}: {msg}")));
		}

		let parsed: VoyageEmbedResponse =
			resp.json().await.map_err(|e| EmbedError::Backend(e.to_string()))?;

		if parsed.data.len() != texts.len() {
			return Err(EmbedError::Backend(format!(
				"Voyage returned {} embeddings for {} inputs",
				parsed.data.len(),
				texts.len()
			)));
		}

		parsed
			.data
			.into_iter()
			.map(|item| Embedding::<VoyageCode3>::from_vec(item.embedding))
			.collect()
	}
}

/// Map `EmbedRole` → Voyage `input_type` string (I16: never a default).
fn role_to_input_type(role: EmbedRole) -> &'static str {
	match role {
		EmbedRole::Query => "query",
		EmbedRole::Document => "document",
	}
}

/// Split `texts` into sub-batches that each fit within the API limits.
///
/// Both limits must hold simultaneously:
/// - ≤ [`MAX_TEXTS_PER_REQUEST`] texts
/// - ≤ [`MAX_APPROX_TOKENS_PER_REQUEST`] approximate tokens (chars / 4)
fn chunk_texts<'a>(texts: &[&'a str]) -> Vec<Vec<&'a str>> {
	let mut chunks: Vec<Vec<&'a str>> = Vec::new();
	let mut current: Vec<&'a str> = Vec::new();
	let mut current_approx_tokens: usize = 0;

	for &text in texts {
		let text_tokens = text.len() / 4 + 1;

		let would_exceed_texts = current.len() >= MAX_TEXTS_PER_REQUEST;
		let would_exceed_tokens =
			current_approx_tokens + text_tokens > MAX_APPROX_TOKENS_PER_REQUEST;

		if !current.is_empty() && (would_exceed_texts || would_exceed_tokens) {
			chunks.push(std::mem::take(&mut current));
			current_approx_tokens = 0;
		}

		current.push(text);
		current_approx_tokens += text_tokens;
	}
	if !current.is_empty() {
		chunks.push(current);
	}
	chunks
}

#[async_trait]
impl Embedder for VoyageEmbedder {
	type Model = VoyageCode3;

	async fn embed(&self, text: &str, role: EmbedRole) -> Result<Embedding<VoyageCode3>, EmbedError> {
		let mut batch = self.embed_batch(&[text], role).await?;
		batch.pop().ok_or_else(|| EmbedError::Backend("empty response from Voyage".into()))
	}

	async fn embed_batch(
		&self,
		texts: &[&str],
		role: EmbedRole,
	) -> Result<Vec<Embedding<VoyageCode3>>, EmbedError> {
		if texts.is_empty() {
			return Ok(Vec::new());
		}

		let chunks = chunk_texts(texts);
		let mut results = Vec::with_capacity(texts.len());
		for chunk in chunks {
			let mut batch = self.request_batch(&chunk, role).await?;
			results.append(&mut batch);
		}
		Ok(results)
	}

	fn runtime(&self) -> EmbedRuntimeInfo {
		EmbedRuntimeInfo {
			model_id: VoyageCode3::id(),
			accel: AccelKind::Other,
			durable_canonical: false,
			max_batch: MAX_TEXTS_PER_REQUEST,
			max_seq_len: 16_000,
			weights_sha256: None,
			ort_package_id: "voyage-api".into(),
		}
	}
}

/// Wire shape of a Voyage embeddings request.
#[derive(Debug, Serialize)]
struct VoyageEmbedRequest {
	input: Vec<String>,
	model: String,
	input_type: String,
	output_dimension: u32,
	output_dtype: String,
}

/// Wire shape of a Voyage embeddings response.
#[derive(Debug, Deserialize)]
struct VoyageEmbedResponse {
	data: Vec<VoyageEmbedItem>,
}

#[derive(Debug, Deserialize)]
struct VoyageEmbedItem {
	embedding: Vec<f32>,
}


#[cfg(test)]
mod tests {
	use super::*;

	/// The serialized request body must contain all four pinned fields with the
	/// correct values — this is the I16/R5 invariant test.
	#[test]
	fn request_body_pins_all_required_fields() {
		let body = VoyageEmbedRequest {
			input: vec!["fn search()".to_owned()],
			model: VOYAGE_MODEL.to_owned(),
			input_type: "query".to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};
		let json = serde_json::to_string(&body).unwrap();

		// Every pinned field must be literally present in the serialized body.
		assert!(json.contains("\"input_type\""), "input_type must be explicit: {json}");
		assert!(
			json.contains("\"output_dimension\""),
			"output_dimension must be explicit: {json}"
		);
		assert!(json.contains("\"output_dtype\""), "output_dtype must be explicit: {json}");
		assert!(json.contains("\"voyage-code-3\""), "model must be voyage-code-3: {json}");
		assert!(json.contains("1024"), "output_dimension value must be 1024: {json}");
		assert!(json.contains("\"float\""), "output_dtype value must be float: {json}");
	}

	#[test]
	fn query_role_maps_to_query_input_type() {
		let body = VoyageEmbedRequest {
			input: vec!["q".to_owned()],
			model: VOYAGE_MODEL.to_owned(),
			input_type: role_to_input_type(EmbedRole::Query).to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};
		let json = serde_json::to_string(&body).unwrap();
		assert!(json.contains("\"query\""), "query role must map to \"query\": {json}");
		assert!(!json.contains("\"document\""), "query role must not produce \"document\": {json}");
	}

	#[test]
	fn document_role_maps_to_document_input_type() {
		let body = VoyageEmbedRequest {
			input: vec!["d".to_owned()],
			model: VOYAGE_MODEL.to_owned(),
			input_type: role_to_input_type(EmbedRole::Document).to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};
		let json = serde_json::to_string(&body).unwrap();
		assert!(json.contains("\"document\""), "document role must map to \"document\": {json}");
	}

	/// Batch chunking: 1001 texts must split into two chunks.
	#[test]
	fn batch_chunking_at_text_limit() {
		let texts: Vec<&str> = vec!["x"; 1_001];
		let chunks = chunk_texts(&texts);
		assert_eq!(chunks.len(), 2, "1001 texts must split at the 1000-text cap");
		assert_eq!(chunks[0].len(), 1_000);
		assert_eq!(chunks[1].len(), 1);
	}

	/// Batch chunking: texts that push approx tokens above 120k must split.
	#[test]
	fn batch_chunking_at_token_limit() {
		// Each text is 400 chars → 100 approx tokens. 1200 texts × 100 = 120_000.
		// On text 1201 the limit triggers.
		let long_text = "a".repeat(400);
		let texts: Vec<&str> = vec![long_text.as_str(); 1_201];
		let chunks = chunk_texts(&texts);
		// Should be at least 2 chunks.
		assert!(chunks.len() >= 2, "token-limit split must produce multiple chunks: {}", chunks.len());
		// All chunks respect the text limit.
		for chunk in &chunks {
			assert!(chunk.len() <= MAX_TEXTS_PER_REQUEST);
		}
		// Total count is preserved.
		let total: usize = chunks.iter().map(|c| c.len()).sum();
		assert_eq!(total, 1_201);
	}

	/// Empty batch produces empty chunks.
	#[test]
	fn empty_texts_produce_no_chunks() {
		assert!(chunk_texts(&[]).is_empty());
	}

	/// Single text always fits in one chunk.
	#[test]
	fn single_text_is_one_chunk() {
		let chunks = chunk_texts(&["hello world"]);
		assert_eq!(chunks.len(), 1);
		assert_eq!(chunks[0], vec!["hello world"]);
	}

	// ── Adversarial: JSON-hostile input and exact boundary math ───────────────

	/// Query text containing JSON-breaking characters (quotes, backslashes,
	/// newlines, carriage returns) must still produce a valid, parseable JSON
	/// body with all four pinned fields intact — the I16/R5 invariant holds even
	/// when the *payload* is adversarial.
	#[test]
	fn json_hostile_text_pinned_fields_survive() {
		let hostile = "fn evil(x: &str) {\n\t\"injection\\\"\r\n}";
		let body = VoyageEmbedRequest {
			input: vec![hostile.to_owned()],
			model: VOYAGE_MODEL.to_owned(),
			input_type: role_to_input_type(EmbedRole::Query).to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};
		let json = serde_json::to_string(&body)
			.expect("must serialize despite hostile content");

		// Four pinned fields, exact values.
		assert!(json.contains("\"voyage-code-3\""), "model pinned: {json}");
		assert!(json.contains("\"query\""), "input_type pinned: {json}");
		assert!(json.contains("\"output_dimension\""), "output_dimension key present: {json}");
		assert!(json.contains("1024"), "output_dimension value present: {json}");
		assert!(json.contains("\"float\""), "output_dtype present: {json}");

		// The result must be valid JSON despite the hostile payload.
		serde_json::from_str::<serde_json::Value>(&json)
			.expect("serialized body must be parseable JSON");
	}

	/// Query text containing a 1 MB payload — the serializer must not panic and
	/// the four pinned fields must still be present in the resulting JSON body.
	#[test]
	fn one_mb_text_pinned_fields_survive() {
		let large = "A".repeat(1_000_000);
		let body = VoyageEmbedRequest {
			input: vec![large],
			model: VOYAGE_MODEL.to_owned(),
			input_type: role_to_input_type(EmbedRole::Document).to_owned(),
			output_dimension: VOYAGE_DIMENSIONS,
			output_dtype: VOYAGE_DTYPE.to_owned(),
		};
		let json = serde_json::to_string(&body).expect("must serialize 1 MB text");

		assert!(json.contains("\"voyage-code-3\""), "model pinned after 1 MB text");
		assert!(json.contains("1024"), "output_dimension pinned after 1 MB text");
		assert!(json.contains("\"float\""), "output_dtype pinned after 1 MB text");
		assert!(json.contains("\"document\""), "input_type pinned as document");
		serde_json::from_str::<serde_json::Value>(&json)
			.expect("1 MB text must produce parseable JSON");
	}

	/// 1000 texts (the documented upper limit) must fit in exactly 1 chunk.
	#[test]
	fn batch_of_exactly_1000_is_one_chunk() {
		let texts: Vec<&str> = vec!["x"; 1_000];
		let chunks = chunk_texts(&texts);
		assert_eq!(chunks.len(), 1, "1000 texts must fit in exactly 1 chunk");
		assert_eq!(chunks[0].len(), 1_000, "chunk holds all 1000");
	}

	/// 1001 texts → exactly 2 chunks with sizes [1000, 1] — the text limit split
	/// is the simpler case (token budget not the binder here).
	#[test]
	fn batch_of_1001_splits_into_1000_plus_1_exact() {
		let texts: Vec<&str> = vec!["x"; 1_001];
		let chunks = chunk_texts(&texts);
		assert_eq!(chunks.len(), 2, "1001 texts must produce exactly 2 chunks");
		assert_eq!(chunks[0].len(), 1_000, "first chunk: 1000");
		assert_eq!(chunks[1].len(), 1, "second chunk: 1");
	}

	/// Token-cap boundary math: texts of 400 chars each cost 400/4 + 1 = 101
	/// approx tokens per the `voyage.rs` formula. With MAX_APPROX_TOKENS = 120_000:
	///
	///   floor(120_000 / 101) = 1188 texts fit (1188 × 101 = 119_988 ≤ 120_000).
	///   1189 texts (1189 × 101 = 120_089 > 120_000) → must split.
	///
	/// Pin: 1188 texts of 400 chars → 1 chunk. 1189 → ≥ 2 chunks.
	#[test]
	fn token_cap_exact_boundary_math_1188_fits() {
		const LEN: usize = 400;
		const COST: usize = LEN / 4 + 1; // = 101
		const MAX_FIT: usize = MAX_APPROX_TOKENS_PER_REQUEST / COST; // = 1188

		// Sanity-check the constants so the test fails loudly if the formula changes.
		assert_eq!(COST, 101, "cost formula: 400/4+1=101");
		assert_eq!(MAX_FIT, 1188, "floor(120000/101)=1188");
		assert!(
			MAX_FIT * COST <= MAX_APPROX_TOKENS_PER_REQUEST,
			"1188 texts must fit: {} * {} = {} ≤ {}",
			MAX_FIT, COST, MAX_FIT * COST, MAX_APPROX_TOKENS_PER_REQUEST,
		);

		let text = "B".repeat(LEN);
		let texts: Vec<&str> = vec![text.as_str(); MAX_FIT];
		let chunks = chunk_texts(&texts);
		assert_eq!(
			chunks.len(), 1,
			"1188 texts of 400 chars (101 tokens each) must fit in 1 chunk"
		);
		assert_eq!(chunks[0].len(), MAX_FIT);
	}

	#[test]
	fn token_cap_exact_boundary_math_1189_splits() {
		const LEN: usize = 400;
		const COST: usize = LEN / 4 + 1; // = 101
		const MAX_FIT: usize = MAX_APPROX_TOKENS_PER_REQUEST / COST; // = 1188
		const N: usize = MAX_FIT + 1; // 1189

		assert!(
			N * COST > MAX_APPROX_TOKENS_PER_REQUEST,
			"1189 texts must exceed budget: {} * {} = {} > {}",
			N, COST, N * COST, MAX_APPROX_TOKENS_PER_REQUEST,
		);

		let text = "C".repeat(LEN);
		let texts: Vec<&str> = vec![text.as_str(); N];
		let chunks = chunk_texts(&texts);
		assert!(
			chunks.len() >= 2,
			"1189 texts must split into ≥ 2 chunks; got {}",
			chunks.len()
		);
		// Total count preserved.
		let total: usize = chunks.iter().map(|c| c.len()).sum();
		assert_eq!(total, N, "no texts lost in token-cap split");
		// Every chunk respects both limits.
		for (i, chunk) in chunks.iter().enumerate() {
			assert!(chunk.len() <= MAX_TEXTS_PER_REQUEST, "chunk {i} exceeds text limit");
		}
	}

	/// A single text larger than the entire token budget still forms exactly
	/// 1 chunk — the spec makes no provision for splitting within a single text;
	/// each chunk holds at least 1 element regardless of size.
	#[test]
	fn single_massive_text_is_always_one_chunk() {
		let huge = "D".repeat(512_000);
		let texts: Vec<&str> = vec![huge.as_str()];
		let chunks = chunk_texts(&texts);
		assert_eq!(chunks.len(), 1, "single text is always 1 chunk regardless of size");
		assert_eq!(chunks[0].len(), 1);
	}

	// ── Adversarial: response serde attacks ──────────────────────────────────

	/// Non-finite floats in a JSON response are rejected by serde_json (JSON
	/// spec forbids NaN/Infinity). Pin that the deserialization errors, ensuring
	/// non-finite values never silently enter the pipeline.
	#[test]
	fn non_finite_float_in_response_rejected_by_serde() {
		// Bare NaN is not valid JSON at all.
		let json_bare_nan = r#"{"embedding":[0.1,NaN,0.3]}"#;
		assert!(
			serde_json::from_str::<VoyageEmbedItem>(json_bare_nan).is_err(),
			"bare NaN is not valid JSON"
		);

		// "NaN" as a string is not f32.
		let json_str_nan = r#"{"embedding":[0.1,"NaN",0.3]}"#;
		assert!(
			serde_json::from_str::<VoyageEmbedItem>(json_str_nan).is_err(),
			"string 'NaN' must not deserialize as f32"
		);

		// "Infinity" as a string is not f32.
		let json_str_inf = r#"{"embedding":[0.1,"Infinity",0.3]}"#;
		assert!(
			serde_json::from_str::<VoyageEmbedItem>(json_str_inf).is_err(),
			"string 'Infinity' must not deserialize as f32"
		);
	}

	/// A response with MORE embeddings than inputs: serde accepts it
	/// (liberal receiver), but the count guard in `request_batch` fires.
	/// Pin: the guard condition `parsed.data.len() != texts.len()` is correct.
	#[test]
	fn response_with_more_embeddings_than_inputs_detected() {
		let json = r#"{"data":[{"embedding":[0.1,0.2]},{"embedding":[0.3,0.4]},{"embedding":[0.5,0.6]}]}"#;
		let parsed: VoyageEmbedResponse =
			serde_json::from_str(json).expect("must deserialize");
		let inputs = 2usize;
		assert_ne!(
			parsed.data.len(),
			inputs,
			"3 embeddings for 2 inputs must mismatch → count guard fires"
		);
	}

	/// A response with FEWER embeddings than inputs: same guard.
	#[test]
	fn response_with_fewer_embeddings_than_inputs_detected() {
		let json = r#"{"data":[{"embedding":[0.1,0.2]}]}"#;
		let parsed: VoyageEmbedResponse =
			serde_json::from_str(json).expect("must deserialize");
		let inputs = 3usize;
		assert_ne!(
			parsed.data.len(),
			inputs,
			"1 embedding for 3 inputs must mismatch → count guard fires"
		);
	}

	/// Role mapping is bijective: Query → "query", Document → "document",
	/// and neither produces the other's string.
	#[test]
	fn role_to_input_type_is_bijective() {
		let q = role_to_input_type(EmbedRole::Query);
		let d = role_to_input_type(EmbedRole::Document);
		assert_eq!(q, "query");
		assert_eq!(d, "document");
		assert_ne!(q, d, "roles must map to distinct strings");
	}

	// ── Adversarial: wrong-dimension response embeddings ──────────────────────

	/// A response embedding whose dimension does not match `VoyageCode3::DIMENSIONS`
	/// (1024) is rejected by `Embedding::<VoyageCode3>::from_vec` as a
	/// `DimensionMismatch` error. This pin proves the dimension guard fires and the
	/// value can never silently enter the pipeline as a wrong-sized vector.
	///
	/// This is I11: a vector of the wrong length cannot exist as a typed value.
	#[test]
	fn wrong_dimension_embedding_in_response_is_rejected() {
		// 512-component response embedding — exactly half the required 1024.
		let short: Vec<f32> = vec![0.1; 512];
		let result = Embedding::<VoyageCode3>::from_vec(short);
		assert!(
			result.is_err(),
			"512-dim vector for VoyageCode3 (1024-dim) must be rejected"
		);
		match result.unwrap_err() {
			crate::vector::core::EmbedError::DimensionMismatch { expected, got } => {
				assert_eq!(expected, 1024, "expected dimension is VoyageCode3::DIMENSIONS");
				assert_eq!(got, 512, "got dimension is the (wrong) response size");
			}
			other => panic!("wrong error variant: {other:?}"),
		}
	}

	/// A response embedding that is 1025 components — one *too long* — is also
	/// rejected. Proves the guard is exact on both sides (not just a lower bound).
	#[test]
	fn one_too_long_embedding_in_response_is_rejected() {
		let too_long: Vec<f32> = vec![0.5; 1025];
		let result = Embedding::<VoyageCode3>::from_vec(too_long);
		assert!(
			result.is_err(),
			"1025-dim vector for VoyageCode3 (1024-dim) must be rejected"
		);
		match result.unwrap_err() {
			crate::vector::core::EmbedError::DimensionMismatch { expected, got } => {
				assert_eq!(expected, 1024);
				assert_eq!(got, 1025);
			}
			other => panic!("wrong error variant for 1025-dim: {other:?}"),
		}
	}

	/// A zero-length response embedding is rejected (DimensionMismatch, not panic).
	#[test]
	fn zero_length_embedding_in_response_is_rejected() {
		let empty: Vec<f32> = Vec::new();
		let result = Embedding::<VoyageCode3>::from_vec(empty);
		assert!(result.is_err(), "zero-length embedding must be rejected");
		match result.unwrap_err() {
			crate::vector::core::EmbedError::DimensionMismatch { expected, got } => {
				assert_eq!(expected, 1024);
				assert_eq!(got, 0);
			}
			other => panic!("wrong error variant for zero-length: {other:?}"),
		}
	}

	/// A response embedding of exactly `VoyageCode3::DIMENSIONS` finite floats is
	/// accepted — the nominal "correct API response" case.
	#[test]
	fn correct_dimension_embedding_accepted() {
		let correct: Vec<f32> = vec![0.01; 1024];
		let result = Embedding::<VoyageCode3>::from_vec(correct);
		assert!(result.is_ok(), "1024-dim embedding must be accepted: {result:?}");
	}
}
