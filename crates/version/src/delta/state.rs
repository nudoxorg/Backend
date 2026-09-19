//! Exact relation states and before/after transition algebra.
//!
//! A [`Delta`] is never free-floating: preparation binds it to the exact
//! complete base root, computes the target root, and derives the transition
//! identity from the ordered change set.  Applying, composing, and inverting
//! deltas retain those root and value preconditions.

use crate::{
    CanonicalNode, CoverageWitness, IdAdmissionError, IdContext, NodeError, ObjectVersion,
    Relation, Schema, SchemaIdentity, StateRoot, UntrustedId,
    persistent::{PersistentTree, TreeError},
};
use core::{fmt, marker::PhantomData};
use std::collections::BTreeMap;

/// One exact visible relation entry and its complete value version.
#[derive(Debug, Eq, PartialEq)]
pub struct RelationEntry<R: Relation> {
    /// Logical key.
    pub key: R::Key,
    /// Complete logical value.
    pub value: R::Value,
    /// Content identity of `value` under the relation schema.
    pub version: ObjectVersion<ValueSchema<R>>,
}

impl<R: Relation> Clone for RelationEntry<R> {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            value: self.value.clone(),
            version: self.version,
        }
    }
}

/// Adapter schema used to type relation value versions.
pub struct ValueSchema<R: Relation>(PhantomData<fn() -> R>);

impl<R: Relation> Schema for ValueSchema<R> {
    /// Relation domain reused for value identity separation.
    const DOMAIN: u8 = R::DOMAIN;
    /// Relation type reused for value identity separation.
    const TYPE: u16 = R::TYPE;
    /// Relation value encoding version.
    const VERSION: u8 = R::VERSION;
    /// Relation value type.
    type Value = R::Value;

    /// Uses the relation's canonical value encoder.
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        R::encode_value(value, out);
    }
}

/// Canonical visible ordered relation state.
#[derive(Debug)]
pub struct RelationState<R: Relation> {
    pub(crate) tree: PersistentTree<R>,
    pub(crate) root: StateRoot<R>,
    pub(crate) coverage: CoverageWitness,
}

impl<R: Relation> Clone for RelationState<R> {
    fn clone(&self) -> Self {
        Self {
            tree: self.tree.clone(),
            root: self.root,
            coverage: self.coverage,
        }
    }
}

impl<R: Relation> PartialEq for RelationState<R> {
    fn eq(&self, other: &Self) -> bool {
        self.root() == other.root()
            && self.coverage == other.coverage
            && self.iter().eq(other.iter())
    }
}

impl<R: Relation> Eq for RelationState<R> {}

/// Failure while admitting one visible relation state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    /// Two rows carried the same logical key.
    DuplicateKey,
    /// Canonical tree construction rejected the state.
    Canonical(NodeError),
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid relation state: {self:?}")
    }
}

impl std::error::Error for StateError {}

impl<R: Relation> RelationState<R> {
    /// Creates an empty checked relation state with the supplied coverage
    /// witness. The canonical empty root is produced by the version kernel,
    /// so this constructor cannot admit a missing or forged relation object.
    #[must_use]
    pub fn empty(coverage: CoverageWitness) -> Self {
        let tree = PersistentTree::empty();
        let root = tree.root().commitment();
        Self {
            tree,
            root,
            coverage,
        }
    }

    /// Builds a state from arbitrary input order after rejecting duplicates.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::DuplicateKey`] for duplicate logical keys or
    /// [`StateError::Canonical`] when canonical node limits are exceeded.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (R::Key, R::Value)>,
        coverage: CoverageWitness,
    ) -> Result<Self, StateError> {
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            if map.insert(key, value).is_some() {
                return Err(StateError::DuplicateKey);
            }
        }
        let items: Vec<_> = map.into_iter().collect();
        let tree = PersistentTree::from_items(&items).map_err(|error| match error {
            TreeError::Canonical(error) => StateError::Canonical(error),
            TreeError::UnsortedOrDuplicate | TreeError::InvalidRoot | TreeError::Overflow => {
                StateError::Canonical(NodeError::OversizedNode)
            }
        })?;
        let root = tree.root().commitment();
        Ok(Self {
            tree,
            root,
            coverage,
        })
    }

    /// Returns the exact canonical relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.root
    }

    /// Returns the coverage witness carried by this state.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Looks up one visible relation value.
    #[must_use]
    pub fn get(&self, key: &R::Key) -> Option<&R::Value> {
        self.tree.get(key)
    }

    /// Iterates visible relation entries in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = (&R::Key, &R::Value)> {
        self.tree.iter()
    }

    /// Seeks a bounded key range in canonical order without visiting keys
    /// before the lower bound.
    pub fn range<B: std::ops::RangeBounds<R::Key>>(
        &self,
        bounds: B,
    ) -> impl Iterator<Item = (&R::Key, &R::Value)> {
        self.tree.range(bounds)
    }

    /// Materializes this checked state as its exact canonical root object.
    ///
    /// The returned descriptor owns the canonical root node and retains the
    /// root commitment that was admitted when this state was built.  A store
    /// adapter can use its bytes and relation context to create its own
    /// checked typed object without accepting an unchecked digest/byte pair.
    #[must_use]
    pub fn materialize(&self) -> CheckedStateObjectRef<'_, R> {
        CheckedStateObjectRef {
            root: self.root,
            node: self.tree.root(),
        }
    }

    /// Traverses every retained canonical node in this state without
    /// rematerializing rows or cloning payloads.
    #[must_use]
    pub fn node_closure(&self) -> crate::persistent::TreeNodeClosure<'_, R> {
        self.tree.node_closure()
    }

    /// Creates a zipper over this state's retained canonical tree.
    #[must_use]
    pub fn zipper(&self) -> crate::persistent::TreeZipper<'_, R> {
        self.tree.zipper()
    }

    /// Returns an owning O(1) handle to the retained canonical root node.
    #[must_use]
    pub fn root_handle(&self) -> crate::persistent::TreeNodeHandle<R> {
        self.tree.root_handle()
    }
}

