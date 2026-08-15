//! Edgepack shard identity: the `schema.json` contents baked into every
//! shard and the cache key an edgepack is published/fetched under
//! (09-vector §20.3 shard bakery).

use heart::{ContentHash, ContentHasher, PackageId};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use super::{
    model::{Metric, ModelId},
    quant::QuantProfile,
};

/// The on-disk edgepack format version. Bumping it invalidates every cached
/// shard (it participates in [`EdgepackKey::digest`]).
pub const EDGE_FORMAT_VERSION: u32 = 1;

/// BLAKE3 domain literal for edgepack keys.
const EDGEPACK_KEY_DOMAIN: &str = "nudox.edgepack_key.v1";

/// The `schema.json` of a shard: everything a loader must verify before
/// serving it (I8: mismatched schema is rejected, never coerced).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardSchema {
    /// [`EDGE_FORMAT_VERSION`] at bake time.
    pub format_version: u32,
    /// The embedding model whose vectors this shard holds.
    pub model_id: ModelId,
    /// Vector dimensionality (must match the model brand).
    pub dim: usize,
    /// Distance metric the index was built for.
    pub distance: Metric,
    /// Quantization the vectors were stored under.
    pub quant_profile: QuantProfile,
    /// The embed-text recipe the vectors were built with
    /// ([`crate::recipe::RECIPE_ID`]) — recipe drift is shard drift (I1).
    pub recipe_id: String,
}

/// The identity of one baked edgepack: (package, version) × model × recipe ×
/// quant × format. Two planes computing the same key may share the artifact
/// byte-for-byte; any component change is a new bake.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EdgepackKey {
    pub package: PackageId,
    /// The resolved package version string.
    pub version: SmolStr,
    pub model_id: ModelId,
    /// [`crate::recipe::RECIPE_ID`] at bake time.
    pub recipe_id: SmolStr,
    pub quant_profile: QuantProfile,
    /// [`EDGE_FORMAT_VERSION`] at bake time.
    pub edge_format_version: u32,
}

