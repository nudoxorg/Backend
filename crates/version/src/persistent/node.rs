use super::{Item, Node};
use crate::Relation;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Weak};

/// Opaque retained handle to one checked canonical tree node.
pub struct TreeNodeHandle<R: Relation> {
    pub(super) node: Node<R>,
}

impl<R: Relation> fmt::Debug for TreeNodeHandle<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeNodeHandle")
            .field("id", &self.id())
            .finish()
    }
}

impl<R: Relation> Clone for TreeNodeHandle<R> {
    fn clone(&self) -> Self {
        Self {
            node: self.node.clone(),
        }
    }
}

/// Stable identity for one checked canonical tree node.
///
/// The identity is the node commitment.  Its fields are private so callers
/// can compare and retain identities, but cannot mint a node or identity that
/// was not emitted by a checked tree constructor.
pub struct TreeNodeId<R: Relation> {
    bytes: [u8; crate::ID_BYTES],
    _marker: core::marker::PhantomData<fn() -> R>,
}

impl<R: Relation> fmt::Debug for TreeNodeId<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TreeNodeId").field(&self.bytes).finish()
    }
}

impl<R: Relation> PartialEq for TreeNodeId<R> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<R: Relation> Eq for TreeNodeId<R> {}

impl<R: Relation> Hash for TreeNodeId<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}

impl<R: Relation> PartialOrd for TreeNodeId<R> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<R: Relation> Ord for TreeNodeId<R> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<R: Relation> Copy for TreeNodeId<R> {}

impl<R: Relation> Clone for TreeNodeId<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> TreeNodeId<R> {
    /// Returns the stable commitment bytes for this node identity.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; crate::ID_BYTES] {
        &self.bytes
    }

    /// Copies the stable commitment bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; crate::ID_BYTES] {
        self.bytes
    }
}

/// Weak retained reference to a checked canonical tree node.
///
/// Upgrading this token succeeds while any owning tree or handle retains the
/// node.  The identity remains available after the node is collected, which
/// lets storage adapters perform a lock-free identity/CAS probe first.
pub struct WeakTreeNodeHandle<R: Relation> {
    node: Weak<TreeNode<R>>,
    id: TreeNodeId<R>,
}

impl<R: Relation> fmt::Debug for WeakTreeNodeHandle<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WeakTreeNodeHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl<R: Relation> Clone for WeakTreeNodeHandle<R> {
    fn clone(&self) -> Self {
        Self {
            node: self.node.clone(),
            id: self.id,
        }
    }
}

/// Immutable metadata for one retained canonical tree node.
pub struct TreeNodeSummary<R: Relation> {
    /// First key anchor, if the node is nonempty.
    pub first_key: Option<R::Key>,
    /// Canonical commitment of this node.
    pub commitment: crate::StateRoot<R>,
    /// Node level, where leaves are zero.
    pub level: u16,
    /// Number of visible rows below this node.
    pub len: usize,
    /// Authenticated number of visible rows below this node.
    pub row_count: u64,
    /// Descendant counts indexed by level.
    pub descendant_counts: Box<[usize]>,
}

impl<R: Relation> Clone for TreeNodeSummary<R> {
    fn clone(&self) -> Self {
        Self {
            first_key: self.first_key.clone(),
            commitment: self.commitment,
            level: self.level,
            len: self.len,
            row_count: self.row_count,
            descendant_counts: self.descendant_counts.clone(),
        }
    }
}

impl<R: Relation> fmt::Debug for TreeNodeSummary<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeNodeSummary")
            .field("first_key", &self.first_key)
            .field("commitment", &self.commitment)
            .field("level", &self.level)
            .field("len", &self.len)
            .field("row_count", &self.row_count)
            .field("descendant_counts", &self.descendant_counts)
            .finish()
    }
}

impl<R: Relation> PartialEq for TreeNodeSummary<R> {
    fn eq(&self, other: &Self) -> bool {
        self.first_key == other.first_key
            && self.commitment == other.commitment
            && self.level == other.level
            && self.len == other.len
            && self.row_count == other.row_count
            && self.descendant_counts == other.descendant_counts
    }
}

impl<R: Relation> Eq for TreeNodeSummary<R> {}

