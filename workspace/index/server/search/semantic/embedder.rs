//! The concrete query/document embedder: [`embedrs`] over an OpenAI-compatible
//! (or hosted) embeddings endpoint, branded with the compiled-in model `M` so
//! its vectors can only feed a store of the same model.
//!
//! Config still names a full embeddings URL (`…/v1/embeddings`) plus optional
//! bearer token; we strip the trailing `/embeddings` segment for
//! [`embedrs::Client::openai_compatible`], which posts to `{base}/embeddings`.

#[allow(unused_imports)]
use crate::server::registry;
use std::marker::PhantomData;
use std::time::Duration;

use embedrs::{BackoffConfig, Client as EmbedrsClient, InputType};
use registry::vector::{
    AccelKind, EmbedError, EmbedRole, EmbedRuntimeInfo, Embedder, Embedding, EmbeddingModel,
    ModelId,
};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

/// How long one embedding round-trip may take before it is a timeout.
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(30);

/// How many texts this client will accept in one `embed_batch` round-trip.
/// Surfaced through [`EmbedRuntimeInfo`]; embedrs chunks to the provider's own
/// max when the batch is larger.
const MAX_BATCH: usize = 512;

/// A conservative input-window hint for the runtime self-description. The HTTP
/// backend enforces its own real limit server-side; this is only advisory.
const MAX_SEQ_LEN: usize = 8192;

/// An embedder speaking the ubiquitous OpenAI embeddings wire shape via
/// [`embedrs`], which local servers (ollama, vllm, TEI) and hosted APIs all
/// serve. Branded with `M` so dimension + model-id mismatches fail at the
/// [`Embedding::from_vec`] boundary.
pub struct HttpEmbedder<M: EmbeddingModel> {
    client: EmbedrsClient,
    model: ModelId,
    _brand: PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> HttpEmbedder<M> {
    /// Configure an embedder against an endpoint (and optional bearer token).
    ///
    /// `endpoint` may be either the full embeddings URL (`…/v1/embeddings`) or
    /// the OpenAI-style base (`…/v1`); both resolve to the same embedrs base.
    pub fn new(endpoint: Url, api_key: Option<SecretString>) -> Self {
        let base_url = openai_compatible_base(&endpoint);
        // embedrs always installs a Bearer header; empty string is fine for
        // local servers that ignore Authorization (e.g. stock ollama).
        let key = api_key
            .as_ref()
            .map(|k| k.expose_secret().to_owned())
            .unwrap_or_default();

        let client = EmbedrsClient::openai_compatible(key, base_url)
            .with_model(M::id().as_str().to_owned())
            .with_timeout(EMBEDDING_TIMEOUT)
            // 429/503: exponential backoff (previous client failed immediately).
            .with_retry_backoff(BackoffConfig::default());

        Self {
            client,
            model: M::id(),
            _brand: PhantomData,
        }
    }

    /// One embedrs round-trip for a batch of inputs, positionally aligned.
    async fn request(
        &self,
        texts: &[&str],
        role: EmbedRole,
    ) -> Result<Vec<Embedding<M>>, EmbedError> {
        let owned: Vec<String> = texts.iter().map(|t| (*t).to_owned()).collect();
        let input_type = role_to_input_type(role);

        let result = self
            .client
            .embed(owned)
            .input_type(input_type)
            .await
            .map_err(map_embedrs_error)?;

        if result.embeddings.len() != texts.len() {
            return Err(EmbedError::Backend(format!(
                "embedding batch size mismatch: expected {}, received {}",
                texts.len(),
                result.embeddings.len()
            )));
        }

        // `Embedding::from_vec` validates exact dimension + finiteness against
        // the brand `M`, so a wrong-length or NaN row is rejected here.
        result
            .embeddings
            .into_iter()
            .map(Embedding::from_vec)
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
        tracing::trace!(count = texts.len(), ?role, model = %self.model, "embedding batch");
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        self.request(texts, role).await
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
            ort_package_id: "embedrs-openai-compatible".into(),
        }
    }
}

/// Map our retrieval-asymmetry role onto embedrs' input-type hint. Providers
/// that ignore the field (plain OpenAI-compatible) no-op; Voyage/Jina/Cohere
/// honoring it keeps query vs document encoding correct when the same client
/// is pointed at those APIs.
fn role_to_input_type(role: EmbedRole) -> InputType {
    match role {
        EmbedRole::Query => InputType::SearchQuery,
        EmbedRole::Document => InputType::SearchDocument,
    }
}

/// Convert a configured embeddings URL into the base embedrs expects
/// (`{base}/embeddings` is the POST path). Accepts both the full path and the
/// already-stripped base so operators can write either.
fn openai_compatible_base(endpoint: &Url) -> String {
    let s = endpoint.as_str().trim_end_matches('/');
    s.strip_suffix("/embeddings")
        .map(|base| base.trim_end_matches('/').to_owned())
        .unwrap_or_else(|| s.to_owned())
}

/// Fold embedrs errors into the plane's single `Backend(String)` escape hatch,
/// preserving the detail that matters for ops (status, timeout, transport).
fn map_embedrs_error(error: embedrs::Error) -> EmbedError {
    let message = match &error {
        embedrs::Error::Api {
            status, message, ..
        } => {
            let body: String = message.chars().take(512).collect();
            if *status == 429 {
                format!("embedding backend rate-limited (HTTP 429): {body}")
            } else {
                format!("embedding provider returned {status}: {body}")
            }
        }
        embedrs::Error::Timeout(duration) => {
            format!("embedding transport timeout after {duration:?}")
        }
        embedrs::Error::Http(inner) => format!("embedding transport error: {inner}"),
        embedrs::Error::Json(inner) => format!("malformed embedding response: {inner}"),
        embedrs::Error::InputTooLarge(size, max) => {
            format!("embedding batch too large: {size} texts (provider max {max})")
        }
        other => format!("embedding backend error: {other}"),
    };
    EmbedError::Backend(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_embeddings_suffix() {
        let full = Url::parse("http://127.0.0.1:11434/v1/embeddings").unwrap();
        assert_eq!(openai_compatible_base(&full), "http://127.0.0.1:11434/v1");

        let with_slash = Url::parse("http://127.0.0.1:11434/v1/embeddings/").unwrap();
        // Url::as_str keeps trailing slash only when present in input path; trim handles both.
        assert_eq!(
            openai_compatible_base(&with_slash),
            "http://127.0.0.1:11434/v1"
        );
    }

    #[test]
    fn leaves_base_url_alone() {
        let base = Url::parse("http://127.0.0.1:11434/v1").unwrap();
        assert_eq!(openai_compatible_base(&base), "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn role_mapping_is_asymmetric() {
        assert!(matches!(
            role_to_input_type(EmbedRole::Query),
            InputType::SearchQuery
        ));
        assert!(matches!(
            role_to_input_type(EmbedRole::Document),
            InputType::SearchDocument
        ));
    }
}
