//! Persistent canonical relation used as a closure's object index.
//!
//! A closure is an ordered relation from an immutable object identity to its
//! compact typed descriptor.  The relation is deliberately separate from the
//! object bytes: the bytes live in the immutable object CAS, while this tree
//! gives a closure a canonical root and permits exact path-copy updates.

use super::{Hash, ObjectId, StoreError, TypedObject};
use backend_version::{
    CanonicalRelation, PersistentTree, Relation, RelationDecodeError, TreeChange, TreeError,
    TreeNodeHandle,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The schema for a persistent closure object index.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ManifestRelation;

/// The compact value retained in one closure-index row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManifestEntry {
    pub(super) schema_domain: u8,
    pub(super) schema_type: u16,
    pub(super) schema_version: u8,
    pub(super) object_key: Hash,
    pub(super) object_version: Hash,
    pub(super) byte_len: u64,
}

impl ManifestEntry {
    pub(crate) fn from_object(object: &TypedObject) -> Result<Self, StoreError> {
        Ok(Self {
            schema_domain: object.schema().domain(),
            schema_type: object.schema().ty(),
            schema_version: object.schema().version(),
            object_key: *object.key(),
            object_version: *object.version(),
            byte_len: u64::try_from(object.bytes().len()).map_err(|_| StoreError::Bounds)?,
        })
    }
}

impl Relation for ManifestRelation {
    const DOMAIN: u8 = 0x73;
    const TYPE: u16 = 0x7f01;

    type Key = Hash;
    // The object identity is the set membership commitment.  Keeping the
    // descriptor out of the canonical row makes the shared byte cap apply to
    // all manifest sizes; descriptors remain in the lazy record overlay.
    type Value = ();

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.extend_from_slice(key);
    }

    fn encode_value(_value: &Self::Value, _out: &mut Vec<u8>) {}
}

impl CanonicalRelation for ManifestRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.is_empty() {
            Ok(())
        } else {
            Err(RelationDecodeError::Malformed)
        }
    }
}

pub(super) type ManifestTreeKernel = PersistentTree<ManifestRelation>;

/// Immutable persistent closure-index tree plus the changed frontier used by
/// the durable node CAS.
#[derive(Debug)]
pub(super) struct ManifestTree {
    kernel: ManifestTreeKernel,
    source_root: Option<Hash>,
    frontier: Option<Arc<[TreeNodeHandle<ManifestRelation>]>>,
}

impl Clone for ManifestTree {
    fn clone(&self) -> Self {
        Self {
            kernel: self.kernel.clone(),
            source_root: self.source_root,
            frontier: self.frontier.clone(),
        }
    }
}

impl ManifestTree {
    pub(crate) fn from_objects(
        objects: &BTreeMap<ObjectId, Arc<TypedObject>>,
    ) -> Result<Self, StoreError> {
        let items = objects
            .keys()
            .map(|id| Ok((id.as_bytes().to_owned(), ())))
            .collect::<Result<Vec<_>, StoreError>>()?;
        let kernel = PersistentTree::from_sorted_items(&items).map_err(map_tree_error)?;
        Ok(Self {
            kernel,
            source_root: None,
            frontier: None,
        })
    }

    pub(crate) fn root_id(&self) -> Hash {
        self.kernel.root_handle().id().to_bytes()
    }

    pub(crate) fn apply(
        &self,
        changes: &[TreeChange<ManifestRelation>],
    ) -> Result<(Self, backend_version::TreeWork), StoreError> {
        if changes.is_empty() {
            return Ok((self.clone(), backend_version::TreeWork::default()));
        }
        let prepared = self
            .kernel
            .prepare_update(changes)
            .map_err(map_tree_error)?;
        let work = prepared.work();
        let target_kernel = prepared.commit();
        let frontier = changed_frontier(&self.kernel.root_handle(), &target_kernel.root_handle())?;
        let source_root = self.root_id();
        Ok((
            Self {
                kernel: target_kernel,
                source_root: Some(source_root),
                frontier: Some(Arc::from(frontier.into_boxed_slice())),
            },
            work,
        ))
    }

    pub(crate) fn root_handle(&self) -> TreeNodeHandle<ManifestRelation> {
        self.kernel.root_handle()
    }

    pub(crate) fn contains(&self, key: &Hash) -> bool {
        self.kernel.get(key).is_some()
    }

    pub(crate) fn source_root(&self) -> Option<Hash> {
        self.source_root
    }

    pub(crate) fn frontier(&self) -> Option<&[TreeNodeHandle<ManifestRelation>]> {
        self.frontier.as_deref()
    }
}

fn map_tree_error(error: TreeError) -> StoreError {
    match error {
        TreeError::UnsortedOrDuplicate => StoreError::MalformedDelta,
        TreeError::Canonical(_) | TreeError::InvalidRoot => StoreError::Corrupt,
        TreeError::Overflow => StoreError::Bounds,
    }
}

fn changed_frontier(
    old: &TreeNodeHandle<ManifestRelation>,
    new: &TreeNodeHandle<ManifestRelation>,
) -> Result<Vec<TreeNodeHandle<ManifestRelation>>, StoreError> {
    let mut output = Vec::new();
    collect_changed(old, new, &mut output)?;
    if output.is_empty() && old.id() != new.id() {
        output.push(new.clone());
    }
    Ok(output)
}

fn collect_changed(
    old: &TreeNodeHandle<ManifestRelation>,
    new: &TreeNodeHandle<ManifestRelation>,
    output: &mut Vec<TreeNodeHandle<ManifestRelation>>,
) -> Result<(), StoreError> {
    if old.id() == new.id() {
        return Ok(());
    }
    let old_children = old.children().collect::<Vec<_>>();
    let new_children = new.children().collect::<Vec<_>>();
    if old.summary().level == new.summary().level
        && old_children.len() == new_children.len()
        && !old_children.is_empty()
    {
        for (old_child, new_child) in old_children.iter().zip(new_children.iter()) {
            collect_changed(old_child, new_child, output)?;
        }
        output.push(new.clone());
        return Ok(());
    }
    collect_subtree(new, output)
}

fn collect_subtree(
    node: &TreeNodeHandle<ManifestRelation>,
    output: &mut Vec<TreeNodeHandle<ManifestRelation>>,
) -> Result<(), StoreError> {
    output.push(node.clone());
    for child in node.children() {
        collect_subtree(&child, output)?;
    }
    Ok(())
}