#[derive(Clone, Debug)]
pub(crate) struct TreeNode<R: Relation> {
    pub(super) canonical: Arc<crate::CanonicalNode<R>>,
    pub(super) leaf_items: Option<Arc<[Item<R>]>>,
    pub(super) children: Option<Arc<[Node<R>]>>,
    pub(super) child_entry_offsets: Option<Arc<[usize]>>,
    pub(super) len: usize,
    pub(super) level_counts: Arc<[usize]>,
}
impl<R: Relation> TreeNodeHandle<R> {
    /// Returns the stable identity of this checked node.
    #[must_use]
    pub fn id(&self) -> TreeNodeId<R> {
        let commitment = self.node.canonical.commitment();
        TreeNodeId {
            bytes: *commitment.as_bytes(),
            _marker: core::marker::PhantomData,
        }
    }

    /// Downgrades this owning handle without extending node lifetime.
    #[must_use]
    pub fn downgrade(&self) -> WeakTreeNodeHandle<R> {
        WeakTreeNodeHandle {
            node: Arc::downgrade(&self.node),
            id: self.id(),
        }
    }

    /// Returns immutable canonical metadata for this handle.
    #[must_use]
    pub fn summary(&self) -> TreeNodeSummary<R> {
        TreeNodeSummary {
            first_key: self.node.first_key().cloned(),
            commitment: self.node.canonical.commitment(),
            level: self.node.level(),
            len: self.node.len,
            row_count: self.node.canonical.row_count(),
            descendant_counts: self.node.level_counts.to_vec().into_boxed_slice(),
        }
    }

    /// Returns the exact canonical bytes for this node.
    #[must_use]
    pub fn canonical(&self) -> &crate::CanonicalNode<R> {
        self.node.canonical.as_ref()
    }

    /// Returns retained child handles in canonical order.
    pub fn children(&self) -> impl Iterator<Item = TreeNodeHandle<R>> + '_ {
        self.node
            .children()
            .into_iter()
            .flatten()
            .cloned()
            .map(|node| TreeNodeHandle { node })
    }

    /// Returns leaf rows when this handle is a leaf.
    #[must_use]
    pub fn entries(&self) -> Option<&[(R::Key, R::Value)]> {
        self.node.entries()
    }

    pub(super) fn same_structure(&self, other: &Self) -> bool {
        self.node.canonical.commitment() == other.node.canonical.commitment()
            && self.node.level() == other.node.level()
            && self.node.len == other.node.len
            && self.node.level_counts.as_ref() == other.node.level_counts.as_ref()
            && self.node.child_entry_offsets() == other.node.child_entry_offsets()
            && self.node.first_key() == other.node.first_key()
    }
}

pub(super) fn node_id<R: Relation>(node: &TreeNode<R>) -> TreeNodeId<R> {
    let commitment = node.canonical.commitment();
    TreeNodeId {
        bytes: *commitment.as_bytes(),
        _marker: core::marker::PhantomData,
    }
}

impl<R: Relation> WeakTreeNodeHandle<R> {
    /// Returns the stable identity retained by this weak token.
    #[must_use]
    pub const fn id(&self) -> TreeNodeId<R> {
        TreeNodeId {
            bytes: self.id.bytes,
            _marker: core::marker::PhantomData,
        }
    }

    /// Attempts to upgrade this token to an owning checked node handle.
    #[must_use]
    pub fn upgrade(&self) -> Option<TreeNodeHandle<R>> {
        self.node.upgrade().and_then(|node| {
            let handle = TreeNodeHandle { node };
            (handle.id().to_bytes() == self.id.to_bytes()).then_some(handle)
        })
    }
}
impl<R: Relation> TreeNode<R> {
    pub(super) fn first_key(&self) -> Option<&R::Key> {
        self.canonical.first_key()
    }
    pub(super) fn level(&self) -> u16 {
        self.canonical.level()
    }
    pub(super) fn entries(&self) -> Option<&[Item<R>]> {
        self.leaf_items.as_deref()
    }
    pub(super) fn children(&self) -> Option<&[Node<R>]> {
        self.children.as_deref()
    }
    pub(super) fn child_entry_offsets(&self) -> Option<&[usize]> {
        self.child_entry_offsets.as_deref()
    }
    pub(super) fn entry_count(&self) -> usize {
        self.len
    }
    pub(super) fn node_count_at_level(&self, level: u16) -> usize {
        self.level_counts
            .get(usize::from(level))
            .copied()
            .unwrap_or(0)
    }
    pub(super) fn leaf_count(&self) -> usize {
        self.node_count_at_level(0)
    }
}
