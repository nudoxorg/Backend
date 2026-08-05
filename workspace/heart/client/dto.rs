//! Request/response DTOs — the serialized shapes the API accepts and returns.
//!
//! These are *pure wire vocabulary*: serializable shapes with self-contained
//! validation, no transport and no composition-layer error types. The lowering
//! of a DTO into the typed domain (e.g. resolving an origin against the
//! operator's custom-registry table, or mapping a store hit into a response
//! entry) is a composition concern and lives in the staged server bits, which
//! own the `index`/`registry` handles the lowering reads.
//!
//! Moved here from the former `server::http::dto` (§8: wire DTOs the client
//! needs → `heart::client`). The DTOs that are genuinely entangled with the
//! vector plane (`DepshardManifestDto`) stay in the composition, since they
//! reference `registry::vector` shard/quant types.

use crate::ecosystem::Language;

// ── Add-package DTO ──────────────────────────────────────────────────────────

/// The request body for adding a package to the index.
///
/// This is the wire shape only. Lowering it into typed package coordinates
/// (resolving the `origin` against the configured custom-registry table) is a
/// composition step that needs the operator config, so it lives in the staged
/// server bits rather than here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AddPackageDto {
    /// The ecosystem the package belongs to.
    pub ecosystem: Language,
    /// The package name (validated per-ecosystem during lowering).
    pub name: String,
    /// The version string (validated per-ecosystem during lowering).
    pub version: String,
    /// An optional custom-registry origin name; resolved during lowering.
    #[serde(default)]
    pub origin: Option<String>,
}

// ── Compiled-lookup DTOs ─────────────────────────────────────────────────────

/// Maximum number of [`JobKeyHex`] values accepted in a single
/// `POST /v1/compiled/lookup` request (SMOLVM-PLAN §5.1).
pub const COMPILED_LOOKUP_MAX_KEYS: usize = 1024;

/// Why a [`JobKeyHex`] failed to validate at the list level.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum JobKeyHexError {
    /// The list was empty.
    #[error("compiled-lookup request carried no job_keys")]
    Empty,
    /// The list exceeded [`COMPILED_LOOKUP_MAX_KEYS`].
    #[error("too many job_keys in lookup request: {count} (maximum {max})")]
    TooMany {
        /// The count supplied.
        count: usize,
        /// The maximum allowed.
        max: usize,
    },
}

/// A validated, lower-hex-encoded [`crate::JobKey`] as received over the wire.
///
/// Validation happens at deserialize time:
/// - Must be exactly 64 lowercase hex characters (a 32-byte BLAKE3 digest).
/// - Upper-case hex is rejected.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(transparent)]
pub struct JobKeyHex(String);

impl JobKeyHex {
    /// The validated lower-hex string as received from the client.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Decode into the raw 32-byte digest. Infallible after construction
    /// (the constructor validates hex).
    pub fn into_bytes(self) -> [u8; 32] {
        let mut raw = [0u8; 32];
        data_encoding::HEXLOWER
            .decode_mut(self.0.as_bytes(), &mut raw)
            .expect("hex was validated at deserialize time");
        raw
    }
}

impl<'de> serde::Deserialize<'de> for JobKeyHex {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        if s.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "job_key must be exactly 64 hex chars, got {}",
                s.len()
            )));
        }
        // Reject upper-case hex and non-hex bytes.
        let mut raw = [0u8; 32];
        data_encoding::HEXLOWER
            .decode_mut(s.as_bytes(), &mut raw)
            .map_err(|_| serde::de::Error::custom("job_key is not valid lowercase hex"))?;
        Ok(JobKeyHex(s))
    }
}

impl std::fmt::Display for JobKeyHex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The request body for `POST /v1/compiled/lookup` (SMOLVM-PLAN §5.1).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CompiledLookupRequest {
    /// The job keys to look up (non-empty, ≤ [`COMPILED_LOOKUP_MAX_KEYS`]).
    pub job_keys: Vec<JobKeyHex>,
}

impl CompiledLookupRequest {
    /// Validate list-level constraints (non-empty; ≤ 1024 keys). Per-key
    /// validation already happened in [`JobKeyHex`]'s `Deserialize`.
    pub fn validate(&self) -> Result<(), JobKeyHexError> {
        if self.job_keys.is_empty() {
            return Err(JobKeyHexError::Empty);
        }
        if self.job_keys.len() > COMPILED_LOOKUP_MAX_KEYS {
            return Err(JobKeyHexError::TooMany {
                count: self.job_keys.len(),
                max: COMPILED_LOOKUP_MAX_KEYS,
            });
        }
        Ok(())
    }
}

/// One entry in the `POST /v1/compiled/lookup` response.
///
/// JSON shape:
/// - hit:  `{"job_key":"…","hit":true,"package":"…","channel":"…","tip":"<64hex>","generation_stamp":"<hex>"}`
/// - miss: `{"job_key":"…","hit":false}`
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompiledLookupEntry {
    /// The looked-up job key.
    pub job_key: String,
    /// Whether the lookup hit.
    pub hit: bool,
    /// The package uuid, present on hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Pijul channel name. Present on hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// 64-char lowercase hex tip change-hash. Present on hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    /// Hash① of the generation (64-char lower-hex). Present on hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_stamp: Option<String>,
}

