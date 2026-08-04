//! Request DTOs — the serialized shapes the API *accepts* — and their lowering
//! into the typed domain vocabulary.

//! Request DTOs — the serialized shapes the API *accepts* — and their lowering
//! into the typed domain vocabulary.
//!
//! The search surfaces no longer live here: `POST /search`, `/packages/search`,
//! and `/usages` deserialize the one query algebra ([`heart::query::Query`])
//! directly (INDEX-PLAN §9), so there is no `SearchRequestDto`. What remains are
//! the mutation/lookup DTOs (add-package, compiled-lookup, depshard manifest,
//! rerank) whose shapes are genuinely distinct from any query.

#[allow(unused_imports)]
use crate::{registry, vector};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwap;
use index::ecosystem::PackageNameExt as _;
use heart::{Language, PackageVersion, RegistryOrigin};
use crate::registry::package::{Coordinates as PackageCoordinates, PackageName};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use url::Url;

use crate::config::CustomRegistry;
use crate::error::{BadRequestReason, ServerError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddPackageDto {
	pub ecosystem: Language,
	pub name: String,
	pub version: String,
	#[serde(default)]
	pub origin: Option<String>,
}

impl AddPackageDto {
	pub fn into_coordinates(&self) -> Result<PackageCoordinates, ServerError> {
		let name = PackageName::new(self.ecosystem, self.name.as_str())
			.map_err(BadRequestReason::from)?;
		let version = PackageVersion::try_from((self.ecosystem, self.version.as_str()))
			.map_err(BadRequestReason::from)?;
		let origin = match self.origin.as_deref() {
			None => required_or_default_origin(self.ecosystem)?,
			Some(custom) => resolve_custom_origin(custom)?,
		};
		Ok(PackageCoordinates { origin, name, version })
	}
}

/// The canonical default origin per ecosystem. Every ecosystem now has one —
/// Go resolves via proxy.golang.org and Java via Maven Central (M5); an
/// explicit `origin` still overrides for self-hosted registries.
fn required_or_default_origin(ecosystem: Language) -> Result<RegistryOrigin, ServerError> {
	match ecosystem {
		Language::Rust => Ok(RegistryOrigin::CratesIo),
		Language::Typescript => Ok(RegistryOrigin::NpmPublic),
		Language::Python => Ok(RegistryOrigin::PyPi),
		Language::Nix => Ok(RegistryOrigin::FlakeHub),
		Language::CSharp => Ok(RegistryOrigin::NuGet),
		Language::Go => Ok(RegistryOrigin::GoProxy),
		Language::Java => Ok(RegistryOrigin::MavenCentral),
		// `cpp` is registry-less: the git repository is the package (RL-1).
		Language::Cpp => Ok(RegistryOrigin::Git),
	}
}

/// The process-wide lookup table behind the `origin` field of an add-package
/// request. A free function ([`resolve_custom_origin`]) resolves against it, so
/// the table is installed once at assembly rather than threaded through every
/// DTO conversion.
static CUSTOM_REGISTRIES: LazyLock<ArcSwap<HashMap<SmolStr, Url>>> =
	LazyLock::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Install (replacing wholesale) the operator-configured custom registries as
/// the origin lookup table. Called once per assembly from [`crate::Server`].
pub fn register_custom_registries(registries: &[CustomRegistry]) {
	let table: HashMap<SmolStr, Url> = registries
		.iter()
		.map(|registry| (registry.name.clone(), registry.url.clone()))
		.collect();
	tracing::debug!(registries = table.len(), "custom registries registered");
	CUSTOM_REGISTRIES.store(Arc::new(table));
}

fn resolve_custom_origin(name: &str) -> Result<RegistryOrigin, ServerError> {
	CUSTOM_REGISTRIES
		.load()
		.get(name)
		.map(|url| RegistryOrigin::Custom { name: SmolStr::new(name), url: url.clone() })
		.ok_or_else(|| BadRequestReason::UnknownCustomRegistry { name: name.to_owned() }.into())
}

// ── Compiled-lookup DTOs ─────────────────────────────────────────────────────

/// Maximum number of [`JobKeyHex`] values accepted in a single
/// `POST /v1/compiled/lookup` request (SMOLVM-PLAN §5.1).
pub const COMPILED_LOOKUP_MAX_KEYS: usize = 1024;

/// A validated, lower-hex-encoded [`heart::JobKey`] as received over the wire.
///
/// Validation happens at deserialize time:
/// - Must be exactly 64 lowercase hex characters (a 32-byte BLAKE3 digest).
/// - Upper-case hex is rejected.
///
/// The inner `[u8; 32]` is the raw digest; use [`JobKeyHex::into_bytes`] to
/// obtain it after deserialization.
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
///
/// Validation:
/// - `job_keys` must be non-empty.
/// - `job_keys` must contain ≤ [`COMPILED_LOOKUP_MAX_KEYS`] entries.
/// - Each key must be exactly 64 lowercase hex characters.
///
/// These constraints are enforced lazily (at the `.validate()` call in the
/// handler) rather than in `Deserialize`, so the handler can return a typed
/// `400` with a structured error rather than axum's default JSON rejection.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CompiledLookupRequest {
    pub job_keys: Vec<JobKeyHex>,
}

