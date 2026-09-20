//! Canonical product input/output encodings and semantic claim checks.

use super::{
    BuiltinInputSchema, ProductInput, ProductSourceRecord, ProductSourceRelation, ProfileIds,
    product_dependency_manifest,
};
use crate::dispatch::{
    SemanticCoverageAdmissionError, SemanticCoverageBinding, SemanticCoverageState,
    UntrustedSemanticCoverageClaim,
};
use backend_replication::WireIdentity;
use backend_semantic::DependencyManifest;
use backend_version::{ObjectVersion, Relation, StateRoot, WorkspaceRoot};

/// Fixed bytes after the profile prefix in a Product projection result.
///
/// The body contains the selected workspace root, source relation root,
/// project/file/declaration counts, and a digest over every canonical source
/// row. Keeping the result fixed-size makes remote admission independent of
/// source cardinality while still requiring the worker to traverse the
/// admitted relation.
pub const PRODUCT_OUTPUT_BODY_BYTES: usize = 32 + 32 + 8 + 8 + 8 + 32;

/// Canonical, fixed-size result of the Product source projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductProjection {
    source: StateRoot<ProductSourceRelation>,
    projects: u64,
    files: u64,
    declarations: u64,
    rows: [u8; 32],
}

impl ProductProjection {
    /// Returns the exact source relation this projection consumed.
    #[must_use]
    pub const fn source(&self) -> StateRoot<ProductSourceRelation> {
        self.source
    }

    /// Returns the number of project frontier rows observed.
    #[must_use]
    pub const fn projects(&self) -> u64 {
        self.projects
    }

    /// Returns the number of independently versioned source-file rows.
    #[must_use]
    pub const fn files(&self) -> u64 {
        self.files
    }

    /// Returns the number of declarations across all source-file rows.
    #[must_use]
    pub const fn declarations(&self) -> u64 {
        self.declarations
    }

    /// Returns the digest over canonical source rows in key order.
    #[must_use]
    pub const fn rows(&self) -> [u8; 32] {
        self.rows
    }
}

/// Streaming accumulator shared by local and remote Product execution.
///
/// Storage adapters feed already admitted rows in canonical key order. The
/// accumulator retains only counters, the prior key, and one BLAKE3 state, so
/// projection memory does not grow with the source relation.
pub struct ProductProjectionBuilder {
    prior: Option<[u8; 32]>,
    projects: u64,
    files: u64,
    declarations: u64,
    rows: blake3::Hasher,
}

impl ProductProjectionBuilder {
    /// Starts one empty, domain-separated projection.
    #[must_use]
    pub fn new() -> Self {
        let mut rows = blake3::Hasher::new();
        rows.update(b"backend.product.projection.rows.v1\0");
        Self {
            prior: None,
            projects: 0,
            files: 0,
            declarations: 0,
            rows,
        }
    }

    /// Adds one checked source row in strict canonical key order.
    /// # Errors
    ///
    /// Returns an error for duplicate/out-of-order keys or counter overflow.
    pub fn push(
        &mut self,
        key: &[u8; 32],
        value: &ProductSourceRecord,
    ) -> Result<(), &'static str> {
        if self.prior.is_some_and(|prior| prior >= *key) {
            return Err("product projection rows are not in canonical key order");
        }
        match value {
            ProductSourceRecord::Project { .. } => {
                self.projects = self
                    .projects
                    .checked_add(1)
                    .ok_or("project count overflow")?;
            }
            ProductSourceRecord::File { declarations, .. } => {
                self.files = self.files.checked_add(1).ok_or("file count overflow")?;
                self.declarations = self
                    .declarations
                    .checked_add(
                        u64::try_from(declarations.len())
                            .map_err(|_| "declaration count overflow")?,
                    )
                    .ok_or("declaration count overflow")?;
            }
            ProductSourceRecord::Package { .. }
            | ProductSourceRecord::Dependency { .. }
            | ProductSourceRecord::DependencySource { .. } => {}
        }
        let mut value_bytes = Vec::new();
        ProductSourceRelation::encode_value(value, &mut value_bytes);
        self.rows.update(&canonical_relation_row(key, &value_bytes));
        self.prior = Some(*key);
        Ok(())
    }

    /// Seals the projection against the exact relation root traversed.
    #[must_use]
    pub fn finish(self, source: StateRoot<ProductSourceRelation>) -> ProductProjection {
        ProductProjection {
            source,
            projects: self.projects,
            files: self.files,
            declarations: self.declarations,
            rows: *self.rows.finalize().as_bytes(),
        }
    }
}

impl Default for ProductProjectionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the bounded input object version for a canonical relation root.
#[must_use]
pub fn product_input_version(input: &ProductInput) -> ObjectVersion<BuiltinInputSchema> {
    let bytes = input.root().to_bytes();
    ObjectVersion::<BuiltinInputSchema>::from_value(&bytes)
}

