//! [`ConfigId`] — content-addressed identifier for a feature+target compilation
//! configuration. Used to key per-config `ApiSurface` snapshots (§9.7).
//!
//! Domain: `nudox.config.v1` (frozen).

use serde::{Deserialize, Serialize};

use nudox_ir::change::ContentBlake3;

// ---------------------------------------------------------------------------
// ConfigId
// ---------------------------------------------------------------------------

/// Content-addressed identifier for a `(sorted-features, target-triple)` build
/// configuration. Two configs with identical feature sets and identical target
/// triple are the same `ConfigId`.
///
/// Domain `nudox.config.v1` (frozen — changing the preimage format requires a
/// new domain tag and a new type epoch).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
pub struct ConfigId(ContentBlake3);

impl ConfigId {
    /// `nudox.config.v1` domain tag (frozen).
    pub const DOMAIN: &'static str = "nudox.config.v1";

    /// The canonical default configuration: no extra features, no target triple
    /// (host). Represents the "all-features" or "default" canonical channel
    /// surface (§9.7).
    pub const DEFAULT: Self = Self(ContentBlake3::from_raw([0u8; 32]));

    /// Derive a `ConfigId` from a sorted feature list and a target triple.
    ///
    /// Features MUST be passed sorted (ascending, by raw bytes) and
    /// deduplicated. The preimage is:
    /// `"nudox.config.v1" || u32le(feature_count) || (u32le(len) || utf8)* || u32le(triple_len) || utf8_triple`
    pub fn from_parts(sorted_features: &[&str], target_triple: &str) -> Self {
        use nudox_ir::change::encode::{encode_segments, encode_str};
        let mut preimage = Vec::new();
        encode_segments(&mut preimage, sorted_features);
        encode_str(&mut preimage, target_triple);
        Self(ContentBlake3::from_domain(Self::DOMAIN, &preimage))
    }

    /// Raw content hash.
    #[inline]
    pub fn as_content_blake3(&self) -> ContentBlake3 {
        self.0
    }

    /// Lowercase hex representation.
    pub fn to_hex(&self) -> String {
        self.0.to_hex()
    }
}