impl CompiledLookupRequest {
    /// Validate list-level constraints (non-empty; ≤ 1024 keys).
    /// Per-key validation already happened in `JobKeyHex`'s `Deserialize`.
    pub fn validate(&self) -> Result<(), ServerError> {
        if self.job_keys.is_empty() {
            return Err(BadRequestReason::MissingField { field: "job_keys" }.into());
        }
        if self.job_keys.len() > COMPILED_LOOKUP_MAX_KEYS {
            return Err(BadRequestReason::TooManyJobKeys {
                count: self.job_keys.len(),
                max: COMPILED_LOOKUP_MAX_KEYS,
            }
            .into());
        }
        Ok(())
    }
}

/// One entry in the `POST /v1/compiled/lookup` response.
///
/// JSON shape:
/// - hit:  `{"job_key":"…","hit":true,"package":"…","channel":"…","tip":"<64hex>","generation_stamp":"<hex>"}`
/// - miss: `{"job_key":"…","hit":false}`
///
/// The client trusts the mapping because (a) writes are fleet-only (SV-6), and
/// (b) the referenced change-set is verified AT IMPORT by Pijul content-
/// addressing when pulled over iroh — the response is a claim, the changes
/// are the proof.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompiledLookupEntry {
    pub job_key: String,
    pub hit: bool,
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

    /// Construct a hit entry from a store hit.
    pub fn hit(job_key: &JobKeyHex, hit: &registry::compiled::CompiledHit) -> Self {
        Self {
            job_key: job_key.as_str().to_owned(),
            hit: true,
            package: Some(hit.package.as_uuid().to_string()),
            channel: Some(hit.channel.as_str().to_owned()),
            tip: Some(hit.tip.as_str().to_owned()),
            generation_stamp: Some(hit.generation_stamp.hex()),
        }
    }
}

/// The response body for `POST /v1/compiled/lookup`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompiledLookupResponse {
    pub results: Vec<CompiledLookupEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	pub ready: bool,
	pub degraded: Vec<heart::BackendKind>,
}

// ── Dep-shard DTOs (09-vector §20.3) ─────────────────────────────────────────

/// The response body for `GET /v1/depshards/{package}/{version}/manifest`:
/// the full edgepack key (so the client can verify it derived the same
/// identity), the artifact id + RAM estimate once baked, and the lifecycle
/// status. `artifact_id`/`ram_estimate` are absent until `status == "ready"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepshardManifestDto {
	/// The package uuid.
	pub package: String,
	/// The package's canonical version.
	pub version: String,
	/// The embedding model id the shard was baked under.
	pub model_id: String,
	/// The embed-text recipe revision.
	pub recipe_id: String,
	/// The quantization profile token (`qp1`).
	pub quant_profile: String,
	/// The qdrant-edge on-disk format version.
	pub edge_format_version: u32,
	/// Lower-hex blake3 of the edgepack key — the artifact's identity.
	pub edgepack_key_digest: String,
	/// Lower-hex blake3 of the packed artifact bytes (the CAS key), once ready.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub artifact_id: Option<String>,
	/// The client-side admission estimate in bytes (§20.4), once ready.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ram_estimate: Option<i64>,
	/// `pending` | `ready` | `failed`.
	pub status: String,
}

impl DepshardManifestDto {
	/// Assemble from the computed key and the (possibly absent) ledger row.
	/// No row, or a row still `claimed`, presents as `pending` — the bakery
	/// poller will (or already did) pick the package up.
	pub fn from_parts(
		key: &vector::shard::EdgepackKey,
		row: Option<&crate::bakery::EdgepackRow>,
	) -> Self {
		use crate::bakery::EdgepackStatus;
		let status = match row.map(|row| row.status) {
			Some(EdgepackStatus::Ready) => "ready",
			Some(EdgepackStatus::Failed) => "failed",
			Some(EdgepackStatus::Claimed) | None => "pending",
		};
		Self {
			package: key.package.to_string(),
			version: key.version.to_string(),
			model_id: key.model_id.to_string(),
			recipe_id: key.recipe_id.to_string(),
			quant_profile: quant_profile_token(&key.quant_profile),
			edge_format_version: key.edge_format_version,
			edgepack_key_digest: key.digest().hex(),
			artifact_id: row.and_then(|row| row.artifact).map(|hash| hash.hex()),
			ram_estimate: row.and_then(|row| row.ram_estimate),
			status: status.to_owned(),
		}
	}

	/// Whether the artifact is servable.
	pub fn is_ready(&self) -> bool { self.status == "ready" }
}