/// Length-prefixed field fold (the `JobKey::derive` convention).
fn fold(h: &mut ContentHasher, bytes: &[u8]) {
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

impl EdgepackKey {
    /// The content-addressed cache key: domain-separated BLAKE3 over every
    /// identity component. Distinct quant profiles (and distinct quantiles /
    /// residency flags within `ScalarInt8`) digest differently.
    pub fn digest(&self) -> ContentHash {
        let mut h = ContentHash::builder();
        fold(&mut h, EDGEPACK_KEY_DOMAIN.as_bytes());
        fold(&mut h, self.package.as_uuid().as_bytes());
        fold(&mut h, self.version.as_bytes());
        fold(&mut h, self.model_id.as_str().as_bytes());
        fold(&mut h, self.recipe_id.as_bytes());
        match self.quant_profile {
            QuantProfile::None => fold(&mut h, b"quant:none"),
            QuantProfile::ScalarInt8 {
                quantile,
                always_ram,
            } => {
                fold(&mut h, b"quant:scalar-int8");
                fold(&mut h, &quantile.to_le_bytes());
                fold(&mut h, &[u8::from(always_ram)]);
            }
        }
        fold(&mut h, &self.edge_format_version.to_le_bytes());
        h.finalize()
    }
}

#[cfg(test)]
mod tests {
    use super::super::quant::QuantProfile;
    use super::super::{
        model::EmbeddingModel, model::JinaCodeV2, quant::QP1, store::NAMESPACE_NUDOX,
    };
    use super::*;

    fn key() -> EdgepackKey {
        EdgepackKey {
            package: PackageId::from_name(&NAMESPACE_NUDOX, b"serde"),
            version: "1.0.219".into(),
            model_id: JinaCodeV2::id(),
            recipe_id: super::super::recipe::RECIPE_ID.into(),
            quant_profile: QP1,
            edge_format_version: EDGE_FORMAT_VERSION,
        }
    }

    #[test]
    fn digest_is_deterministic() {
        assert_eq!(key().digest(), key().digest());
    }

    // ── adversarial: EdgepackKey digest changes for each field independently ──

    /// always_ram flag change within ScalarInt8 must change the digest.
    #[test]
    fn digest_changes_on_always_ram_flag() {
        let base = key().digest();
        let mut k = key();
        k.quant_profile = QuantProfile::ScalarInt8 {
            quantile: 0.99,
            always_ram: false,
        };
        assert_ne!(
            base,
            k.digest(),
            "always_ram=false must differ from always_ram=true"
        );
    }

    /// QuantProfile::None vs ScalarInt8 with the same quantile byte pattern.
    #[test]
    fn digest_none_vs_scalar_int8_differ() {
        let base_none = {
            let mut k = key();
            k.quant_profile = QuantProfile::None;
            k.digest()
        };
        let with_int8 = {
            let mut k = key();
            k.quant_profile = QuantProfile::ScalarInt8 {
                quantile: 0.0,
                always_ram: false,
            };
            k.digest()
        };
        assert_ne!(
            base_none, with_int8,
            "None and ScalarInt8 must produce different digests even with quantile=0"
        );
    }

    // ── adversarial: ShardSchema JSON roundtrip ───────────────────────────────

    /// ShardSchema must roundtrip through serde_json.
    #[test]
    fn shard_schema_json_roundtrip() {
        let schema = ShardSchema {
            format_version: EDGE_FORMAT_VERSION,
            model_id: JinaCodeV2::id(),
            dim: JinaCodeV2::DIMENSIONS,
            distance: super::super::model::Metric::Cosine,
            quant_profile: QP1,
            recipe_id: super::super::recipe::RECIPE_ID.to_owned(),
        };
        let json = serde_json::to_string(&schema).unwrap();
        let restored: ShardSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, restored, "ShardSchema must roundtrip through JSON");
    }

    /// ShardSchema roundtrip with QuantProfile::None.
    #[test]
    fn shard_schema_json_roundtrip_none_quant() {
        let schema = ShardSchema {
            format_version: EDGE_FORMAT_VERSION,
            model_id: JinaCodeV2::id(),
            dim: JinaCodeV2::DIMENSIONS,
            distance: super::super::model::Metric::Cosine,
            quant_profile: QuantProfile::None,
            recipe_id: super::super::recipe::RECIPE_ID.to_owned(),
        };
        let json = serde_json::to_string(&schema).unwrap();
        let restored: ShardSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(schema, restored);
    }

    /// Unknown field in ShardSchema JSON: pin whether serde accepts or rejects it.
    ///
    /// FROZEN-KNOWN-BEHAVIOR: ShardSchema derives Deserialize without
    /// `#[serde(deny_unknown_fields)]`. Unknown fields are SILENTLY IGNORED.
    /// This is pinned here so future tightening is a conscious choice.
    #[test]
    fn shard_schema_unknown_field_silently_accepted_frozen_behavior() {
        let json = r#"{
			"format_version": 1,
			"model_id": "jinaai/jina-embeddings-v2-base-code",
			"dim": 768,
			"distance": "Cosine",
			"quant_profile": "None",
			"recipe_id": "nudox.embedtext.v2",
			"UNKNOWN_FUTURE_FIELD": "some value"
		}"#;
        // Frozen: serde_json ignores unknown fields by default.
        let result: Result<ShardSchema, _> = serde_json::from_str(json);
        // Pin: accepted (no deny_unknown_fields).
        assert!(
            result.is_ok(),
            "FROZEN: ShardSchema accepts unknown fields silently (no deny_unknown_fields): {result:?}"
        );
    }

    #[test]
    fn digest_changes_with_every_identity_component() {
        let base = key().digest();

        let mut k = key();
        k.version = "1.0.220".into();
        assert_ne!(base, k.digest());

        let mut k = key();
        k.package = PackageId::from_name(&NAMESPACE_NUDOX, b"tokio");
        assert_ne!(base, k.digest());

        let mut k = key();
        k.model_id = ModelId::new("voyage/voyage-code-3");
        assert_ne!(base, k.digest());

        let mut k = key();
        k.recipe_id = "nudox.embedtext.v3".into();
        assert_ne!(base, k.digest());

        let mut k = key();
        k.quant_profile = QuantProfile::None;
        assert_ne!(base, k.digest());

        let mut k = key();
        k.quant_profile = QuantProfile::ScalarInt8 {
            quantile: 0.95,
            always_ram: true,
        };
        assert_ne!(base, k.digest(), "quantile is part of identity");

        let mut k = key();
        k.edge_format_version = 2;
        assert_ne!(base, k.digest(), "format bump invalidates every shard");
    }
}
