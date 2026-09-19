use core::fmt;
use core::marker::PhantomData;

use crate::{
    CanonicalRelation, ID_BYTES, IdAdmissionError, Relation, RelationDecodeError, StateRoot,
};

/// Opaque canonical node bytes and their authenticated relation commitment.
pub struct CanonicalNode<R: Relation> {
    bytes: Vec<u8>,
    commitment: StateRoot<R>,
    first_key: Option<R::Key>,
    level: u16,
    row_count: u64,
}

impl<R: Relation> Clone for CanonicalNode<R> {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            commitment: self.commitment,
            first_key: self.first_key.clone(),
            level: self.level,
            row_count: self.row_count,
        }
    }
}

impl<R: Relation> fmt::Debug for CanonicalNode<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CanonicalNode")
            .field("bytes", &self.bytes)
            .field("commitment", &self.commitment)
            .field("level", &self.level)
            .field("row_count", &self.row_count)
            .field("first_key", &self.first_key)
            .finish()
    }
}

impl<R: Relation> PartialEq for CanonicalNode<R> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<R: Relation> Eq for CanonicalNode<R> {}

impl<R: Relation> CanonicalNode<R> {
    pub(crate) fn from_parts(
        bytes: Vec<u8>,
        commitment: StateRoot<R>,
        first_key: Option<R::Key>,
        level: u16,
        row_count: u64,
    ) -> Self {
        Self {
            bytes,
            commitment,
            first_key,
            level,
            row_count,
        }
    }

    /// Returns canonical encoded bytes suitable for immutable storage.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the commitment derived directly from accepted canonical bytes.
    #[must_use]
    pub const fn commitment(&self) -> StateRoot<R> {
        self.commitment
    }

    /// Returns the tree level, where leaves are zero.
    #[must_use]
    pub const fn level(&self) -> u16 {
        self.level
    }

    /// Returns the authenticated number of logical rows below this node.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.row_count
    }

    /// Returns the first key anchor, if this node is nonempty.
    #[must_use]
    pub fn first_key(&self) -> Option<&R::Key> {
        self.first_key.as_ref()
    }
}

/// An authenticated canonical node admitted from bounded wire bytes.
///
/// A branch node authenticates its child anchors and commitments but does not
/// materialize the child objects; those are admitted independently from the
/// immutable object closure.  The private fields ensure callers cannot pair a
/// parsed node with a different root after admission.
pub struct CheckedCanonicalRoot<R: CanonicalRelation> {
    node: CanonicalNode<R>,
    root: StateRoot<R>,
}

impl<R: CanonicalRelation> fmt::Debug for CheckedCanonicalRoot<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheckedCanonicalRoot")
            .field("root", &self.root)
            .field("node", &self.node)
            .finish()
    }
}

impl<R: CanonicalRelation> Clone for CheckedCanonicalRoot<R> {
    fn clone(&self) -> Self {
        Self {
            node: self.node.clone(),
            root: self.root,
        }
    }
}

impl<R: CanonicalRelation> PartialEq for CheckedCanonicalRoot<R> {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root && self.node == other.node
    }
}

impl<R: CanonicalRelation> Eq for CheckedCanonicalRoot<R> {}

impl<R: CanonicalRelation> CheckedCanonicalRoot<R> {
    pub(crate) fn from_parts(root: StateRoot<R>, node: CanonicalNode<R>) -> Self {
        Self { node, root }
    }

    /// Returns the root commitment recomputed from the admitted bytes.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.root
    }

    /// Returns the checked canonical node descriptor.
    #[must_use]
    pub const fn node(&self) -> &CanonicalNode<R> {
        &self.node
    }

    /// Returns the authenticated number of logical rows below this root.
    #[must_use]
    pub const fn row_count(&self) -> u64 {
        self.node.row_count()
    }

    /// Returns the exact bytes covered by [`Self::root`].
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.node.as_bytes()
    }

    /// Consumes the evidence and returns its checked node and root together.
    #[must_use]
    pub fn into_parts(self) -> (StateRoot<R>, CanonicalNode<R>) {
        (self.root, self.node)
    }
}

/// One child node and its first-key anchor for branch encoding.
#[derive(Debug, Eq, PartialEq)]
pub struct Child<R: Relation> {
    /// First key covered by the child.
    pub first_key: R::Key,
    /// Canonical child node.
    pub node: CanonicalNode<R>,
}

impl<R: Relation> Clone for Child<R> {
    fn clone(&self) -> Self {
        Self {
            first_key: self.first_key.clone(),
            node: self.node.clone(),
        }
    }
}

