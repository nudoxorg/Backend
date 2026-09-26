//! Checked workspace closure binding and reference admission.

use super::{ClosureId, ClosureManifest, Hash, RelationAdmissionRegistry, StoreError};
mod construct;
use backend_version::{
    CanonicalNode, CanonicalRelation, SchemaIdentity, StateRoot, TreeNodeHandle, WorkspaceRoot,
};

/// Borrowed access to a node whose canonical bytes already carry version
/// admission evidence.
///
/// Both retained trees and lazy path-copy updates implement this seam, so the
/// workspace closure can publish one relation frontier without converting it
/// into a second tree representation.
pub trait CheckedRelationNode<R: CanonicalRelation> {
    /// Returns the admitted canonical node.
    fn admitted_node(&self) -> &CanonicalNode<R>;
}

impl<R: CanonicalRelation> CheckedRelationNode<R> for TreeNodeHandle<R> {
    fn admitted_node(&self) -> &CanonicalNode<R> {
        self.canonical()
    }
}

impl<R: CanonicalRelation> CheckedRelationNode<R> for CanonicalNode<R> {
    fn admitted_node(&self) -> &CanonicalNode<R> {
        self
    }
}

impl<R, N> CheckedRelationNode<R> for &N
where
    R: CanonicalRelation,
    N: CheckedRelationNode<R>,
{
    fn admitted_node(&self) -> &CanonicalNode<R> {
        (*self).admitted_node()
    }
}

/// A closure manifest bound to a checked typed workspace root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceClosure {
    root: WorkspaceRoot,
    manifest: ClosureManifest,
    binding: WorkspaceBinding,
    root_only: bool,
    /// Relation nodes in the current path-copy frontier that still need
    /// physical publication. Only the selected root is retained in the
    /// closure manifest; child nodes move directly into the relation CAS.
    selected_roots: Vec<super::TypedObject>,
}

/// Exact workspace-root and closure binding persisted with a publication.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceBinding {
    pub(super) root: Hash,
    pub(super) closure: ClosureId,
    pub(super) proof: Hash,
}
