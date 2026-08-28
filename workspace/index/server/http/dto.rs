//! The server's lowering of wire DTOs into the typed domain — and the DTOs
//! that are genuinely server-local.
//!
//! # Single source of truth
//!
//! The *wire shapes* the client and server exchange — `AddPackageDto`,
//! `HealthDto`, `JobKeyHex`, `CompiledLookupRequest`/`Entry`/`Response`,
//! `RerankRequestDto`/`ResponseDto`, and their bounds — are defined **once**, in
//! [`heart::client::dto`], and re-exported here so existing `crate::server::
//! http::dto::…` paths keep resolving. `LOCAL-REMOTE-CONTRACT.md` §0.6 recorded
//! the defect this closes: these types used to be declared a *second* time in
//! this file, structurally identical to `heart`'s copy and free to drift from
//! it silently. There is now nothing to drift.
//!
//! What stays here is the half that is genuinely a *composition* concern and
//! cannot live in `heart` (which is transport- and store-free):
//!
//! - **Lowering** a wire DTO into typed domain coordinates — [`AddPackageExt::
//!   into_coordinates`] resolves an `origin` against the operator's custom-
//!   registry table; [`compiled_hit_entry`] projects a store hit into the wire
//!   entry. `heart`'s DTO doc is explicit that this lowering is a composition
//!   step, not a method on the wire type.
//! - **`DepshardManifestDto`**, whose fields reference `registry::vector`
//!   shard/quant types and so cannot cross into `heart`.
//! - The custom-registry origin table itself.
//!
//! Wire validation (`CompiledLookupRequest::validate`, `RerankRequestDto::
//! validate`) is `heart`'s inherent method; its `heart` error types lower to
//! [`ServerError`] via `From` impls in [`crate::server::error`], so a handler's
//! `req.validate()?` still yields the same typed `400` it always did.

#[allow(unused_imports)]
use crate::server::{registry, vector};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use crate::ecosystem::PackageNameExt as _;
use crate::server::registry::package::{Coordinates as PackageCoordinates, PackageName};
use arc_swap::ArcSwap;
use heart::{Language, PackageVersion, RegistryOrigin};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use url::Url;

use crate::server::config::CustomRegistry;
use crate::server::error::{BadRequestReason, ServerError};

// ── Wire shapes: re-exported from the single source of truth in `heart` ───────
//
// Handlers, tests, and the client all name these at `heart::client::dto`; the
// re-export keeps the `crate::server::http::dto::…` spelling working for the
// server's own call sites without a second definition to keep in sync.
pub use heart::client::dto::{
    AddPackageDto, COMPILED_LOOKUP_MAX_KEYS, CompiledLookupEntry, CompiledLookupRequest,
    CompiledLookupResponse, HealthDto, JobKeyHex, RERANK_MAX_DOCUMENTS, RerankRequestDto,
    RerankResponseDto,
};

// ── Add-package lowering ──────────────────────────────────────────────────────

/// Server-side lowering of an [`AddPackageDto`] into typed package coordinates.
///
/// This is an extension trait rather than an inherent method because the DTO
/// now lives in `heart` (which has no notion of a custom-registry table): the
/// wire shape is shared vocabulary, the lowering is composition. Bring the
/// trait into scope (`use crate::server::http::dto::AddPackageExt;`) to call
/// `dto.into_coordinates()`.
pub trait AddPackageExt {
    /// Resolve this request's ecosystem/name/version/origin into the typed
    /// [`PackageCoordinates`] the indexing pipeline consumes. Validates the
    /// name and version for the ecosystem and resolves `origin` against the
    /// operator's custom-registry table (falling back to the ecosystem default
    /// when absent).
    fn into_coordinates(&self) -> Result<PackageCoordinates, ServerError>;
}