/// An authenticated child summary used to recompute a branch commitment.
///
/// A proof does not need a sibling's encoded bytes: the parent commits to the
/// child's first key, tree level, and fixed-width commitment reference. This type remains
/// separate from [`CanonicalNode`] so a caller cannot claim to own bytes it did
/// not decode.
#[derive(Debug, Eq, PartialEq)]
pub struct CommittedChild<R: Relation> {
    /// First key covered by the child.
    pub first_key: R::Key,
    /// Canonical root committed by the child.
    pub commitment: ChildCommitment<R>,
    /// Child level, where a leaf is level zero.
    pub level: u16,
    /// Number of logical rows covered by the child subtree.
    pub row_count: u64,
}

impl<R: Relation> Clone for CommittedChild<R> {
    fn clone(&self) -> Self {
        Self {
            first_key: self.first_key.clone(),
            commitment: self.commitment,
            level: self.level,
            row_count: self.row_count,
        }
    }
}

/// An untrusted fixed-width commitment reference carried by a branch.
///
/// This is deliberately distinct from [`StateRoot`]: a child reference only
/// identifies the bytes named by a parent and does not admit those bytes as a
/// canonical relation node. Obtain a [`StateRoot`] by admitting the referenced
/// canonical node through [`crate::admit_canonical_root_claim`].
///
/// ```compile_fail
/// use backend_version::{ChildCommitment, Relation, StateRoot};
/// fn forged<R: Relation>(bytes: &[u8]) {
///     let reference = ChildCommitment::<R>::from_bytes(bytes).unwrap();
///     let _root: StateRoot<R> = reference;
/// }
/// ```
pub struct ChildCommitment<R: Relation> {
    bytes: [u8; ID_BYTES],
    marker: PhantomData<fn() -> R>,
}

impl<R: Relation> Copy for ChildCommitment<R> {}

impl<R: Relation> Clone for ChildCommitment<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R: Relation> fmt::Debug for ChildCommitment<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ChildCommitment").field(&self.bytes).finish()
    }
}

impl<R: Relation> PartialEq for ChildCommitment<R> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<R: Relation> Eq for ChildCommitment<R> {}

impl<R: Relation> ChildCommitment<R> {
    /// Parses one fixed-width commitment reference from a wire slice.
    ///
    /// The result is an untrusted reference until the corresponding child
    /// node is admitted. This parser checks width only.
    ///
    /// # Errors
    /// Returns [`NodeError::MalformedEncoding`] when `bytes` is not exactly
    /// one commitment width.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NodeError> {
        let bytes = bytes.try_into().map_err(|_| NodeError::MalformedEncoding)?;
        Ok(Self {
            bytes,
            marker: PhantomData,
        })
    }

    /// Creates a reference from a checked child root commitment.
    #[must_use]
    pub const fn from_state_root(root: StateRoot<R>) -> Self {
        Self {
            bytes: root.to_bytes(),
            marker: PhantomData,
        }
    }

    /// Returns the fixed-width referenced commitment bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; ID_BYTES] {
        &self.bytes
    }

    /// Copies the fixed-width referenced commitment bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; ID_BYTES] {
        self.bytes
    }
}

impl<R: Relation> From<StateRoot<R>> for ChildCommitment<R> {
    fn from(root: StateRoot<R>) -> Self {
        Self::from_state_root(root)
    }
}

/// Failure while constructing a canonical node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeError {
    /// Entries or child anchors were not strictly increasing.
    UnsortedOrDuplicate,
    /// The requested branch shape was empty, leaf-level, or could not reduce.
    InvalidBranch,
    /// A branch child was not exactly one level below its parent.
    LevelMismatch,
    /// A public child anchor did not match its node's authenticated first key.
    AnchorMismatch,
    /// A node exceeded the policy's maximum entry count or encoded-byte size.
    OversizedNode,
    /// The wire bytes did not contain one complete canonical node grammar.
    MalformedEncoding,
    /// The node header named a different ABI, policy, or relation schema.
    SchemaMismatch,
    /// A decoded key or value did not round-trip to its canonical bytes.
    NonCanonicalEncoding,
    /// A relation decoder rejected a key or value body.
    RelationDecode(RelationDecodeError),
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid canonical node: {self:?}")
    }
}

impl std::error::Error for NodeError {}

/// Failure while binding a validated canonical node to a wire root claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalRootAdmissionError {
    /// The canonical node grammar or relation decoder rejected the bytes.
    Node(NodeError),
    /// The supplied root claim carried the wrong context or digest.
    Identity(IdAdmissionError),
}

impl fmt::Display for CanonicalRootAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "canonical relation root admission failed: {self:?}")
    }
}

impl std::error::Error for CanonicalRootAdmissionError {}
