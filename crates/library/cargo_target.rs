//! Explicit selection and retained scope of one Cargo target.

use serde::{Deserialize, Serialize};

/// A caller-selected Cargo target name, resolved against admitted Cargo facts.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CargoTargetSelector(String);

impl CargoTargetSelector {
    /// Admits a bounded target name without interpreting it as a path.
    pub fn new(name: String) -> Result<Self, &'static str> {
        if name.is_empty()
            || name.len() > 256
            || name
                .chars()
                .any(|ch| ch.is_control() || ch == '/' || ch == '\\')
        {
            return Err("Cargo target name is empty, too long, or contains a path separator");
        }
        Ok(Self(name))
    }

    /// Exact target name supplied by the caller.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CargoTargetSelector {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CargoTargetSelector> for String {
    fn from(value: CargoTargetSelector) -> Self {
        value.0
    }
}

/// Exact selected Cargo context and the compiler-proven semantic source closure.
/// Package inputs outside this closure remain captured provenance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoTargetScope {
    /// Exact admitted target name.
    pub name: CargoTargetSelector,
    /// Package-relative Cargo source root.
    pub root: String,
    /// Exact sorted Cargo target kinds.
    pub kinds: Box<[String]>,
    /// Domain-separated digest of target identity and sorted exact source rows.
    pub source_closure: [u8; 32],
    /// Number of source rows in this selected target closure.
    pub source_count: u64,
}

impl CargoTargetScope {
    /// Maximum canonical JSON size retained in a capture or operation receipt.
    pub const MAX_ENCODED_BYTES: usize = 8192;

    /// Checks the bounded retained context before storage or projection.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.root.is_empty()
            || self.root.len() > 4096
            || self.root.starts_with('/')
            || self
                .root
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || self.root.contains('\\')
            || self.root.chars().any(char::is_control)
            || self.kinds.is_empty()
            || self.kinds.len() > 16
            || self.kinds.iter().any(|kind| {
                kind.is_empty() || kind.len() > 64 || kind.chars().any(char::is_control)
            })
            || self.kinds.windows(2).any(|pair| pair[0] >= pair[1])
            || self.source_closure == [0; 32]
            || self.source_count == 0
            || self.source_count > 100_000
        {
            return Err("selected Cargo target scope is malformed");
        }
        if serde_json::to_vec(self)
            .map_err(|_| "selected Cargo target scope cannot be encoded")?
            .len()
            > Self::MAX_ENCODED_BYTES
        {
            return Err("selected Cargo target scope exceeds its encoding bound");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_request_digest_preserves_legacy_and_distinguishes_targets() {
        let package = crate::PackageReference::parse("/cargo-target-test").expect("package");
        let intent = crate::CompileExecutionIntent::Interactive;
        let library = CargoTargetSelector::new("library".to_owned()).expect("target");
        let binary = CargoTargetSelector::new("binary".to_owned()).expect("target");
        let digest =
            |target| crate::index_operation_scoped_request_digest(&package, intent, target);
        assert_eq!(
            digest(None),
            crate::index_operation_request_digest(&package, intent)
        );
        assert_ne!(digest(None), digest(Some(&library)));
        assert_ne!(digest(Some(&library)), digest(Some(&binary)));
        for invalid in ["", "../target", "target\\name", "target\nname"] {
            assert!(CargoTargetSelector::new(invalid.to_owned()).is_err());
        }
        assert!(serde_json::from_str::<CargoTargetSelector>("\"../target\"").is_err());
    }
}
