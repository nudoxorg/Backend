//! Canonical manifests, closure proofs, and product input bases.

use super::{
    BuiltinInputSchema, PRODUCT_RECIPE_BYTES, ProductSemanticPublicationRelation,
    ProductSemanticPublicationSnapshot, ProductSourceRelation, ProductSourceSnapshot, ProfileIds,
    authorized_coverage,
};
use backend_execution::AuthorityVersion;
use backend_replication::WireIdentity;
use backend_semantic::{
    DependencyFact, DependencyManifest, FacetKind, FacetValue, FacetValueSchema,
    Read as SemanticRead, ReadDependencyFact, ReadManifest as SemanticReadManifest,
    Recipe as SemanticRecipe, ScopedRead,
};
use backend_version::{
    Coverage, CoverageWitness, ObjectVersion, RelationBinding, RelationState, ScopeRoot,
    WorkspaceManifest, WorkspaceRoot,
};

/// Returns the canonical compact read-manifest bytes for the product.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn product_read_manifest(authority: AuthorityVersion) -> Result<Vec<u8>, String> {
    let scope = ScopeRoot::from_bytes(authority.to_bytes());
    SemanticReadManifest::new(vec![
        SemanticRead::exact(FacetKind::Name, scope),
        SemanticRead::negative(FacetKind::Type, scope),
        SemanticRead::range(FacetKind::Occurrence, scope, 1),
        SemanticRead::negative_range(FacetKind::Component, scope, 2),
    ])
    .map(|manifest| manifest.canonical_bytes())
    .map_err(|error| error.to_string())
}

/// Builds the exact positive, negative, and range dependencies retained with
/// reusable product output.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn product_dependency_manifest(ids: ProfileIds) -> Result<DependencyManifest, String> {
    if !ids.include_basis {
        return Err("dependency manifest is only defined for the product profile".to_owned());
    }
    let coverage = authorized_coverage(ids.authority)?;
    let scope = ScopeRoot::from_bytes(ids.authority.to_bytes());
    let compact = SemanticReadManifest::new(vec![
        SemanticRead::exact(FacetKind::Name, scope),
        SemanticRead::negative(FacetKind::Type, scope),
        SemanticRead::range(FacetKind::Occurrence, scope, 1),
        SemanticRead::negative_range(FacetKind::Component, scope, 2),
    ])
    .map_err(|error| error.to_string())?;
    let recipe = SemanticRecipe::Names
        .typed(1, PRODUCT_RECIPE_BYTES.to_vec(), compact)
        .value_version();
    let name_value = FacetValue::new(FacetKind::Name, b"backend-builtin".to_vec());
    let name_version = ObjectVersion::<FacetValueSchema>::from_value(&name_value);
    let occurrence_value = FacetValue::new(FacetKind::Occurrence, b"builtin-occurrence".to_vec());
    let occurrence_version = ObjectVersion::<FacetValueSchema>::from_value(&occurrence_value);
    let facts = vec![
        DependencyFact::read(
            ReadDependencyFact::positive(
                recipe,
                ScopedRead::exact(FacetKind::Name, scope, b"backend-builtin".to_vec()),
                name_version,
                coverage,
            )
            .map_err(|error| error.to_string())?,
        ),
        DependencyFact::read(
            ReadDependencyFact::negative(
                recipe,
                ScopedRead::negative(FacetKind::Type, scope, b"builtin-missing-type".to_vec()),
                coverage,
            )
            .map_err(|error| error.to_string())?,
        ),
        DependencyFact::read(
            ReadDependencyFact::range(
                recipe,
                FacetKind::Occurrence,
                scope,
                b"builtin-occurrence".to_vec(),
                b"builtin-occurrence~".to_vec(),
                occurrence_version,
                coverage,
            )
            .map_err(|error| error.to_string())?,
        ),
        DependencyFact::read(
            ReadDependencyFact::negative_range(
                recipe,
                FacetKind::Component,
                scope,
                b"builtin-missing-component".to_vec(),
                b"builtin-missing-component~".to_vec(),
                coverage,
            )
            .map_err(|error| error.to_string())?,
        ),
    ];
    DependencyManifest::new(facts).map_err(|error| error.to_string())
}

