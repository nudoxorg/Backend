//! Storage projections for checked relation roots.

use super::{ClosureManifest, StoreError, TypedObject};
use backend_version::{
    CanonicalRelation, IdContext, TreeNodeHandle, UntrustedId, admit_canonical_root_claim,
};

pub(super) fn root_only_from_node<R: CanonicalRelation>(
    node: &TreeNodeHandle<R>,
) -> Result<ClosureManifest, StoreError> {
    let id = *node.id().as_bytes();
    let claim = UntrustedId::<R>::from_wire(&id, IdContext::relation::<R>())
        .map_err(|_| StoreError::Corrupt)?;
    let admitted = admit_canonical_root_claim::<R>(claim, node.canonical().as_bytes())
        .map_err(|_| StoreError::Corrupt)?;
    let (root, canonical) = admitted.into_parts();
    let object = TypedObject::from_state_root(root, &canonical)?;
    ClosureManifest::new_index(vec![object])
}
