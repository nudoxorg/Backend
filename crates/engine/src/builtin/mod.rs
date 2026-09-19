//! Shared compiled builtin profile grammar.
//!
//! The local daemon and pure worker are separate process crates, but they must
//! agree on the bytes that identify a recipe, its semantic reads, its input
//! object, and its result.  The facade below keeps those values owned by the
//! engine while each capability boundary has a focused implementation module.

mod authority;
mod manifest;
mod output;
mod profile;
mod relation;
mod semantic_relation;

pub use authority::WorkspaceViewProducerAdmission;
pub use authority::coverage_from_admitted_authority;
pub(crate) use authority::{authorized_coverage, complete_coverage};
pub use manifest::{
    ProductClosureManifestClaim, admit_product_closure_manifest, execution_input_basis,
    execution_input_basis_from_source, execution_input_manifest,
    execution_input_manifest_from_source, product_closure_manifest, product_closure_root,
    product_dependency_manifest, product_read_manifest, semantic_execution_input_basis,
    semantic_execution_input_basis_from_snapshot, semantic_execution_input_manifest,
    semantic_execution_input_manifest_from_snapshot,
};
pub use output::{
    PRODUCT_OUTPUT_BODY_BYTES, ProductProjection, ProductProjectionBuilder,
    SEMANTIC_OUTPUT_BODY_BYTES, SemanticPublicationProjection,
    SemanticPublicationProjectionBuilder, canonical_relation_row, echo_row_bytes,
    product_input_bytes, product_input_claim, product_input_version, product_output_bytes,
    semantic_input_bytes, semantic_input_claim, semantic_input_version,
    semantic_publication_output_bytes, validate_semantic_claim, validate_semantic_manifest,
};
pub use profile::{
    ECHO_AUTHORITY_BYTES, ECHO_EQUIVALENCE_BYTES, ECHO_OUTPUT_BYTES, ECHO_READ_BYTES,
    ECHO_RECIPE_BYTES, ECHO_WITNESS_BYTES, PRODUCT_AUTHORITY_BYTES, PRODUCT_EQUIVALENCE_BYTES,
    PRODUCT_EXECUTION_MEMORY_BYTES, PRODUCT_EXECUTION_NODE_BUDGET, PRODUCT_OUTPUT_BYTES,
    PRODUCT_RECIPE_BYTES, PRODUCT_WITNESS_BYTES, Profile, ProfileDescriptor, ProfileIds,
    execution_manifest, execution_resources, profile_descriptor, profile_ids, profile_output_len,
};
pub use relation::{
    BuiltinInputSchema, Container, DeclarationKind, DeclarationRetention, ProductFileRef,
    ProductInput, ProductProjectRef, ProductSourceDeltaFacts, ProductSourceRecord,
    ProductSourceRelation, ProductSourceRetentionFacts, ProductSourceSnapshot,
    ProductSourceTransition, RetainedDeclarations, SourceDeclaration, SourceLanguage,
    SourceLocation, SourceUnavailableReason, product_source_file_key,
};
pub use semantic_relation::{
    ActivatedSemanticPublication, PartialSemanticCoverage, ProductSemanticPublicationKey,
    ProductSemanticPublicationRecord, ProductSemanticPublicationRelation,
    ProductSemanticPublicationSnapshot, SemanticActivationError, SemanticPublicationClaim,
    SemanticPublicationCoverage, SemanticPublicationInput, SemanticPublicationRetentionFacts,
    SemanticPublicationSelection, SemanticPublicationSelectionError,
    SemanticPublicationTargetError, SemanticUnavailableReason,
};

/// Builds a checked Product-shaped source fixture for compatibility adapters.
///
/// The product process path uses [`ProductSourceSnapshot`] from a checked
/// workspace head; this helper remains for focused fixtures and tests.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn product_source_fixture(
    expanded: bool,
) -> Result<backend_version::RelationState<ProductSourceRelation>, String> {
    relation::product_source_fixture(expanded)
}

/// Builds a Product-shaped source fixture with complete authority coverage.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn product_source_fixture_with_authority(
    expanded: bool,
    authority: backend_execution::AuthorityVersion,
) -> Result<backend_version::RelationState<ProductSourceRelation>, String> {
    relation::product_source_fixture_with_authority(expanded, authority)
}

/// Builds an authority-covered semantic publication relation for focused
/// protocol fixtures. Production code opens the owner-selected relation.
///
/// # Errors
/// Returns an error if the fixture cannot be encoded under the selected
/// authority.
pub fn semantic_publication_fixture_with_authority(
    expanded: bool,
    authority: backend_execution::AuthorityVersion,
) -> Result<backend_version::RelationState<ProductSemanticPublicationRelation>, String> {
    semantic_relation::semantic_publication_fixture_with_authority(expanded, authority)
}