/// Canonical manifest preimage for a product closure root.
#[must_use]
pub fn product_closure_manifest(
    workspace: WorkspaceRoot,
    relation: [u8; 32],
    input: [u8; 32],
) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(PRODUCT_CLOSURE_MANIFEST_MAGIC.len() + 96);
    bytes.extend_from_slice(PRODUCT_CLOSURE_MANIFEST_MAGIC);
    bytes.extend_from_slice(workspace.as_bytes());
    bytes.extend_from_slice(&relation);
    bytes.extend_from_slice(&input);
    bytes
}

const PRODUCT_CLOSURE_MANIFEST_MAGIC: &[u8] = b"backend.product.closure.manifest.v1\0";

/// The authenticated children carried by a product closure manifest proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductClosureManifestClaim {
    /// Workspace root claimed by the manifest.
    pub workspace: [u8; 32],
    /// Canonical relation node committed by the manifest.
    pub relation: [u8; 32],
    /// Exact immutable input object committed by the manifest.
    pub input: [u8; 32],
}

/// Admits the canonical product closure manifest grammar against its
/// immutable object-version claim.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn admit_product_closure_manifest(
    claim: WireIdentity,
    proof: &[u8],
) -> Result<ProductClosureManifestClaim, String> {
    ObjectVersion::<BuiltinInputSchema>::admit_value(
        claim.into_untrusted().map_err(|error| error.to_string())?,
        proof,
    )
    .map_err(|error| error.to_string())?;
    if proof.len() != PRODUCT_CLOSURE_MANIFEST_MAGIC.len() + 96
        || !proof.starts_with(PRODUCT_CLOSURE_MANIFEST_MAGIC)
    {
        return Err("invalid product closure manifest".to_owned());
    }
    let mut workspace = [0_u8; 32];
    let mut relation = [0_u8; 32];
    let mut input = [0_u8; 32];
    let mut offset = PRODUCT_CLOSURE_MANIFEST_MAGIC.len();
    workspace.copy_from_slice(&proof[offset..offset + 32]);
    offset += 32;
    relation.copy_from_slice(&proof[offset..offset + 32]);
    offset += 32;
    input.copy_from_slice(&proof[offset..offset + 32]);
    Ok(ProductClosureManifestClaim {
        workspace,
        relation,
        input,
    })
}

/// Returns the authenticated synthetic root commitment for a product closure
/// manifest.
#[must_use]
pub fn product_closure_root(
    workspace: WorkspaceRoot,
    relation: [u8; 32],
    input: [u8; 32],
) -> [u8; 32] {
    ObjectVersion::<BuiltinInputSchema>::from_value(&product_closure_manifest(
        workspace, relation, input,
    ))
    .to_bytes()
}

/// Derives the workspace input basis for a relation and authority.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn execution_input_basis(
    relation: &RelationState<ProductSourceRelation>,
    authority: AuthorityVersion,
) -> Result<WorkspaceRoot, String> {
    execution_input_manifest(relation, authority).map(|manifest| manifest.root())
}

/// Derives the product input basis directly from an owner-held lazy source.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn execution_input_basis_from_source(
    source: &ProductSourceSnapshot,
    authority: AuthorityVersion,
) -> Result<WorkspaceRoot, String> {
    execution_input_manifest_from_source(source, authority).map(|manifest| manifest.root())
}

/// Builds the checked product input manifest from a persisted relation root.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn execution_input_manifest_from_source(
    source: &ProductSourceSnapshot,
    authority: AuthorityVersion,
) -> Result<backend_version::CheckedWorkspaceManifest, String> {
    let authority_bytes = authority.to_bytes();
    if *source.authority_bytes() != authority_bytes {
        return Err("source authority does not match supplied authority".to_owned());
    }
    let coverage = authorized_coverage(authority)?;
    let root = source.relation().root_handle();
    let relation_binding =
        RelationBinding::from_persisted_root(&root, CoverageWitness::Complete(coverage));
    WorkspaceManifest::from_versions(
        1,
        vec![relation_binding],
        Vec::new(),
        authority,
        CoverageWitness::Complete(coverage),
    )
    .map_err(|error| error.to_string())
}

