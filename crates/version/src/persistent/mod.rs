//! Persistent canonical relation tree with path-copy updates.

use std::sync::Arc;

use crate::{CanonicalRelation, CheckedCanonicalRoot, UntrustedId};

pub(crate) type Node<R> = Arc<node::TreeNode<R>>;
type Item<R> = (<R as crate::Relation>::Key, <R as crate::Relation>::Value);

mod build;
mod interner;
mod iter;
mod lazy;
mod node;
mod update;
mod view;
mod work;

pub use interner::{NoInterner, TreeInterner};
pub use iter::{TreeIter, TreeRangeIter};
pub use lazy::{
    LazyPreparedUpdate, LazyTree, LazyTreeError, LazyTreePage, LazyTreeWork, PersistedTreeRoot,
};
pub use node::{TreeNodeHandle, TreeNodeId, TreeNodeSummary, WeakTreeNodeHandle};
pub use view::{
    TreeNodeChildren, TreeNodeClosure, TreeNodeClosureWork, TreeNodeTraversalError, TreeNodeView,
    TreePath, TreeZipper,
};
pub use work::{TreeError, TreeWork};

/// Store independent callback for authenticated lazy node reopening.
///
/// A loader receives a context carrying the requested relation identity and
/// must return a node admitted from the exact canonical bytes for that claim.
/// Implementations may fetch only the node on the requested path; sibling
/// nodes remain authenticated summaries until a caller asks for them.  The
/// version crate deliberately owns this contract while storage owns caching,
/// I/O, and eviction policy.
pub trait TreeNodeLoader<R: CanonicalRelation> {
    /// Storage or transport error reported before canonical admission.
    type Error;

    /// Loads and admits one canonical node for an untrusted identity claim.
    ///
    /// The returned evidence binds the node bytes and typed root together;
    /// callers must not reconstruct that pairing from separate labels.
    ///
    /// # Errors
    ///
    /// The associated error is returned when storage cannot provide the
    /// requested node or canonical admission rejects its bytes.
    fn load(&self, claim: UntrustedId<R>) -> Result<CheckedCanonicalRoot<R>, Self::Error>;
}

/// One checked before/after key update for a persistent tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeChange<R: crate::Relation> {
    /// Ordered logical key being changed.
    pub key: R::Key,
    /// Value to retain at the key after the update, or `None` to delete it.
    pub after: Option<R::Value>,
}

/// Immutable canonical ordered tree with path-copy updates.
///
/// The node representation and constructors are private. A tree can only
/// enter this type through canonical checked rows, and updates return a new
/// tree while retaining untouched child arcs.
#[derive(Debug)]
pub struct PersistentTree<R: crate::Relation, I = NoInterner> {
    root: Node<R>,
    interner: I,
}

impl<R: crate::Relation, I: Clone> Clone for PersistentTree<R, I> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            interner: self.interner.clone(),
        }
    }
}

/// Prepared immutable tree update retaining its path-copied target tree.
#[derive(Debug)]
pub struct PreparedUpdate<R: crate::Relation, I = NoInterner> {
    tree: PersistentTree<R, I>,
    work: TreeWork,
}

impl<R: crate::Relation, I> PreparedUpdate<R, I> {
    /// Returns the work performed during preparation.
    #[must_use]
    pub const fn work(&self) -> TreeWork {
        self.work
    }

    /// Borrows the already materialized target tree.
    #[must_use]
    pub const fn tree(&self) -> &PersistentTree<R, I> {
        &self.tree
    }

    /// Consumes this update and publishes its retained target tree.
    #[must_use]
    pub fn commit(self) -> PersistentTree<R, I> {
        self.tree
    }
}
