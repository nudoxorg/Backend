//! Exact-base path-copy updates for the persistent closure index.

use super::{
    ClosureId, ClosureManifest, ManifestChange, ManifestEntry, ManifestLookup, ManifestState,
    ManifestWork, PreparedManifestDelta, StoreError,
};
use backend_version::{SchemaIdentity, TreeChange};
use std::sync::Arc;

pub(super) fn prepare_delta(
    manifest: &ClosureManifest,
    changes: &[ManifestChange],
) -> Result<PreparedManifestDelta, StoreError> {
    if changes
        .windows(2)
        .any(|window| window[0].key >= window[1].key)
    {
        return Err(StoreError::MalformedDelta);
    }
    let mut tree_changes = Vec::with_capacity(changes.len());
    for change in changes {
        let present = manifest.state.tree.contains(change.key.as_bytes());
        match (change.before.is_some(), present) {
            (false, false) => {}
            (true, true) => {
                let expected = change.before.as_ref().ok_or(StoreError::Corrupt)?;
                let key = SchemaIdentity::new(
                    expected.schema_domain,
                    expected.schema_type,
                    expected.schema_version,
                );
                let actual = manifest
                    .state
                    .lookup
                    .find_entry(
                        (key, expected.object_version),
                        expected.object_key,
                        change.key,
                    )
                    .ok_or_else(|| StoreError::BeforeMismatch(change.key.as_bytes().to_vec()))?;
                if ManifestEntry::from_object(&actual)? != *expected {
                    return Err(StoreError::BeforeMismatch(change.key.as_bytes().to_vec()));
                }
            }
            _ => return Err(StoreError::BeforeMismatch(change.key.as_bytes().to_vec())),
        }
        let after = change
            .after
            .as_ref()
            .map(ManifestEntry::from_object)
            .transpose()?;
        if after.is_none() && !present {
            return Err(StoreError::WrongBase);
        }
        tree_changes.push(TreeChange {
            key: *change.key.as_bytes(),
            after: after.map(|_| ()),
        });
    }
    let (tree, tree_work) = manifest.state.tree.apply(&tree_changes)?;
    let lookup = ManifestLookup::delta(&manifest.state.lookup, changes);
    let retained_objects = changes
        .iter()
        .filter_map(|change| change.after.as_ref().map(|object| Arc::new(object.clone())))
        .collect::<Vec<_>>();
    let target = ClosureManifest {
        id: ClosureId::from_bytes(tree.root_id()),
        state: Arc::new(ManifestState::new(
            tree,
            lookup,
            Arc::from(retained_objects.into_boxed_slice()),
        )),
    };
    Ok(PreparedManifestDelta {
        base: manifest.id,
        target,
        work: ManifestWork::from_tree(tree_work),
    })
}
