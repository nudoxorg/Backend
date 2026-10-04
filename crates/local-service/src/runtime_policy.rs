//! Bounded, versioned policy carried from a desktop host into a cold locald.
//!
//! The value contains only effective permissions and a registry-cache horizon.
//! It never carries registry endpoints, source paths, credentials, or compiler
//! locations.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;

/// Environment variable carrying the desktop's effective locald policy.
pub const RUNTIME_POLICY_ENV: &str = "BACKEND_LOCALD_RUNTIME_POLICY";

/// Maximum UTF-8 bytes accepted or emitted by the runtime-policy codec.
pub const MAX_RUNTIME_POLICY_BYTES: usize = 512;

/// Maximum registry cache age admitted from a desktop policy (90 days).
pub const MAX_REGISTRY_CACHE_AGE_MILLIS: u64 = 90 * 86_400_000;

const RUNTIME_POLICY_VERSION: u8 = 1;

/// Effective network and cache permissions for one embedded locald.
///
/// Use [`ProcessConfig::runtime_policy`](crate::process::ProcessConfig::runtime_policy)
/// to capture the final owner settings. The fields remain private so callers
/// cannot construct a value that bypasses version, range, or codec admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocaldRuntimePolicy {
    registry_network_allowed: bool,
    discovery_network_allowed: bool,
    advisory_network_allowed: bool,
    advisory_refresh_enabled: bool,
    registry_cache_max_age_millis: Option<u64>,
}

impl LocaldRuntimePolicy {
    pub(crate) fn new(
        registry_network_allowed: bool,
        discovery_network_allowed: bool,
        advisory_network_allowed: bool,
        advisory_refresh_enabled: bool,
        registry_cache_max_age_millis: Option<u64>,
    ) -> Result<Self, RuntimePolicyError> {
        if registry_cache_max_age_millis.is_some_and(|age| age > MAX_REGISTRY_CACHE_AGE_MILLIS) {
            return Err(RuntimePolicyError::CacheAgeExceeded);
        }
        Ok(Self {
            registry_network_allowed,
            discovery_network_allowed,
            advisory_network_allowed,
            advisory_refresh_enabled,
            registry_cache_max_age_millis,
        })
    }

    /// Whether registry acquisition may use the network.
    #[must_use]
    pub const fn registry_network_allowed(self) -> bool {
        self.registry_network_allowed
    }

    /// Whether package-catalog discovery may use the network.
    #[must_use]
    pub const fn discovery_network_allowed(self) -> bool {
        self.discovery_network_allowed
    }

    /// Whether advisory sources may be read from the network.
    #[must_use]
    pub const fn advisory_network_allowed(self) -> bool {
        self.advisory_network_allowed
    }

    /// Whether explicit advisory refresh work is enabled.
    #[must_use]
    pub const fn advisory_refresh_enabled(self) -> bool {
        self.advisory_refresh_enabled
    }

    /// Exact cache-age setting; `None` retains locald's default, and `Some(0)`
    /// explicitly disables registry-result reuse.
    #[must_use]
    pub const fn registry_cache_max_age_millis(self) -> Option<u64> {
        self.registry_cache_max_age_millis
    }

    /// Encodes this policy as the canonical version-1 JSON object.
    ///
    /// # Errors
    /// Returns an error if the policy exceeds the cache-age or encoded-size
    /// bound. The encoded form has fixed field order and no insignificant
    /// whitespace.
    pub fn encode(self) -> Result<String, RuntimePolicyError> {
        let value = RuntimePolicyV1 {
            version: RUNTIME_POLICY_VERSION,
            registry_network_allowed: self.registry_network_allowed,
            discovery_network_allowed: self.discovery_network_allowed,
            advisory_network_allowed: self.advisory_network_allowed,
            advisory_refresh_enabled: self.advisory_refresh_enabled,
            registry_cache_max_age_millis: self.registry_cache_max_age_millis,
        };
        let encoded = serde_json::to_string(&value).map_err(|_| RuntimePolicyError::InvalidJson)?;
        if encoded.len() > MAX_RUNTIME_POLICY_BYTES {
            return Err(RuntimePolicyError::TooLarge);
        }
        Ok(encoded)
    }

    /// Parses and admits one canonical version-1 JSON policy.
    ///
    /// Unknown, missing, or duplicate fields are rejected. Parsing does not
    /// include raw input in errors, so malformed environment values cannot
    /// leak secrets or paths through startup diagnostics.
    ///
    /// # Errors
    /// Returns an error for an oversized, malformed, noncanonical, unsupported,
    /// or out-of-range policy.
    pub fn parse(encoded: &str) -> Result<Self, RuntimePolicyError> {
        if encoded.len() > MAX_RUNTIME_POLICY_BYTES {
            return Err(RuntimePolicyError::TooLarge);
        }
        let value: RuntimePolicyV1Input =
            serde_json::from_str(encoded).map_err(|_| RuntimePolicyError::InvalidJson)?;
        if value.version != RUNTIME_POLICY_VERSION {
            return Err(RuntimePolicyError::UnsupportedVersion);
        }
        let policy = Self::new(
            value.registry_network_allowed,
            value.discovery_network_allowed,
            value.advisory_network_allowed,
            value.advisory_refresh_enabled,
            value.registry_cache_max_age_millis.0,
        )?;
        if policy.encode()?.as_bytes() != encoded.as_bytes() {
            return Err(RuntimePolicyError::NonCanonical);
        }
        Ok(policy)
    }
}

