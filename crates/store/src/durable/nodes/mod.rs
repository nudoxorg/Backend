//! Persistent canonical node and closure-index storage.
//!
//! The submodules keep the durable node boundary small and explicit:
//! relation admission owns schema/version references, tree owns map
//! publication and hydration, manifest owns the lazy closure cursor, and
//! wire owns the checked descriptor/reference framing.

use super::{ClosureId, Hash, LayoutId, ObjectId, PackId, RawRelation, StateRoot, StoreError};
use crate::Node;
use std::sync::Arc;

mod manifest;
mod relation;
mod tree;
mod wire;

pub use manifest::{DurableManifest, DurableManifestPage, ManifestReadStats};
pub use relation::{OwnedRelationNodeLoader, RelationNodeChild, RelationNodeRead};
pub use tree::DurableTree;
pub(in crate::durable) use wire::{
    decode_manifest_descriptor, encode_manifest_descriptor, is_manifest_descriptor,
};

pub(super) const TREE_PACK_MAGIC: &[u8] = b"LUNA_TREE_PACK_V1\0";
pub(super) const MANIFEST_INDEX_MAGIC: &[u8] = b"LUNA_MANIFEST_INDEX_V2\0";
pub(super) const MANIFEST_INDEX_MAGIC_V1: &[u8] = b"LUNA_MANIFEST_INDEX_V1\0";
pub(super) const RELATION_REF_MAGIC: &[u8] = b"LUNA_REL_REF_V1\0";
pub(super) const NODE_MAX_COUNT: usize = 1_000_000;

/// One tree publication retained by the prepared typestate.
#[derive(Clone, Debug)]
pub(super) struct TreePublication {
    pub(super) layout: LayoutId,
    pub(super) target: StateRoot<RawRelation>,
    pub(super) root: Node,
    pub(super) id: PackId,
    pub(super) frontier: Option<Arc<[Node]>>,
}

impl TreePublication {
    pub(super) fn new(
        root: Node,
        target: StateRoot<RawRelation>,
        layout: LayoutId,
        frontier: Option<Arc<[Node]>>,
    ) -> Result<Self, StoreError> {
        if root.id().to_bytes() != *target.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        let id = tree::tree_pack_id(layout, target, root.id().to_bytes());
        Ok(Self {
            layout,
            target,
            root,
            id,
            frontier,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TreePackDescriptor {
    pub(super) id: PackId,
    pub(super) layout: LayoutId,
    pub(super) target: Hash,
    pub(super) root: Hash,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
/// Work performed while admitting one durable canonical tree publication.
pub struct TreeWriteStats {
    /// Number of canonical node objects created by the last tree publication.
    pub nodes_written: usize,
    /// Canonical bytes written by the last tree publication.
    pub bytes_written: usize,
    /// Number of canonical nodes inspected by the publication frontier.
    pub nodes_visited: usize,
}

/// Work observed while reading a lazy durable tree.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TreeReadStats {
    /// Canonical node objects opened by one lookup.
    pub nodes_read: usize,
    /// Canonical bytes read from node objects.
    pub bytes_read: usize,
}

/// Work and the root identity produced by schema-parametric relation-node
/// admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelationNodeWriteStats {
    root: ObjectId,
    /// Number of relation node objects created by this admission.
    pub nodes_written: usize,
    /// Bytes written for newly created relation node objects and descriptors.
    pub bytes_written: usize,
}

impl RelationNodeWriteStats {
    /// Returns the physical immutable-object identity of the admitted root.
    #[must_use]
    pub const fn root(self) -> ObjectId {
        self.root
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ManifestDescriptor {
    pub(super) id: ClosureId,
    pub(super) root: ObjectId,
    pub(super) count: Option<usize>,
}
