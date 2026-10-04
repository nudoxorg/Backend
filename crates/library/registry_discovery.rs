//! Exact source-native facts carried by unacquired registry release claims.
//!
//! Discovery facts remain separate from acquired package metadata: the source
//! proof and freshness belong to the enclosing discovery candidate, while the
//! fields here preserve the exact version-specific records the registry sent.

use crate::{ProductAdmissionError, RegistryEvidenceFacet, SourceAtomText};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Maximum number of records admitted in one discovery metadata collection.
pub const MAX_REGISTRY_DISCOVERY_FACT_ROWS: usize = 4_096;
/// Maximum total nested Cargo feature/dependency records admitted per release.
pub const MAX_REGISTRY_DISCOVERY_CARGO_ITEMS: usize = 65_536;
/// Maximum retained source metadata bytes for one discovered release.
pub const MAX_REGISTRY_DISCOVERY_METADATA_BYTES: usize = 16 * 1024 * 1024;

/// One Cargo feature declaration from an exact sparse-index release row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryCargoFeature {
    /// Exact registry feature key.
    pub name: SourceAtomText,
    /// Exact expressions declared by the registry for this feature.
    pub members: Box<[SourceAtomText]>,
}

/// One dependency declaration from an exact Cargo sparse-index release row.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryCargoDependency {
    /// Exact registry dependency name.
    pub name: SourceAtomText,
    /// Exact registry version requirement.
    pub requirement: SourceAtomText,
    /// Renamed package, when the registry row publishes one.
    pub package: RegistryEvidenceFacet<SourceAtomText>,
    /// Requested dependency features, preserving unknown and absent states.
    pub features: RegistryEvidenceFacet<Box<[SourceAtomText]>>,
    /// Whether this declaration is optional.
    pub optional: RegistryEvidenceFacet<bool>,
    /// Whether Cargo's default features are enabled.
    pub default_features: RegistryEvidenceFacet<bool>,
    /// Target predicate supplied by the registry.
    pub target: RegistryEvidenceFacet<SourceAtomText>,
    /// Dependency kind supplied by the registry.
    pub kind: RegistryEvidenceFacet<SourceAtomText>,
    /// Alternate registry URL supplied by the registry.
    pub registry: RegistryEvidenceFacet<SourceAtomText>,
}

impl Ord for RegistryDiscoveryCargoDependency {
    fn cmp(&self, other: &Self) -> Ordering {
        self.name
            .cmp(&other.name)
            .then_with(|| self.requirement.cmp(&other.requirement))
            .then_with(|| facet_cmp(&self.package, &other.package))
            .then_with(|| facet_cmp(&self.features, &other.features))
            .then_with(|| facet_cmp(&self.optional, &other.optional))
            .then_with(|| facet_cmp(&self.default_features, &other.default_features))
            .then_with(|| facet_cmp(&self.target, &other.target))
            .then_with(|| facet_cmp(&self.kind, &other.kind))
            .then_with(|| facet_cmp(&self.registry, &other.registry))
    }
}

impl PartialOrd for RegistryDiscoveryCargoDependency {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn facet_cmp<T: Ord>(
    left: &RegistryEvidenceFacet<T>,
    right: &RegistryEvidenceFacet<T>,
) -> Ordering {
    match (left, right) {
        (RegistryEvidenceFacet::Known(left), RegistryEvidenceFacet::Known(right)) => {
            left.cmp(right)
        }
        (RegistryEvidenceFacet::Known(_), _) => Ordering::Less,
        (_, RegistryEvidenceFacet::Known(_)) => Ordering::Greater,
        (RegistryEvidenceFacet::Absent, RegistryEvidenceFacet::Absent)
        | (RegistryEvidenceFacet::Unknown, RegistryEvidenceFacet::Unknown) => Ordering::Equal,
        (RegistryEvidenceFacet::Absent, RegistryEvidenceFacet::Unknown) => Ordering::Less,
        (RegistryEvidenceFacet::Unknown, RegistryEvidenceFacet::Absent) => Ordering::Greater,
    }
}

/// Cargo sparse-index facts bound to the enclosing exact release coordinate.
///
/// `SourceAtomText` ensures only the shared atom byte cap at decode time.
/// Call [`Self::admit`] after decoding to enforce this record's narrower
/// source bounds and aggregate retained-size limit before using or cloning it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistryDiscoveryCargoMetadata {
    /// Exact 64-character archive checksum from the sparse-index record.
    pub checksum: SourceAtomText,
    /// Sparse-index schema version recorded by the registry.
    pub schema_version: u32,
    /// Declared Rust version, when this record publishes one.
    pub rust_version: RegistryEvidenceFacet<SourceAtomText>,
    /// Native linking name, when this record publishes one.
    pub links: RegistryEvidenceFacet<SourceAtomText>,
    /// `features` map from the exact sparse-index record.
    pub features: RegistryEvidenceFacet<Box<[RegistryDiscoveryCargoFeature]>>,
    /// `features2` map from the exact sparse-index record.
    pub features2: RegistryEvidenceFacet<Box<[RegistryDiscoveryCargoFeature]>>,
    /// Dependency declarations from the exact sparse-index record.
    pub dependencies: RegistryEvidenceFacet<Box<[RegistryDiscoveryCargoDependency]>>,
}