/// Builds the canonical workspace manifest that authenticates a product input
/// basis. It is retained for compatibility adapters; persisted workspace
/// dispatch uses the source snapshot variant above.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn execution_input_manifest(
    relation: &RelationState<ProductSourceRelation>,
    authority: AuthorityVersion,
) -> Result<backend_version::CheckedWorkspaceManifest, String> {
    if relation.coverage().state() != Coverage::Complete {
        return Err("authority-bearing workspace requires complete relation coverage".to_owned());
    }
    let coverage = authorized_coverage(authority)?;
    let relation_binding = RelationBinding::from_state(relation);
    WorkspaceManifest::from_versions(
        1,
        vec![relation_binding],
        Vec::new(),
        authority,
        CoverageWitness::Complete(coverage),
    )
    .map_err(|error| error.to_string())
}

/// Derives the workspace input basis for a checked semantic publication
/// relation and execution authority.
///
/// # Errors
/// Returns an error when relation coverage or authority admission fails.
pub fn semantic_execution_input_basis(
    relation: &RelationState<ProductSemanticPublicationRelation>,
    authority: AuthorityVersion,
) -> Result<WorkspaceRoot, String> {
    semantic_execution_input_manifest(relation, authority).map(|manifest| manifest.root())
}

/// Derives the input basis from the owner-selected semantic publication plane.
///
/// # Errors
/// Returns an error when the selected workspace authority and recipe authority
/// differ or the checked relation cannot be rebound.
pub fn semantic_execution_input_basis_from_snapshot(
    source: &ProductSemanticPublicationSnapshot,
    authority: AuthorityVersion,
) -> Result<WorkspaceRoot, String> {
    semantic_execution_input_manifest_from_snapshot(source, authority)
        .map(|manifest| manifest.root())
}

/// Builds a checked one-relation workspace manifest for remote semantic
/// execution from the owner-held lazy publication relation.
///
/// # Errors
/// Returns an error when the selected workspace authority differs or the
/// manifest cannot be admitted.
pub fn semantic_execution_input_manifest_from_snapshot(
    source: &ProductSemanticPublicationSnapshot,
    authority: AuthorityVersion,
) -> Result<backend_version::CheckedWorkspaceManifest, String> {
    if *source.authority_bytes() != authority.to_bytes() {
        return Err("semantic publication authority does not match recipe authority".to_owned());
    }
    let coverage = authorized_coverage(authority)?;
    let relation_binding = RelationBinding::from_persisted_root(
        &source.relation().root_handle(),
        CoverageWitness::Complete(coverage),
    );
    WorkspaceManifest::from_versions(
        1,
        vec![relation_binding],
        Vec::new(),
        authority,
        CoverageWitness::Complete(coverage),
    )
    .map_err(|error| error.to_string())
}