impl CompiledLookupEntry {
    /// Construct a miss entry.
    pub fn miss(job_key: &JobKeyHex) -> Self {
        Self {
            job_key: job_key.as_str().to_owned(),
            hit: false,
            package: None,
            channel: None,
            tip: None,
            generation_stamp: None,
        }
    }

    /// Construct a hit entry from its parts (the composition maps a store hit
    /// into these fields — the DTO stays store-type-agnostic).
    pub fn hit(
        job_key: &JobKeyHex,
        package: String,
        channel: String,
        tip: String,
        generation_stamp: String,
    ) -> Self {
        Self {
            job_key: job_key.as_str().to_owned(),
            hit: true,
            package: Some(package),
            channel: Some(channel),
            tip: Some(tip),
            generation_stamp: Some(generation_stamp),
        }
    }
}

/// The response body for `POST /v1/compiled/lookup`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompiledLookupResponse {
    /// The per-key results.
    pub results: Vec<CompiledLookupEntry>,
}

// ── Health DTO ───────────────────────────────────────────────────────────────

/// The response body for the health surface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HealthDto {
    /// Whether the node is ready to serve.
    pub ready: bool,
    /// Backends currently degraded.
    pub degraded: Vec<crate::error::BackendKind>,
}

// ── Rerank DTOs (09-vector §20.8) ────────────────────────────────────────────

/// The largest number of documents one rerank call accepts.
pub const RERANK_MAX_DOCUMENTS: usize = 256;

/// One candidate document for reranking: the caller's opaque id plus the text
/// the cross-encoder scores against the query.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankDocument {
    /// The caller's identity for this document (echoed back on the score).
    pub id: String,
    /// The text scored against the query.
    pub text: String,
}

/// One reranked result: the document's id and its relevance score.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankScore {
    /// The id of the scored document.
    pub id: String,
    /// The relevance score (higher is more relevant).
    pub score: f32,
}

/// The request body for `POST /v1/rerank`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankRequestDto {
    /// The query the documents are scored against.
    pub query: String,
    /// The candidate documents.
    pub documents: Vec<RerankDocument>,
    /// How many results to return (descending relevance).
    pub top_k: std::num::NonZeroU32,
}

impl RerankRequestDto {
    /// Structural validation. Returns the count of documents when the batch is
    /// oversized so the composition can build its typed 400.
    pub fn validate(&self) -> Result<(), RerankRequestError> {
        if self.query.trim().is_empty() {
            return Err(RerankRequestError::EmptyQuery);
        }
        if self.documents.is_empty() {
            return Err(RerankRequestError::NoDocuments);
        }
        if self.documents.len() > RERANK_MAX_DOCUMENTS {
            return Err(RerankRequestError::TooManyDocuments {
                count: self.documents.len(),
                max: RERANK_MAX_DOCUMENTS,
            });
        }
        Ok(())
    }
}

/// Why a [`RerankRequestDto`] failed validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RerankRequestError {
    /// The query was empty after trimming.
    #[error("rerank query was empty")]
    EmptyQuery,
    /// No documents were supplied.
    #[error("rerank request carried no documents")]
    NoDocuments,
    /// Too many documents were supplied.
    #[error("too many rerank documents: {count} (maximum {max})")]
    TooManyDocuments {
        /// The count supplied.
        count: usize,
        /// The maximum allowed.
        max: usize,
    },
}

/// The response body for `POST /v1/rerank`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RerankResponseDto {
    /// Scores in descending relevance order, at most `top_k` of them.
    pub scores: Vec<RerankScore>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_key_hex_rejects_wrong_length_and_uppercase() {
        let too_short: Result<JobKeyHex, _> = serde_json::from_str("\"abcd\"");
        assert!(too_short.is_err());
        let upper: Result<JobKeyHex, _> = serde_json::from_str(&format!("\"{}\"", "A".repeat(64)));
        assert!(upper.is_err());
        let ok: JobKeyHex =
            serde_json::from_str(&format!("\"{}\"", "a".repeat(64))).expect("valid");
        assert_eq!(ok.as_str().len(), 64);
    }

    #[test]
    fn compiled_lookup_request_validates_bounds() {
        let empty = CompiledLookupRequest { job_keys: vec![] };
        assert_eq!(empty.validate(), Err(JobKeyHexError::Empty));
    }

    #[test]
    fn rerank_request_validation() {
        let valid = RerankRequestDto {
            query: "parse a toml file".to_owned(),
            documents: vec![RerankDocument {
                id: "a".to_owned(),
                text: "toml::from_str".to_owned(),
            }],
            top_k: std::num::NonZeroU32::new(5).expect("non-zero"),
        };
        assert!(valid.validate().is_ok());

        let mut empty_query = valid.clone();
        empty_query.query = "  ".to_owned();
        assert_eq!(empty_query.validate(), Err(RerankRequestError::EmptyQuery));

        let mut oversize = valid;
        oversize.documents = (0..=RERANK_MAX_DOCUMENTS)
            .map(|i| RerankDocument {
                id: i.to_string(),
                text: String::new(),
            })
            .collect();
        assert!(matches!(
            oversize.validate(),
            Err(RerankRequestError::TooManyDocuments { .. })
        ));
    }
}
