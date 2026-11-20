//! Workspace accessors and bounded row preflight for the Vue source relation.
//!
//! Rows remain untrusted until their selected owner, basis and actual host are
//! verified. No compiler language profile or complete graph is implied.
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_frontend_vue::{VueInventoryRecord, VueInventoryRelation, VueInventoryRootSchema};
use backend_store::TypedObject;
use backend_version::{ObjectKey, Relation, Schema, SchemaIdentity, StateRoot};
use std::collections::BTreeMap;

const ROOT_KEY: [u8; 32] = [0x47; 32];
const MAX_ROWS: usize = 100_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Check canonical key order and bounded work before staging a Vue relation.
pub fn validate_vue_inventory_rows(
    rows: &[([u8; 32], VueInventoryRecord)],
) -> Result<(), &'static str> {
    if rows.len() > MAX_ROWS {
        return Err("Vue inventory row limit");
    }
    let mut previous = None;
    let mut total = 0usize;
    let mut selections = BTreeMap::new();
    let mut hosts =
        BTreeMap::<[u8; 32], (usize, usize, Vec<([u8; 32], [u8; 32])>, Vec<String>)>::new();
    for (key, row) in rows {
        if previous.is_some_and(|prior| prior >= *key) || row.key() != *key {
            return Err("Vue inventory key mismatch or order");
        }
        previous = Some(*key);
        row.validate()?;
        match row {
            VueInventoryRecord::Selection {
                project,
                basis,
                producer,
                discovered,
                inventory_digest,
                unavailable,
                ..
            } => {
                if selections
                    .insert(
                        *project,
                        (
                            (*basis, *producer),
                            *discovered as usize,
                            *unavailable as usize,
                            *inventory_digest,
                        ),
                    )
                    .is_some()
                {
                    return Err("duplicate Vue project selection");
                }
            }
            VueInventoryRecord::Host(host) => {
                let owner = host.owner().0;
                let entry = hosts.entry(owner).or_default();
                entry.0 = entry.0.checked_add(1).ok_or("Vue host count overflow")?;
                entry.2.push((host.analysis_basis(), host.producer()));
                entry.3.push(host.owner().1.to_owned());
            }
            VueInventoryRecord::Unavailable { project, path, .. } => {
                let entry = hosts.entry(*project).or_default();
                entry.0 = entry.0.checked_add(1).ok_or("Vue host count overflow")?;
                entry.1 = entry
                    .1
                    .checked_add(1)
                    .ok_or("Vue unavailable count overflow")?;
                entry.3.push(path.clone());
            }
        }
        let mut value = Vec::new();
        VueInventoryRelation::encode_value(row, &mut value);
        total = total
            .checked_add(40)
            .and_then(|n| n.checked_add(value.len()))
            .ok_or("Vue inventory size overflow")?;
        if total > MAX_BYTES {
            return Err("Vue inventory byte limit");
        }
    }
    for (project, (count, unavailable, bases, paths)) in &hosts {
        let Some((basis, discovered, expected_unavailable, digest)) = selections.get(project)
        else {
            return Err("Vue host lacks selected project");
        };
        if count != discovered
            || unavailable != expected_unavailable
            || bases.iter().any(|seen| seen != basis)
            || {
                let mut paths = paths.clone();
                paths.sort();
                backend_frontend_vue::vue_path_inventory_digest(&paths)? != *digest
            }
        {
            return Err("Vue host selection count or basis differs");
        }
    }
    if selections
        .iter()
        .any(|(project, (_, discovered, unavailable, digest))| {
            !hosts.contains_key(project)
                && (*discovered != 0
                    || *unavailable != 0
                    || backend_frontend_vue::vue_path_inventory_digest(&[]).ok() != Some(*digest))
        })
    {
        return Err("Vue selection omits hosts");
    }
    Ok(())
}
/// Canonical key for the selected Vue inventory pointer.
pub fn vue_inventory_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<VueInventoryRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Build a typed pointer; selection authority belongs to the workspace owner.
pub fn vue_inventory_root_object(root: StateRoot<VueInventoryRelation>) -> TypedObject {
    let key = ObjectKey::<VueInventoryRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Open the selected Vue inventory; an absent root is unknown, not empty.
pub fn vue_inventory_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<VueInventoryRelation>>, WorkspaceRelationError> {
    snapshot.auxiliary_relation::<VueInventoryRelation>(
        SchemaIdentity::new(
            VueInventoryRootSchema::DOMAIN,
            VueInventoryRootSchema::TYPE,
            VueInventoryRootSchema::VERSION,
        ),
        vue_inventory_root_pointer_key(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_frontend_vue::{
        VueHostInventory, VueInventoryRecord, parse, vue_path_inventory_digest,
    };

    #[test]
    fn selected_host_keys_counts_and_path_witness_must_agree() {
        let parsed = parse(b"<script>const x = 1;</script>").unwrap();
        let host = VueInventoryRecord::Host(
            VueHostInventory::capture([1; 32], "src/App.vue", [2; 32], &parsed).unwrap(),
        );
        let selection = VueInventoryRecord::Selection {
            project: [1; 32],
            source_root: "/tmp/vue-fixture".into(),
            basis: [2; 32],
            producer: backend_frontend_vue::support_fingerprint(),
            discovered: 1,
            inventory_digest: vue_path_inventory_digest(&["src/App.vue".into()]).unwrap(),
            unavailable: 0,
            walk_truncated: false,
        };
        let mut rows = vec![(host.key(), host), (selection.key(), selection)];
        rows.sort_unstable_by_key(|(key, _)| *key);
        assert!(validate_vue_inventory_rows(&rows).is_ok());
        let mut wrong = rows.clone();
        wrong[0].0 = [9; 32];
        wrong.sort_unstable_by_key(|(key, _)| *key);
        assert!(validate_vue_inventory_rows(&wrong).is_err());
        let mut missing = rows.clone();
        missing.retain(|(_, row)| !matches!(row, VueInventoryRecord::Host(_)));
        assert!(validate_vue_inventory_rows(&missing).is_err());
    }
}