/// Builds a checked one-relation workspace manifest for a materialized
/// semantic publication relation used by focused protocol fixtures.
///
/// # Errors
/// Returns an error unless relation and authority coverage are complete.
pub fn semantic_execution_input_manifest(
    relation: &RelationState<ProductSemanticPublicationRelation>,
    authority: AuthorityVersion,
) -> Result<backend_version::CheckedWorkspaceManifest, String> {
    if relation.coverage().state() != Coverage::Complete {
        return Err("semantic execution requires complete relation transport coverage".to_owned());
    }
    let coverage = authorized_coverage(authority)?;
    WorkspaceManifest::from_versions(
        1,
        vec![RelationBinding::from_state(relation)],
        Vec::new(),
        authority,
        CoverageWitness::Complete(coverage),
    )
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        execution_input_basis, execution_input_manifest, execution_input_manifest_from_source,
    };
    use crate::builtin::{
        ProductSourceRelation, ProductSourceSnapshot, Profile, complete_coverage,
        product_source_fixture_with_authority, profile_descriptor,
    };
    use crate::workspace::WorkspaceHead;
    use backend_execution::AuthorityVersion;
    use backend_store::{
        ClosureManifest, FileStore, RelationAdmissionRegistry, TypedObject, WorkspaceClosure,
    };
    use backend_version::{
        ObjectClosure, ObjectKey, RelationBinding, WorkspaceError, WorkspaceManifest,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SOURCE_FIXTURE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn compiled_profiles_start_with_checked_authority_closure() -> Result<(), String> {
        for profile in [Profile::Product, Profile::EchoFixture] {
            let descriptor = profile_descriptor(profile)?;
            let relation = product_source_fixture_with_authority(false, descriptor.ids.authority)?;
            let manifest = execution_input_manifest(&relation, descriptor.ids.authority)?;

            assert!(manifest.is_checked());
            assert_eq!(
                manifest.authority_closure(),
                Some(ObjectClosure::from_version(descriptor.ids.authority))
            );
            assert_eq!(
                execution_input_basis(&relation, descriptor.ids.authority)?,
                manifest.root()
            );
        }
        Ok(())
    }

    #[test]
    fn encoded_profile_manifest_is_untrusted_until_re_admitted() -> Result<(), String> {
        let descriptor = profile_descriptor(Profile::Product)?;
        let relation = product_source_fixture_with_authority(false, descriptor.ids.authority)?;
        let original = execution_input_manifest(&relation, descriptor.ids.authority)?;
        let encoded = original.encode();

        let raw =
            WorkspaceManifest::decode_untrusted(&encoded).map_err(|error| error.to_string())?;
        assert!(!raw.is_checked());
        assert_eq!(raw.authority_closure(), None);
        assert_eq!(raw.validate(), Err(WorkspaceError::UnverifiedManifest));

        let binding = RelationBinding::from_state(&relation);
        let admitted = raw
            .admit_checked(
                vec![binding],
                Vec::new(),
                ObjectClosure::from_version(descriptor.ids.authority),
                complete_coverage(descriptor.ids.authority)?,
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(admitted.root(), original.root());

        let wrong_authority = AuthorityVersion::from_value(b"backend.worker.wrong.authority.v1");
        let result = WorkspaceManifest::decode_untrusted(&encoded)
            .map_err(|error| error.to_string())?
            .admit_checked(
                vec![binding],
                Vec::new(),
                ObjectClosure::from_version(wrong_authority),
                complete_coverage(wrong_authority)?,
            );
        assert_eq!(result, Err(WorkspaceError::AuthorityMismatch));
        Ok(())
    }

    #[test]
    fn source_manifest_rejects_a_mismatched_authority() -> Result<(), String> {
        let descriptor = profile_descriptor(Profile::Product)?;
        let authority = descriptor.ids.authority;
        let relation = product_source_fixture_with_authority(false, authority)?;
        let manifest = execution_input_manifest(&relation, authority)?;
        let registry = RelationAdmissionRegistry::default()
            .with_relation::<ProductSourceRelation>()
            .map_err(|error| format!("register product relation: {error:?}"))?;
        let store_path = std::env::temp_dir().join(format!(
            "backend-engine-builtin-source-authority-{}-{}",
            std::process::id(),
            NEXT_SOURCE_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let store = FileStore::open_with_registry(&store_path, 8 * 1024 * 1024, registry.clone())
            .map_err(|error| format!("open source store: {error:?}"))?;
        store
            .write_relation_state(&relation)
            .map_err(|error| format!("write source relation: {error:?}"))?;
        let relation_object = TypedObject::from_relation_state(&relation)
            .map_err(|error| format!("materialize source relation: {error:?}"))?;
        let authority_key = ObjectKey::<backend_execution::AuthorityVersionSchema>::from_value(
            super::super::PRODUCT_AUTHORITY_BYTES,
        );
        let authority_object =
            TypedObject::from_value(&authority_key, super::super::PRODUCT_AUTHORITY_BYTES);
        let mut objects = vec![relation_object, authority_object];
        objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let closure_manifest =
            ClosureManifest::new(objects).map_err(|error| format!("source closure: {error:?}"))?;
        let closure = WorkspaceClosure::from_checked_manifest_root_only_with_registry(
            &manifest,
            closure_manifest,
            &registry,
        )
        .map_err(|error| format!("admit source closure: {error:?}"))?;
        let head = WorkspaceHead::genesis_with_registry(manifest, closure, &registry)
            .map_err(|error| format!("source genesis: {error:?}"))?;
        let snapshot = head.snapshot().with_store(Arc::new(store));
        let source = ProductSourceSnapshot::from_workspace(&snapshot)?;

        let wrong_authority = AuthorityVersion::from_value(b"backend.worker.other.authority.v1");
        let mismatch = execution_input_manifest_from_source(&source, wrong_authority);
        assert!(matches!(
            mismatch,
            Err(message) if message == "source authority does not match supplied authority"
        ));
        let checked = execution_input_manifest_from_source(&source, authority)?;
        assert_eq!(
            checked.authority_closure(),
            Some(ObjectClosure::from_version(authority))
        );

        std::fs::remove_dir_all(store_path)
            .map_err(|error| format!("remove source store: {error}"))?;
        Ok(())
    }
}
