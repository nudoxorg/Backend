//! Canonical product input/output encodings and semantic claim checks.

use super::{
    BuiltinInputSchema, ProductInput, ProductSemanticPublicationKey,
    ProductSemanticPublicationRecord, ProductSemanticPublicationRelation, ProductSourceRecord,
    ProductSourceRelation, ProfileIds, SemanticPublicationCoverage, SemanticPublicationInput,
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

/// Fixed bytes after the profile prefix in a semantic publication result.
///
/// The body commits the selected workspace and semantic relation roots, typed
/// publication terminal counts, total immutable semantic artifacts and bytes,
/// and a digest over every canonical PURL/profile/publication row.
pub const SEMANTIC_OUTPUT_BODY_BYTES: usize = 32 + 32 + 8 + 8 + 8 + 8 + 8 + 32;

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

/// Fixed-size result of traversing the canonical compiler publication plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticPublicationProjection {
    publications: StateRoot<ProductSemanticPublicationRelation>,
    packages: u64,
    published: u64,
    unavailable: u64,
    artifacts: u64,
    semantic_bytes: u64,
    rows: [u8; 32],
}

impl SemanticPublicationProjection {
    /// Returns the exact semantic relation consumed by execution.
    #[must_use]
    pub const fn publications(&self) -> StateRoot<ProductSemanticPublicationRelation> {
        self.publications
    }

    /// Returns the number of distinct package scopes observed.
    #[must_use]
    pub const fn packages(&self) -> u64 {
        self.packages
    }

    /// Returns the number of immutable compiler generations observed.
    #[must_use]
    pub const fn published(&self) -> u64 {
        self.published
    }

    /// Returns the number of explicit semantic-unavailable terminals.
    #[must_use]
    pub const fn unavailable(&self) -> u64 {
        self.unavailable
    }

    /// Returns the number of semantic artifacts committed by all generations.
    #[must_use]
    pub const fn artifacts(&self) -> u64 {
        self.artifacts
    }

    /// Returns the total semantic artifact bytes committed by all generations.
    #[must_use]
    pub const fn semantic_bytes(&self) -> u64 {
        self.semantic_bytes
    }

    /// Returns the digest over canonical typed semantic rows in key order.
    #[must_use]
    pub const fn rows(&self) -> [u8; 32] {
        self.rows
    }
}

/// Bounded streaming accumulator for compiler publication rows.
///
/// It retains only one prior typed key, one package identity, fixed counters,
/// and a BLAKE3 state. Both locald and the worker feed it already-admitted
/// relation entries and therefore produce byte-identical results without a
/// generic view or source-declaration projection.
pub struct SemanticPublicationProjectionBuilder {
    prior: Option<ProductSemanticPublicationKey>,
    prior_package: Option<[u8; 32]>,
    packages: u64,
    published: u64,
    unavailable: u64,
    artifacts: u64,
    semantic_bytes: u64,
    rows: blake3::Hasher,
}

impl SemanticPublicationProjectionBuilder {
    /// Starts an empty domain-separated semantic accumulator.
    #[must_use]
    pub fn new() -> Self {
        let mut rows = blake3::Hasher::new();
        rows.update(b"backend.semantic.publication.projection.rows.v1\0");
        Self {
            prior: None,
            prior_package: None,
            packages: 0,
            published: 0,
            unavailable: 0,
            artifacts: 0,
            semantic_bytes: 0,
            rows,
        }
    }

    /// Adds one checked PURL/profile/publication row in canonical key order.
    ///
    /// # Errors
    /// Returns an error for duplicate or out-of-order keys and counter overflow.
    pub fn push(
        &mut self,
        key: &ProductSemanticPublicationKey,
        value: &ProductSemanticPublicationRecord,
    ) -> Result<(), &'static str> {
        key.admit_record(value)
            .map_err(|_| "semantic publication selection does not match its record")?;
        let mut key_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_key(key, &mut key_bytes);
        if self.prior.as_ref().is_some_and(|prior| prior >= key) {
            return Err("semantic publication rows are not in canonical key order");
        }
        let package = key.package_key().to_bytes();
        if self.prior_package != Some(package) {
            self.packages = self
                .packages
                .checked_add(1)
                .ok_or("semantic package count overflow")?;
            self.prior_package = Some(package);
        }
        match value {
            ProductSemanticPublicationRecord::Published { coverage, claim } => {
                // Pattern matching coverage here ensures a future coverage
                // variant cannot silently enter the canonical result grammar.
                match coverage {
                    SemanticPublicationCoverage::Complete
                    | SemanticPublicationCoverage::Partial(_) => {}
                }
                self.published = self
                    .published
                    .checked_add(1)
                    .ok_or("semantic publication count overflow")?;
                self.artifacts = self
                    .artifacts
                    .checked_add(u64::from(claim.manifest().fragment_count))
                    .ok_or("semantic artifact count overflow")?;
                self.semantic_bytes = self
                    .semantic_bytes
                    .checked_add(u64::from(claim.manifest().byte_length))
                    .ok_or("semantic byte count overflow")?;
            }
            ProductSemanticPublicationRecord::Unavailable(_) => {
                self.unavailable = self
                    .unavailable
                    .checked_add(1)
                    .ok_or("semantic unavailable count overflow")?;
            }
        }
        let mut value_bytes = Vec::new();
        ProductSemanticPublicationRelation::encode_value(value, &mut value_bytes);
        self.rows
            .update(&canonical_relation_row(&key_bytes, &value_bytes));
        self.prior = Some(key.clone());
        Ok(())
    }

    /// Seals the summary against the exact semantic publication root traversed.
    #[must_use]
    pub fn finish(
        self,
        publications: StateRoot<ProductSemanticPublicationRelation>,
    ) -> SemanticPublicationProjection {
        SemanticPublicationProjection {
            publications,
            packages: self.packages,
            published: self.published,
            unavailable: self.unavailable,
            artifacts: self.artifacts,
            semantic_bytes: self.semantic_bytes,
            rows: *self.rows.finalize().as_bytes(),
        }
    }
}