impl AddPackageExt for AddPackageDto {
    fn into_coordinates(&self) -> Result<PackageCoordinates, ServerError> {
        let name =
            PackageName::new(self.ecosystem, self.name.as_str()).map_err(BadRequestReason::from)?;
        let version = PackageVersion::try_from((self.ecosystem, self.version.as_str()))
            .map_err(BadRequestReason::from)?;
        let origin = match self.origin.as_deref() {
            None => required_or_default_origin(self.ecosystem)?,
            Some(custom) => resolve_custom_origin(custom)?,
        };
        Ok(PackageCoordinates {
            origin,
            name,
            version,
        })
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
/// the origin lookup table. Called once per assembly from [`crate::server::Server`].
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
        .map(|url| RegistryOrigin::Custom {
            name: SmolStr::new(name),
            url: url.clone(),
        })
        .ok_or_else(|| {
            BadRequestReason::UnknownCustomRegistry {
                name: name.to_owned(),
            }
            .into()
        })
}

// ── Compiled-lookup projection ────────────────────────────────────────────────

/// Project a store hit into the wire [`CompiledLookupEntry`].
///
/// The server-only half of the compiled-lookup contract: it reads a
/// `registry::compiled::CompiledHit` (a store type `heart` cannot see) and
/// fills the wire entry's parts. The `heart` DTO stays store-type-agnostic.
///
/// The client trusts the mapping because (a) writes are fleet-only (SV-6), and
/// (b) the referenced change-set is verified AT IMPORT by Pijul content-
/// addressing when pulled over iroh — the response is a claim, the changes are
/// the proof.
pub fn compiled_hit_entry(
    job_key: &JobKeyHex,
    hit: &registry::compiled::CompiledHit,
) -> CompiledLookupEntry {
    CompiledLookupEntry::hit(
        job_key,
        hit.package.as_uuid().to_string(),
        hit.channel.as_str().to_owned(),
        hit.tip.as_str().to_owned(),
        hit.generation_stamp.hex(),
    )
}

// ── Dep-shard DTOs (09-vector §20.3) ─────────────────────────────────────────

/// The response body for `GET /v1/depshards/{package}/{version}/manifest`:
/// the full edgepack key (so the client can verify it derived the same
/// identity), the artifact id + RAM estimate once baked, and the lifecycle
/// status. `artifact_id`/`ram_estimate` are absent until `status == "ready"`.
///
/// This DTO stays server-local (not in `heart`) because its fields reference
/// `registry::vector` shard/quant identity types.
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
        row: Option<&crate::server::bakery::EdgepackRow>,
    ) -> Self {
        use crate::server::bakery::EdgepackStatus;
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
    pub fn is_ready(&self) -> bool {
        self.status == "ready"
    }
}

/// The stable wire token for a quantization profile (mirrors the bakery
/// fingerprint encoding; clients treat it as opaque).
fn quant_profile_token(profile: &vector::quant::QuantProfile) -> String {
    use vector::quant::QuantProfile;
    match profile {
        QuantProfile::None => "none".to_owned(),
        QuantProfile::ScalarInt8 {
            quantile,
            always_ram,
        } => {
            format!("scalar-int8/q{quantile}/ram{}", u8::from(*always_ram))
        }
    }
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
            recipe_id: smol_str::SmolStr::new(crate::server::bakery::RECIPE_ID),
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
        assert!(
            !object.contains_key("artifact_id"),
            "artifact_id elided while pending"
        );
        assert!(
            !object.contains_key("ram_estimate"),
            "ram_estimate elided while pending"
        );
        assert_eq!(object["status"], "pending");
        assert_eq!(
            object["quant_profile"],
            quant_profile_token(&vector::quant::QP1)
        );

        // Ready: artifact + estimate present, digests lower-hex.
        let row = crate::server::bakery::EdgepackRow {
            digest: key.digest(),
            status: crate::server::bakery::EdgepackStatus::Ready,
            artifact: Some(heart::ContentHash::of_bytes(b"artifact")),
            ram_estimate: Some(9216),
        };
        let ready = DepshardManifestDto::from_parts(&key, Some(&row));
        assert!(ready.is_ready());
        let value = serde_json::to_value(&ready).expect("serializes");
        assert_eq!(value["ram_estimate"], 9216);
        let artifact = value["artifact_id"].as_str().expect("hex artifact id");
        assert_eq!(artifact.len(), 64);
        assert!(
            artifact
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