impl RegistryDiscoveryCargoMetadata {
    /// Checks source-specific text bounds, nested cardinality, and canonical order.
    ///
    /// This admission is required after serde decoding: individual
    /// `SourceAtomText` values enforce only their shared atom cap.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        let mut budget = RetainedBudget::default();
        budget.text(self.checksum.as_str())?;
        budget.rows(1, std::mem::size_of::<Self>())?;
        if self.checksum.as_str().len() != 64
            || !self
                .checksum
                .as_str()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !matches!(self.schema_version, 1 | 2)
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        admit_text_facet(&self.rust_version, 128, &mut budget)?;
        admit_text_facet(&self.links, 256, &mut budget)?;
        admit_features(&self.features, &mut budget)?;
        admit_features(&self.features2, &mut budget)?;
        admit_dependencies(&self.dependencies, &mut budget)?;
        Ok(())
    }
}

#[derive(Default)]
struct RetainedBudget {
    rows: usize,
    bytes: usize,
}

impl RetainedBudget {
    fn text(&mut self, text: &str) -> Result<(), ProductAdmissionError> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or(ProductAdmissionError::RegistryDiscoveryMetadata)?;
        self.bound()
    }

    fn rows(&mut self, count: usize, row_bytes: usize) -> Result<(), ProductAdmissionError> {
        self.rows = self
            .rows
            .checked_add(count)
            .ok_or(ProductAdmissionError::RegistryDiscoveryMetadata)?;
        self.bytes = self
            .bytes
            .checked_add(
                count
                    .checked_mul(row_bytes)
                    .ok_or(ProductAdmissionError::RegistryDiscoveryMetadata)?,
            )
            .ok_or(ProductAdmissionError::RegistryDiscoveryMetadata)?;
        self.bound()
    }

    fn bound(&self) -> Result<(), ProductAdmissionError> {
        if self.rows > MAX_REGISTRY_DISCOVERY_CARGO_ITEMS
            || self.bytes > MAX_REGISTRY_DISCOVERY_METADATA_BYTES
        {
            Err(ProductAdmissionError::RegistryDiscoveryMetadata)
        } else {
            Ok(())
        }
    }
}

fn admit_text_facet(
    value: &RegistryEvidenceFacet<SourceAtomText>,
    maximum_bytes: usize,
    budget: &mut RetainedBudget,
) -> Result<(), ProductAdmissionError> {
    if let RegistryEvidenceFacet::Known(text) = value {
        if text.as_str().is_empty()
            || text.as_str().len() > maximum_bytes
            || text.as_str().contains('\0')
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        budget.text(text.as_str())?;
    }
    Ok(())
}

fn admit_features(
    value: &RegistryEvidenceFacet<Box<[RegistryDiscoveryCargoFeature]>>,
    budget: &mut RetainedBudget,
) -> Result<(), ProductAdmissionError> {
    let RegistryEvidenceFacet::Known(features) = value else {
        return Ok(());
    };
    if features.len() > MAX_REGISTRY_DISCOVERY_FACT_ROWS {
        return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
    }
    budget.rows(
        features.len(),
        std::mem::size_of::<RegistryDiscoveryCargoFeature>(),
    )?;
    let mut previous_name: Option<&str> = None;
    for feature in features.iter() {
        let name = feature.name.as_str();
        if name.is_empty()
            || name.len() > 256
            || name.contains('\0')
            || previous_name.is_some_and(|previous| previous >= name)
            || feature.members.len() > MAX_REGISTRY_DISCOVERY_FACT_ROWS
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        previous_name = Some(name);
        budget.text(name)?;
        budget.rows(feature.members.len(), std::mem::size_of::<SourceAtomText>())?;
        let mut previous_member: Option<&str> = None;
        for member in feature.members.iter() {
            let member = member.as_str();
            if member.is_empty()
                || member.len() > 1024
                || member.contains('\0')
                || previous_member.is_some_and(|previous| previous > member)
            {
                return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
            }
            previous_member = Some(member);
            budget.text(member)?;
        }
    }
    Ok(())
}

