//! Request DTOs — the serialized shapes the API *accepts* — and their lowering
//! into the typed domain vocabulary.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use arc_swap::ArcSwap;
use heart::{Language, PackageVersion, RegistryOrigin};
use crate::registry::package::{Coordinates as PackageCoordinates, PackageName};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strum::IntoEnumIterator;
use url::Url;

use crate::config::CustomRegistry;
use crate::error::{BadRequestReason, ServerError};
use crate::search::query::{
	AbstractQuery, Filter, LiteralQuery, PackageSelector, Pagination, Query, Search,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRequestDto {
	pub query: String,
	#[serde(default)]
	pub semantic: bool,
	#[serde(default)]
	pub ecosystems: Vec<Language>,
	#[serde(default)]
	pub packages: Vec<String>,
	pub limit: std::num::NonZeroU32,
	#[serde(default)]
	pub cursor: Option<String>,
	/// The caller's exploration session, when the results should accumulate
	/// into a session graph (the `/expand` surface).
	#[serde(default)]
	pub session: Option<uuid::Uuid>,
}

impl SearchRequestDto {
	/// Lower the wire request into the typed [`Search`]. A semantic opt-in
	/// becomes an [`AbstractQuery`] (which only the planner may escalate);
	/// everything else is a validated, operator-escaped literal.
	pub fn into_search(self) -> Result<Search<'static>, ServerError> {
		let query = if self.semantic {
			Query::Abstract(AbstractQuery::NaturalLanguage(self.query))
		} else {
			Query::Literal(LiteralQuery::parse(&self.query).map_err(BadRequestReason::from)?)
		};

		// Package names are ecosystem-scoped; a bare name is tried against every
		// requested (or, unstated, every known) ecosystem's grammar.
		let candidates: Vec<Language> = match self.ecosystems.as_slice() {
			[] => Language::iter().collect(),
			requested => requested.to_vec(),
		};
		let mut selectors = Vec::new();
		for raw in &self.packages {
			for &ecosystem in &candidates {
				if let Ok(name) = PackageName::new(ecosystem, raw.as_str()) {
					selectors.push(PackageSelector { name, version: None });
				}
			}
		}
		if !self.packages.is_empty() && selectors.is_empty() {
			return Err(BadRequestReason::NoValidPackageSelectors.into());
		}

		Ok(Search {
			query,
			filter: Filter {
				ecosystems: nonempty::NonEmpty::from_vec(self.ecosystems),
				packages: nonempty::NonEmpty::from_vec(selectors),
			},
			page: Pagination { limit: self.limit, after: self.cursor },
			_lifetime: std::marker::PhantomData,
		})
	}
}

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

/// Return the canonical default origin for ecosystems that have one (Rust, TS,
/// Python, Nix). For Go and Java there is no universal public registry, so
/// `origin` is required; omitting it is a 400 Bad Request.
fn required_or_default_origin(ecosystem: Language) -> Result<RegistryOrigin, ServerError> {
	match ecosystem {
		Language::Rust => Ok(RegistryOrigin::CratesIo),
		Language::Typescript => Ok(RegistryOrigin::NpmPublic),
		Language::Python => Ok(RegistryOrigin::PyPi),
		Language::Nix => Ok(RegistryOrigin::FlakeHub),
		Language::CSharp => Ok(RegistryOrigin::NuGet),
		Language::Go | Language::Java => {
			Err(BadRequestReason::MissingField { field: "origin" }.into())
		}
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

/// The wire error a DTO field rejection projects to.
/// Deprecated in favor of direct construction of `BadRequestReason` variants
/// (which are `Into<ServerError>` via the `#[from]` chain). Kept only for
/// any remaining call sites during the transition.
#[allow(dead_code)]
fn bad_request(error: impl std::fmt::Display) -> ServerError {
	// Fallback only; prefer typed.
	crate::error::BadRequestReason::MalformedQuery(
		crate::error::QueryError::Malformed {
			detail: error.to_string(),
			query: String::new(),
		},
	)
	.into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthDto {
	pub ready: bool,
	pub degraded: Vec<heart::BackendKind>,
}