/// Safe, value-free reason a runtime policy could not be encoded or admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePolicyError {
    /// The input or output exceeds [`MAX_RUNTIME_POLICY_BYTES`].
    TooLarge,
    /// The JSON shape is malformed, incomplete, duplicated, or has unknown fields.
    InvalidJson,
    /// The value is not the canonical encoding produced by [`LocaldRuntimePolicy::encode`].
    NonCanonical,
    /// The policy version is not supported by this locald.
    UnsupportedVersion,
    /// Registry cache age exceeds the 90-day policy ceiling.
    CacheAgeExceeded,
}

impl fmt::Display for RuntimePolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLarge => "runtime policy exceeds its size limit",
            Self::InvalidJson => "runtime policy is not a complete supported JSON object",
            Self::NonCanonical => "runtime policy is not canonically encoded",
            Self::UnsupportedVersion => "runtime policy version is unsupported",
            Self::CacheAgeExceeded => "runtime policy cache age exceeds 90 days",
        })
    }
}

impl std::error::Error for RuntimePolicyError {}

#[derive(Serialize)]
struct RuntimePolicyV1 {
    version: u8,
    registry_network_allowed: bool,
    discovery_network_allowed: bool,
    advisory_network_allowed: bool,
    advisory_refresh_enabled: bool,
    registry_cache_max_age_millis: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePolicyV1Input {
    version: u8,
    registry_network_allowed: bool,
    discovery_network_allowed: bool,
    advisory_network_allowed: bool,
    advisory_refresh_enabled: bool,
    registry_cache_max_age_millis: RequiredOptionalU64,
}

/// A present JSON key whose value is either an unsigned integer or null.
/// Using a non-Option wrapper makes the key itself required during decoding.
struct RequiredOptionalU64(Option<u64>);

impl<'de> Deserialize<'de> for RequiredOptionalU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<u64>::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LocaldRuntimePolicy, MAX_REGISTRY_CACHE_AGE_MILLIS, MAX_RUNTIME_POLICY_BYTES,
        RuntimePolicyError,
    };

    fn allowed(cache_age: Option<u64>) -> LocaldRuntimePolicy {
        LocaldRuntimePolicy::new(true, true, true, true, cache_age).expect("bounded runtime policy")
    }

    #[test]
    fn canonical_codec_preserves_none_zero_and_positive_cache_ages() {
        let none = allowed(None);
        let encoded = none.encode().expect("encode unbounded default");
        assert_eq!(
            encoded,
            r#"{"version":1,"registry_network_allowed":true,"discovery_network_allowed":true,"advisory_network_allowed":true,"advisory_refresh_enabled":true,"registry_cache_max_age_millis":null}"#
        );
        assert_eq!(LocaldRuntimePolicy::parse(&encoded), Ok(none));

        for age in [Some(0), Some(42), Some(MAX_REGISTRY_CACHE_AGE_MILLIS)] {
            let policy = allowed(age);
            assert_eq!(
                LocaldRuntimePolicy::parse(&policy.encode().expect("encode policy")),
                Ok(policy)
            );
        }
        assert_ne!(allowed(None), allowed(Some(0)));
    }

    #[test]
    fn codec_rejects_missing_unknown_version_and_noncanonical_fields() {
        let canonical = allowed(None).encode().expect("encode policy");
        let missing_cache = canonical.replace(",\"registry_cache_max_age_millis\":null", "");
        assert_eq!(
            LocaldRuntimePolicy::parse(&missing_cache),
            Err(RuntimePolicyError::InvalidJson)
        );
        let unknown = canonical.replace("\"version\":1", "\"version\":1,\"extra\":false");
        assert_eq!(
            LocaldRuntimePolicy::parse(&unknown),
            Err(RuntimePolicyError::InvalidJson)
        );
        let duplicate = canonical.replace("\"version\":1", "\"version\":1,\"version\":1");
        assert_eq!(
            LocaldRuntimePolicy::parse(&duplicate),
            Err(RuntimePolicyError::InvalidJson)
        );
        let wrong_boolean = canonical.replace(
            "\"registry_network_allowed\":true",
            "\"registry_network_allowed\":1",
        );
        assert_eq!(
            LocaldRuntimePolicy::parse(&wrong_boolean),
            Err(RuntimePolicyError::InvalidJson)
        );
        let future = canonical.replace("\"version\":1", "\"version\":2");
        assert_eq!(
            LocaldRuntimePolicy::parse(&future),
            Err(RuntimePolicyError::UnsupportedVersion)
        );
        let whitespace = format!(" {canonical}");
        assert_eq!(
            LocaldRuntimePolicy::parse(&whitespace),
            Err(RuntimePolicyError::NonCanonical)
        );
    }

    #[test]
    fn codec_bounds_input_output_and_cache_age() {
        let oversized = " ".repeat(MAX_RUNTIME_POLICY_BYTES + 1);
        assert_eq!(
            LocaldRuntimePolicy::parse(&oversized),
            Err(RuntimePolicyError::TooLarge)
        );
        assert_eq!(
            LocaldRuntimePolicy::new(
                true,
                true,
                true,
                true,
                Some(MAX_REGISTRY_CACHE_AGE_MILLIS + 1)
            ),
            Err(RuntimePolicyError::CacheAgeExceeded)
        );
    }
}