fn admit_dependencies(
    value: &RegistryEvidenceFacet<Box<[RegistryDiscoveryCargoDependency]>>,
    budget: &mut RetainedBudget,
) -> Result<(), ProductAdmissionError> {
    let RegistryEvidenceFacet::Known(dependencies) = value else {
        return Ok(());
    };
    if dependencies.len() > MAX_REGISTRY_DISCOVERY_FACT_ROWS {
        return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
    }
    budget.rows(
        dependencies.len(),
        std::mem::size_of::<RegistryDiscoveryCargoDependency>(),
    )?;
    let mut previous: Option<&RegistryDiscoveryCargoDependency> = None;
    for dependency in dependencies.iter() {
        if dependency.name.as_str().is_empty()
            || dependency.name.as_str().len() > 256
            || dependency.name.as_str().contains('\0')
            || dependency.requirement.as_str().is_empty()
            || dependency.requirement.as_str().len() > 1024
            || dependency.requirement.as_str().contains('\0')
            || previous.is_some_and(|previous| previous > dependency)
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        previous = Some(dependency);
        budget.text(dependency.name.as_str())?;
        budget.text(dependency.requirement.as_str())?;
        admit_text_facet(&dependency.package, 256, budget)?;
        admit_text_slice_facet(&dependency.features, 256, 1024, budget)?;
        admit_text_facet(&dependency.target, 1024, budget)?;
        admit_text_facet(&dependency.kind, 32, budget)?;
        admit_text_facet(&dependency.registry, 2048, budget)?;
    }
    Ok(())
}

fn admit_text_slice_facet(
    value: &RegistryEvidenceFacet<Box<[SourceAtomText]>>,
    maximum_items: usize,
    maximum_bytes: usize,
    budget: &mut RetainedBudget,
) -> Result<(), ProductAdmissionError> {
    let RegistryEvidenceFacet::Known(values) = value else {
        return Ok(());
    };
    if values.len() > maximum_items {
        return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
    }
    budget.rows(values.len(), std::mem::size_of::<SourceAtomText>())?;
    let mut previous: Option<&str> = None;
    for value in values.iter() {
        let text = value.as_str();
        if text.is_empty()
            || text.len() > maximum_bytes
            || text.contains('\0')
            || previous.is_some_and(|previous| previous > text)
        {
            return Err(ProductAdmissionError::RegistryDiscoveryMetadata);
        }
        previous = Some(text);
        budget.text(text)?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::{RegistryDiscoveryCargoFeature, RegistryDiscoveryCargoMetadata};
    use crate::{RegistryDiscoveryMetadata, RegistryEvidenceFacet, SourceAtomText};

    fn atom(value: &str) -> SourceAtomText {
        SourceAtomText::new(value).expect("bounded source atom")
    }

    fn cargo() -> RegistryDiscoveryCargoMetadata {
        RegistryDiscoveryCargoMetadata {
            checksum: atom(&"0".repeat(64)),
            schema_version: 2,
            rust_version: RegistryEvidenceFacet::Unknown,
            links: RegistryEvidenceFacet::Absent,
            features: RegistryEvidenceFacet::Known(Box::new([])),
            features2: RegistryEvidenceFacet::Unknown,
            dependencies: RegistryEvidenceFacet::Absent,
        }
    }

    #[test]
    fn known_empty_absent_and_unknown_sparse_facets_remain_distinct() {
        let value = cargo();
        value
            .admit()
            .expect("known empty source features are valid");
        let encoded = serde_json::to_vec(&value).expect("Cargo metadata serializes");
        let decoded: RegistryDiscoveryCargoMetadata =
            serde_json::from_slice(&encoded).expect("Cargo metadata deserializes");
        assert_eq!(decoded.features, RegistryEvidenceFacet::Known(Box::new([])));
        assert_eq!(decoded.features2, RegistryEvidenceFacet::Unknown);
        assert_eq!(decoded.dependencies, RegistryEvidenceFacet::Absent);
    }

    #[test]
    fn checksum_admission_preserves_source_accepted_hex_spelling() {
        let mut value = cargo();
        value.checksum = atom(&"A".repeat(64));
        value
            .admit()
            .expect("engine admission accepts exact uppercase hexadecimal source text");
    }

    #[test]
    fn new_discovery_metadata_fields_are_required_on_the_public_contract() {
        let metadata = RegistryDiscoveryMetadata::default();
        let mut encoded = serde_json::to_value(metadata).expect("metadata serializes");
        let object = encoded.as_object_mut().expect("metadata is an object");
        object.remove("deprecation");
        object.remove("cargo_sparse");
        assert!(serde_json::from_value::<RegistryDiscoveryMetadata>(encoded).is_err());
    }

    #[test]
    fn cargo_sparse_nested_expressions_have_an_aggregate_admission_fence() {
        let features = (0..17)
            .map(|index| RegistryDiscoveryCargoFeature {
                name: atom(&format!("feature-{index:02}")),
                members: vec![atom("x"); 4096].into_boxed_slice(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut value = cargo();
        value.features = RegistryEvidenceFacet::Known(features);
        assert!(value.admit().is_err());
    }

    #[test]
    fn cargo_sparse_features_require_canonical_order() {
        let mut value = cargo();
        value.features = RegistryEvidenceFacet::Known(
            vec![
                RegistryDiscoveryCargoFeature {
                    name: atom("z-feature"),
                    members: Box::new([]),
                },
                RegistryDiscoveryCargoFeature {
                    name: atom("a-feature"),
                    members: Box::new([]),
                },
            ]
            .into_boxed_slice(),
        );
        assert!(value.admit().is_err());
    }
}