/// Returns the wire identity for a canonical relation input object.
#[must_use]
pub fn product_input_claim(input: &ProductInput) -> WireIdentity {
    let version = product_input_version(input);
    WireIdentity::from_typed(&version)
}

/// Returns the canonical relation root bytes used in the input object frame.
#[must_use]
pub fn product_input_bytes(input: &ProductInput) -> Vec<u8> {
    input.root().to_bytes().to_vec()
}

/// Encodes one relation row as the immutable object preimage shared by page
/// producers and verifiers.
#[must_use]
pub fn canonical_relation_row(key: &[u8], value: &[u8]) -> Vec<u8> {
    let mut row = Vec::with_capacity(16 + key.len() + value.len());
    row.extend_from_slice(&(key.len() as u64).to_be_bytes());
    row.extend_from_slice(key);
    row.extend_from_slice(&(value.len() as u64).to_be_bytes());
    row.extend_from_slice(value);
    row
}

/// Encodes one echo fixture row using the canonical shared framing.
#[must_use]
pub fn echo_row_bytes(key: &u64, value: &u64) -> Vec<u8> {
    canonical_relation_row(&key.to_be_bytes(), &value.to_be_bytes())
}

/// Produces the canonical Product result bytes for an admitted input.
#[must_use]
pub fn product_output_bytes(
    profile: ProfileIds,
    input_basis: WorkspaceRoot,
    projection: ProductProjection,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(profile.output_prefix.len() + PRODUCT_OUTPUT_BODY_BYTES);
    bytes.extend_from_slice(profile.output_prefix);
    bytes.extend_from_slice(input_basis.as_bytes());
    bytes.extend_from_slice(projection.source.as_bytes());
    bytes.extend_from_slice(&projection.projects.to_be_bytes());
    bytes.extend_from_slice(&projection.files.to_be_bytes());
    bytes.extend_from_slice(&projection.declarations.to_be_bytes());
    bytes.extend_from_slice(&projection.rows);
    bytes
}

/// Validates the profile-owned fields of a semantic coverage claim.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn validate_semantic_claim(
    ids: ProfileIds,
    binding: &SemanticCoverageBinding,
    claim: &UntrustedSemanticCoverageClaim,
) -> Result<(), SemanticCoverageAdmissionError> {
    if claim.state() != SemanticCoverageState::Complete
        || claim.scope() != 1
        || claim.witness() != ids.witness
        || claim.read_manifest() != binding.read_manifest()
        || claim.authority() != binding.authority()
    {
        return Err(SemanticCoverageAdmissionError::Rejected);
    }
    Ok(())
}

/// Validates the exact reusable dependency manifest for a compiled profile.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn validate_semantic_manifest(
    ids: ProfileIds,
    binding: &SemanticCoverageBinding,
    claim: &UntrustedSemanticCoverageClaim,
    manifest: &DependencyManifest,
) -> Result<(), SemanticCoverageAdmissionError> {
    validate_semantic_claim(ids, binding, claim)?;
    if !ids.include_basis
        || binding.recipe() != ids.recipe
        || binding.read_manifest() != ids.read_manifest
        || binding.authority() != ids.authority
        || product_dependency_manifest(ids)
            .map_err(|_| SemanticCoverageAdmissionError::Rejected)?
            .canonical_bytes()
            != manifest.canonical_bytes()
    {
        return Err(SemanticCoverageAdmissionError::Rejected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_projection_is_ordered_fixed_size_and_bound_to_every_row() {
        let ids =
            super::super::profile_ids(&super::super::Profile::Product).expect("product profile");
        let relation = super::super::product_source_fixture_with_authority(true, ids.authority)
            .expect("product source");
        let basis =
            super::super::execution_input_basis(&relation, ids.authority).expect("workspace basis");
        let mut builder = ProductProjectionBuilder::new();
        for (key, value) in relation.iter() {
            builder.push(key, value).expect("canonical row");
        }
        let projection = builder.finish(relation.root());
        let output = product_output_bytes(ids, basis, projection);

        assert_eq!(projection.projects(), 2);
        assert_eq!(projection.files(), 0);
        assert_eq!(projection.declarations(), 0);
        assert_eq!(
            output.len(),
            ids.output_prefix.len() + PRODUCT_OUTPUT_BODY_BYTES
        );
        assert_eq!(
            &output[ids.output_prefix.len()..ids.output_prefix.len() + 32],
            basis.as_bytes()
        );
        assert_eq!(
            &output[ids.output_prefix.len() + 32..ids.output_prefix.len() + 64],
            relation.root().as_bytes()
        );

        let (first_key, first_value) = relation.iter().next().expect("first row");
        let mut invalid = ProductProjectionBuilder::new();
        invalid.push(first_key, first_value).expect("first row");
        assert_eq!(
            invalid.push(first_key, first_value),
            Err("product projection rows are not in canonical key order")
        );
    }
}