/// The stable wire token for a quantization profile (mirrors the bakery
/// fingerprint encoding; clients treat it as opaque).
fn quant_profile_token(profile: &vector::quant::QuantProfile) -> String {
	use vector::quant::QuantProfile;
	match profile {
		QuantProfile::None => "none".to_owned(),
		QuantProfile::ScalarInt8 { quantile, always_ram } => {
			format!("scalar-int8/q{quantile}/ram{}", u8::from(*always_ram))
		}
	}
}

// ── Rerank DTOs (09-vector §20.8) ────────────────────────────────────────────

/// The largest number of documents one rerank call accepts (the deep path
/// sends the Stage-1 top-100; a generous ceiling bounds abuse).
pub const RERANK_MAX_DOCUMENTS: usize = 256;

/// The request body for `POST /v1/rerank`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankRequestDto {
	/// The query the documents are scored against.
	pub query: String,
	/// The candidate documents.
	pub documents: Vec<crate::rerank::RerankDocument>,
	/// How many results to return (descending relevance).
	pub top_k: std::num::NonZeroU32,
}

impl RerankRequestDto {
	/// Structural validation, surfaced as typed 400s.
	pub fn validate(&self) -> Result<(), ServerError> {
		if self.query.trim().is_empty() {
			return Err(BadRequestReason::MissingField { field: "query" }.into());
		}
		if self.documents.is_empty() {
			return Err(BadRequestReason::MissingField { field: "documents" }.into());
		}
		if self.documents.len() > RERANK_MAX_DOCUMENTS {
			return Err(BadRequestReason::MalformedQuery(crate::error::QueryError::Malformed {
				detail: format!(
					"too many rerank documents: {} (maximum {RERANK_MAX_DOCUMENTS})",
					self.documents.len()
				),
				query: String::new(),
			})
			.into());
		}
		Ok(())
	}
}

/// The response body for `POST /v1/rerank`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResponseDto {
	/// Scores in descending relevance order, at most `top_k` of them.
	pub scores: Vec<crate::rerank::RerankScore>,
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The manifest wire shape: every edgepack-key field present, hex digests,
	/// and the optional fields elided until ready.
	#[test]
	fn depshard_manifest_serde_shape() {
		let key = vector::shard::EdgepackKey {
			package: heart::PackageId::from_uuid(uuid::Uuid::from_u128(3)),
			version: smol_str::SmolStr::new("0.4.2"),
			model_id: vector::ModelId::new("jinaai/jina-embeddings-v2-base-code"),
			recipe_id: smol_str::SmolStr::new(crate::bakery::RECIPE_ID),
			quant_profile: vector::quant::QP1,
			edge_format_version: vector::shard::EDGE_FORMAT_VERSION,
		};

		// Pending: no row yet → optional fields elided, status pending.
		let pending = DepshardManifestDto::from_parts(&key, None);
		let value = serde_json::to_value(&pending).expect("serializes");
		let object = value.as_object().expect("object");
		for field in [
			"package",
			"version",
			"model_id",
			"recipe_id",
			"quant_profile",
			"edge_format_version",
			"edgepack_key_digest",
			"status",
		] {
			assert!(object.contains_key(field), "manifest must carry `{field}`");
		}
		assert!(!object.contains_key("artifact_id"), "artifact_id elided while pending");
		assert!(!object.contains_key("ram_estimate"), "ram_estimate elided while pending");
		assert_eq!(object["status"], "pending");
		assert_eq!(object["quant_profile"], quant_profile_token(&vector::quant::QP1));

		// Ready: artifact + estimate present, digests lower-hex.
		let row = crate::bakery::EdgepackRow {
			digest: key.digest(),
			status: crate::bakery::EdgepackStatus::Ready,
			artifact: Some(heart::ContentHash::of_bytes(b"artifact")),
			ram_estimate: Some(9216),
		};
		let ready = DepshardManifestDto::from_parts(&key, Some(&row));
		assert!(ready.is_ready());
		let value = serde_json::to_value(&ready).expect("serializes");
		assert_eq!(value["ram_estimate"], 9216);
		let artifact = value["artifact_id"].as_str().expect("hex artifact id");
		assert_eq!(artifact.len(), 64);
		assert!(artifact.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
	}

	/// Rerank request validation: empty query/documents and oversize batches
	/// are typed 400s.
	#[test]
	fn rerank_request_validation() {
		let valid = RerankRequestDto {
			query: "parse a toml file".to_owned(),
			documents: vec![crate::rerank::RerankDocument {
				id: "a".to_owned(),
				text: "toml::from_str".to_owned(),
			}],
			top_k: std::num::NonZeroU32::new(5).expect("non-zero"),
		};
		assert!(valid.validate().is_ok());

		let mut empty_query = valid.clone();
		empty_query.query = "  ".to_owned();
		assert!(empty_query.validate().is_err());

		let mut no_documents = valid.clone();
		no_documents.documents.clear();
		assert!(no_documents.validate().is_err());

		let mut oversize = valid;
		oversize.documents = (0..=RERANK_MAX_DOCUMENTS)
			.map(|i| crate::rerank::RerankDocument { id: i.to_string(), text: String::new() })
			.collect();
		assert!(oversize.validate().is_err());
	}
}
