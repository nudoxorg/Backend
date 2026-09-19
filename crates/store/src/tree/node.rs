//! The store adapter for the shared persistent canonical tree.

use crate::{Arc, Hash, HashMap, Mutex, RawRelation, StoreError};

/// One checked canonical node retained by an [`OrderedMap`](super::OrderedMap).
pub(crate) type Node = backend_version::TreeNodeHandle<RawRelation>;

/// Weak content interner shared by immutable map generations.
pub(crate) type Cas = Arc<Mutex<HashMap<Hash, backend_version::WeakTreeNodeHandle<RawRelation>>>>;

/// Interner adapter that keeps only weak handles so dropped map generations can
/// be reclaimed while live roots continue to deduplicate structurally equal
/// nodes.
#[derive(Clone)]
pub(crate) struct StoreInterner {
    pub(crate) cas: Cas,
}

impl backend_version::TreeInterner<RawRelation> for StoreInterner {
    fn intern(&self, node: Node) -> Node {
        let hash = node.id().to_bytes();
        let Ok(mut entries) = self.cas.lock() else {
            return node;
        };

        entries.retain(|_, weak| weak.upgrade().is_some());
        if let Some(existing) = entries
            .get(&hash)
            .and_then(backend_version::WeakTreeNodeHandle::upgrade)
        {
            return existing;
        }
        entries.insert(hash, node.downgrade());
        node
    }
}

pub(crate) fn map_node_error(error: backend_version::NodeError) -> StoreError {
    match error {
        backend_version::NodeError::OversizedNode => StoreError::Bounds,
        backend_version::NodeError::UnsortedOrDuplicate => StoreError::MalformedDelta,
        backend_version::NodeError::InvalidBranch
        | backend_version::NodeError::LevelMismatch
        | backend_version::NodeError::AnchorMismatch
        | backend_version::NodeError::MalformedEncoding
        | backend_version::NodeError::SchemaMismatch
        | backend_version::NodeError::NonCanonicalEncoding
        | backend_version::NodeError::RelationDecode(_) => StoreError::Corrupt,
    }
}

pub(crate) fn map_tree_error(error: backend_version::TreeError) -> StoreError {
    match error {
        backend_version::TreeError::UnsortedOrDuplicate => StoreError::MalformedDelta,
        backend_version::TreeError::InvalidRoot => StoreError::Corrupt,
        backend_version::TreeError::Overflow => StoreError::Bounds,
        backend_version::TreeError::Canonical(error) => map_node_error(error),
    }
}