/// A lifetime-bound checked relation-state object view.
///
/// The view borrows the persistent tree's retained canonical root node, so
/// creating it and cloning the relation state are both O(1).  A store adapter
/// can borrow [`Self::canonical_bytes`] while constructing its one durable
/// ownership copy.
#[derive(Debug, Eq, PartialEq)]
pub struct CheckedStateObjectRef<'a, R: Relation> {
    root: StateRoot<R>,
    node: &'a CanonicalNode<R>,
}

impl<R: Relation> Copy for CheckedStateObjectRef<'_, R> {}

impl<R: Relation> Clone for CheckedStateObjectRef<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'a, R: Relation> CheckedStateObjectRef<'a, R> {
    pub(crate) const fn from_canonical_node(
        root: StateRoot<R>,
        node: &'a CanonicalNode<R>,
    ) -> Self {
        Self { root, node }
    }

    /// Returns the exact admitted relation state root.
    #[must_use]
    pub const fn root(self) -> StateRoot<R> {
        self.root
    }

    /// Returns the identity context a store adapter must retain.
    #[must_use]
    pub const fn context(self) -> IdContext {
        IdContext::relation::<R>()
    }

    /// Returns the schema identity a store adapter should attach to this
    /// materialized object.
    #[must_use]
    pub const fn schema(self) -> SchemaIdentity {
        SchemaIdentity::of_relation::<R>()
    }

    /// Returns the checked canonical root node.
    #[must_use]
    pub const fn canonical_node(self) -> &'a CanonicalNode<R> {
        self.node
    }

    /// Returns the exact canonical bytes committed by [`Self::root`].
    #[must_use]
    pub fn canonical_bytes(self) -> &'a [u8] {
        self.node.as_bytes()
    }

    /// Copies the borrowed node into an owned checked object descriptor.
    #[must_use]
    pub fn into_owned(self) -> CheckedStateObject<R> {
        CheckedStateObject {
            root: self.root,
            node: self.node.clone(),
        }
    }
}

/// A checked relation-state object ready for store materialization.
///
/// This is deliberately owned by the version layer so version has no
/// dependency on a physical object store.  It can only be created from a
/// [`CheckedStateObjectRef::into_owned`] or by
/// [`StateRoot::admit_canonical_node`], which verifies that the supplied
/// canonical bytes reproduce the claimed root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedStateObject<R: Relation> {
    root: StateRoot<R>,
    node: CanonicalNode<R>,
}

impl<R: Relation> CheckedStateObject<R> {
    /// Returns the exact admitted relation state root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.root
    }

    /// Returns the identity context a store adapter must retain.
    #[must_use]
    pub const fn context(&self) -> IdContext {
        IdContext::relation::<R>()
    }

    /// Returns the schema identity a store adapter should attach to this
    /// materialized object.
    #[must_use]
    pub const fn schema(&self) -> SchemaIdentity {
        SchemaIdentity::of_relation::<R>()
    }

    /// Returns the checked canonical root node.
    #[must_use]
    pub const fn canonical_node(&self) -> &CanonicalNode<R> {
        &self.node
    }

    /// Returns the exact canonical bytes committed by [`Self::root`].
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.node.as_bytes()
    }

    /// Consumes the descriptor and returns its checked canonical node.
    #[must_use]
    pub fn into_canonical_node(self) -> CanonicalNode<R> {
        self.node
    }
}

impl<R: Relation> StateRoot<R> {
    /// Admits canonical node bytes as a checked relation-state object.
    ///
    /// This method is the version boundary for store and replication
    /// adapters.  A root claim by itself is insufficient; the exact accepted
    /// canonical node must reproduce the claim's digest.
    ///
    /// # Errors
    ///
    /// Returns [`IdAdmissionError::DigestMismatch`] when the node does not
    /// reproduce this root.
    pub fn admit_canonical_node(
        self,
        node: CanonicalNode<R>,
    ) -> Result<CheckedStateObject<R>, IdAdmissionError> {
        let claim = UntrustedId::<R>::from_wire(self.as_bytes(), IdContext::relation::<R>())
            .map_err(|_| IdAdmissionError::DigestMismatch)?;
        Self::admit_canonical_bytes(claim, node.as_bytes())?;
        Ok(CheckedStateObject { root: self, node })
    }
}
