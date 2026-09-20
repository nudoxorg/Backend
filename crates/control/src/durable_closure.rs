//! Typed workspace closure construction for the durable control relation.

use backend_store::{ClosureManifest, RelationAdmissionRegistry, TypedObject, WorkspaceClosure};
use backend_version::{
    BasisBinding, CoverageWitness, ObjectKey, ObjectVersion, PersistedTreeRoot, RelationBinding,
    WorkspaceManifest,
};

use crate::ControlError;
use crate::ids::ControlAuthoritySchema;
use crate::record::ControlRelation;

pub(super) fn make_root_workspace_closure_from_root(
    root: &PersistedTreeRoot<ControlRelation>,
    coverage: CoverageWitness,
    summary: crate::ids::ControlSummary,
) -> Result<(WorkspaceManifest, WorkspaceClosure), ControlError> {
    let authority = ObjectVersion::<ControlAuthoritySchema>::from_value(&super::AUTHORITY_VALUE);
    let relation = RelationBinding::from_persisted_root(root, coverage);
    let summary_version = ObjectVersion::<crate::ids::ControlSummarySchema>::from_value(&summary);
    let manifest = WorkspaceManifest::from_versions(
        1,
        vec![relation],
        vec![BasisBinding::from_version(summary_version)],
        authority,
        coverage,
    )?;
    let mut objects = vec![TypedObject::from_state_root(
        root.root(),
        root.evidence().node(),
    )?];
    let summary_key = ObjectKey::<crate::ids::ControlSummarySchema>::from_value(&summary);
    objects.push(TypedObject::from_value(&summary_key, &summary));
    let authority_key = ObjectKey::<ControlAuthoritySchema>::from_value(&super::AUTHORITY_VALUE);
    objects.push(TypedObject::from_value(
        &authority_key,
        &super::AUTHORITY_VALUE,
    ));
    objects.sort_unstable_by_key(|object| (object.schema(), *object.key(), *object.version()));
    let registry = control_registry()?;
    let manifest_objects = ClosureManifest::new(objects).map_err(ControlError::from)?;
    let closure = WorkspaceClosure::from_checked_manifest_root_only_with_registry(
        &manifest,
        manifest_objects,
        &registry,
    )?;
    Ok((manifest, closure))
}

pub(super) fn control_registry() -> Result<RelationAdmissionRegistry, ControlError> {
    RelationAdmissionRegistry::new()
        .with_relation::<ControlRelation>()
        .map_err(ControlError::from)
}