impl Default for SemanticPublicationProjectionBuilder {
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

/// Returns the bounded input object version for a semantic relation root.
#[must_use]
pub fn semantic_input_version(
    input: SemanticPublicationInput,
) -> ObjectVersion<BuiltinInputSchema> {
    ObjectVersion::<BuiltinInputSchema>::from_value(input.root().as_bytes())
}

/// Returns the wire identity for one semantic publication input object.
#[must_use]
pub fn semantic_input_claim(input: SemanticPublicationInput) -> WireIdentity {
    WireIdentity::from_typed(&semantic_input_version(input))
}

/// Returns the canonical semantic relation root bytes carried by the input.
#[must_use]
pub fn semantic_input_bytes(input: SemanticPublicationInput) -> Vec<u8> {
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

/// Produces the canonical result bytes for an admitted semantic publication
/// input. The output is identical for local and remote traversal of the same
/// checked relation root.
#[must_use]
pub fn semantic_publication_output_bytes(
    profile: ProfileIds,
    input_basis: WorkspaceRoot,
    projection: SemanticPublicationProjection,
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(profile.output_prefix.len() + SEMANTIC_OUTPUT_BODY_BYTES);
    bytes.extend_from_slice(profile.output_prefix);
    bytes.extend_from_slice(input_basis.as_bytes());
    bytes.extend_from_slice(projection.publications.as_bytes());
    bytes.extend_from_slice(&projection.packages.to_be_bytes());
    bytes.extend_from_slice(&projection.published.to_be_bytes());
    bytes.extend_from_slice(&projection.unavailable.to_be_bytes());
    bytes.extend_from_slice(&projection.artifacts.to_be_bytes());
    bytes.extend_from_slice(&projection.semantic_bytes.to_be_bytes());
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

    #[test]
    fn semantic_publication_projection_is_deterministic_and_root_bound() {
        let ids =
            super::super::profile_ids(&super::super::Profile::Product).expect("product profile");
        let relation =
            super::super::semantic_publication_fixture_with_authority(true, ids.authority)
                .expect("semantic publication relation");
        let basis = super::super::semantic_execution_input_basis(&relation, ids.authority)
            .expect("semantic workspace basis");

        let mut local = SemanticPublicationProjectionBuilder::new();
        for (key, value) in relation.iter() {
            local.push(key, value).expect("canonical semantic row");
        }
        let local_projection = local.finish(relation.root());
        let local_output = semantic_publication_output_bytes(ids, basis, local_projection);

        let mut remote = SemanticPublicationProjectionBuilder::new();
        for (key, value) in relation.iter() {
            remote.push(key, value).expect("canonical semantic row");
        }
        let remote_projection = remote.finish(relation.root());
        let remote_output = semantic_publication_output_bytes(ids, basis, remote_projection);

        assert_eq!(local_output, remote_output);
        assert_eq!(local_projection.publications(), relation.root());
        assert_eq!(local_projection.packages(), 2);
        assert_eq!(local_projection.published(), 0);
        assert_eq!(local_projection.unavailable(), 2);
        assert_eq!(local_projection.artifacts(), 0);
        assert_eq!(local_projection.semantic_bytes(), 0);
        assert_eq!(
            local_output.len(),
            ids.output_prefix.len() + SEMANTIC_OUTPUT_BODY_BYTES
        );
        assert_eq!(
            &local_output[ids.output_prefix.len()..ids.output_prefix.len() + 32],
            basis.as_bytes()
        );
        assert_eq!(
            &local_output[ids.output_prefix.len() + 32..ids.output_prefix.len() + 64],
            relation.root().as_bytes()
        );
    }

    #[test]
    fn semantic_publication_projection_rejects_duplicate_or_reordered_keys() {
        let ids =
            super::super::profile_ids(&super::super::Profile::Product).expect("product profile");
        let relation =
            super::super::semantic_publication_fixture_with_authority(true, ids.authority)
                .expect("semantic publication relation");
        let mut rows = relation.iter();
        let (first_key, first_value) = rows.next().expect("first semantic row");
        let (second_key, second_value) = rows.next().expect("second semantic row");
        let mut builder = SemanticPublicationProjectionBuilder::new();
        builder.push(second_key, second_value).expect("first row");
        assert_eq!(
            builder.push(second_key, second_value),
            Err("semantic publication rows are not in canonical key order")
        );
        assert_eq!(
            builder.push(first_key, first_value),
            Err("semantic publication rows are not in canonical key order")
        );
    }
}
