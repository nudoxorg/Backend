//! Authenticated page model and resumable frontier state.

use super::page;
use crate::ReplicationError;
use backend_version::{ObjectVersion as VersionObjectVersion, Schema};
use std::collections::VecDeque;

/// A fixed-width Merkle node commitment.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeDigest(pub [u8; 32]);
impl NodeDigest {
    /// Returns the exact digest bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// An exact Merkle root bound to the schema ABI that produced it.
///
/// A root is a checked commitment to an admitted canonical manifest. Its
/// representation cannot be assembled by copying fields from a wire claim:
/// ```compile_fail
/// use backend_replication::{MerkleRoot, NodeDigest};
/// let _forged = MerkleRoot { schema: 1, digest: NodeDigest([0; 32]) };
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MerkleRoot {
    /// Canonical workspace/relation schema ABI.
    schema: u32,
    /// Root node commitment.
    digest: NodeDigest,
}
impl MerkleRoot {
    /// Creates the checked Merkle root for an admitted canonical manifest.
    ///
    /// The version identity is supplied by the owner of the concrete
    /// manifest schema. This keeps replication from hashing a second, subtly
    /// different manifest grammar or promoting a raw digest into a root.
    #[must_use]
    pub fn from_admitted_manifest<T: Schema>(
        schema: u32,
        manifest: VersionObjectVersion<T>,
    ) -> Self {
        Self {
            schema,
            digest: NodeDigest(manifest.to_bytes()),
        }
    }

    /// Returns the schema ABI bound to this checked root.
    #[must_use]
    pub const fn schema(self) -> u32 {
        self.schema
    }

    /// Returns the node commitment bound to this checked root.
    #[must_use]
    pub const fn digest(self) -> NodeDigest {
        self.digest
    }

    /// Returns the untrusted wire form for persistence or transport.
    #[must_use]
    pub const fn to_claim(self) -> MerkleRootClaim {
        MerkleRootClaim {
            schema: self.schema,
            digest: self.digest,
        }
    }

    /// Binds a node commitment to one schema ABI.
    ///
    /// This compatibility constructor is retained for internal tree fixtures
    /// and legacy adapters. Production closure roots should use
    /// [`Self::from_admitted_manifest`] so the commitment comes from a typed
    /// canonical manifest identity.
    #[must_use]
    pub(crate) const fn new(schema: u32, digest: NodeDigest) -> Self {
        Self { schema, digest }
    }
}

/// Untrusted Merkle root claim decoded from a wire or durable marker.
///
/// The claim retains its schema and commitment but cannot be used to start a
/// traversal until it is matched against a caller-owned [`MerkleRoot`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MerkleRootClaim {
    schema: u32,
    digest: NodeDigest,
}

impl MerkleRootClaim {
    /// Decodes the fixed root fields without promoting them to a checked root.
    #[must_use]
    pub const fn from_wire(schema: u32, digest: NodeDigest) -> Self {
        Self { schema, digest }
    }

    /// Returns the claimed schema ABI.
    #[must_use]
    pub const fn schema(self) -> u32 {
        self.schema
    }

    /// Returns the claimed commitment without granting trust.
    #[must_use]
    pub const fn digest(self) -> NodeDigest {
        self.digest
    }

    /// Admits this claim only against the exact checked root expected by the
    /// caller.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityMismatch`] when either the schema
    /// or commitment differs from the expected root.
    pub fn admit_against(self, expected: MerkleRoot) -> Result<MerkleRoot, ReplicationError> {
        if self.schema != expected.schema || self.digest != expected.digest {
            return Err(ReplicationError::IdentityMismatch);
        }
        Ok(expected)
    }
}

/// A checked canonical manifest identity that can bind one Merkle closure.
///
/// The fields are private so callers cannot create this proof from arbitrary
/// bytes. It is intentionally derived from a version-owned typed identity;
/// decoding and admission of the canonical manifest remains with the owner
/// of that schema.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AdmittedMerkleManifest {
    schema: u32,
    version: [u8; 32],
}

impl AdmittedMerkleManifest {
    /// Records an identity for a canonical manifest already admitted by its
    /// owner.
    #[must_use]
    pub fn from_version<T: Schema>(schema: u32, manifest: VersionObjectVersion<T>) -> Self {
        Self {
            schema,
            version: manifest.to_bytes(),
        }
    }

    /// Binds the admitted manifest identity to a checked Merkle root.
    #[must_use]
    pub const fn root(self) -> MerkleRoot {
        MerkleRoot {
            schema: self.schema,
            digest: NodeDigest(self.version),
        }
    }
}

/// A page offset within one immutable node.
///
/// Offsets are node-local and monotone.  A peer can persist this token and
/// resume after reconnect without retaining a decoded node or a full closure.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PageCursor {
    /// Number of child or leaf entries already emitted for this node.
    pub offset: u32,
}
impl PageCursor {
    /// Creates the first page cursor.
    #[must_use]
    pub const fn origin() -> Self {
        Self { offset: 0 }
    }
}

/// One immutable object row in a Merkle leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleObject {
    /// Canonical logical key bytes used for sorted Merkle ranges.
    pub key: Vec<u8>,
    /// Typed object-key identity for transfer admission. Keeping this beside
    /// the range key avoids treating a sort key as a proof of object identity.
    pub key_id: [u8; 32],
    /// Canonical object-version digest bytes.
    pub version: [u8; 32],
    /// Canonical encoded value length.
    pub len: u64,
}

/// One child commitment in a Merkle branch page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleChild {
    /// First key covered by this child.
    pub first_key: Vec<u8>,
    /// Optional exclusive upper key boundary.  A missing boundary means the
    /// child reaches the end of its parent range.
    pub end_key: Option<Vec<u8>>,
    /// Child node commitment.
    pub digest: NodeDigest,
    /// Child tree level, where leaves are level zero.
    pub level: u16,
    /// Number of leaf rows below the child.
    pub row_count: u64,
}

/// A leaf row with a stable key and complete object identity.
pub type MerkleLeafEntry = MerkleObject;

/// The bounded body of one canonical Merkle node page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MerklePageBody {
    /// Branch child commitments.
    Branch(Vec<MerkleChild>),
    /// Sorted immutable object rows.
    Leaf(Vec<MerkleLeafEntry>),
}

/// One untrusted Merkle page claim returned by a peer or local adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerklePage {
    /// Root closure being traversed.
    pub root: MerkleRoot,
    /// Node whose page is returned.
    pub node: NodeDigest,
    /// Node level, where leaves are level zero.
    pub level: u16,
    /// Cursor used to request this page.
    pub cursor: PageCursor,
    /// Cursor for the next page of this same node, if any.
    pub next: Option<PageCursor>,
    /// Bounded branch or leaf body.
    pub body: MerklePageBody,
}

impl MerklePage {
    /// Validates page shape and all allocation bounds without granting trust
    /// to its node or root digest.
    ///
    /// # Errors
    ///
    /// Returns a size, ordering, level, range, or wire error when the page
    /// exceeds the caller's bounds or cannot describe a canonical interval.
    pub fn validate(&self, max_items: usize, max_key_bytes: usize) -> Result<(), ReplicationError> {
        page::validate_window(self, max_items, max_key_bytes)?;
        match &self.body {
            MerklePageBody::Branch(children) => {
                if self.level == 0
                    || children.is_empty()
                    || children
                        .windows(2)
                        .any(|pair| pair[0].first_key >= pair[1].first_key)
                {
                    return Err(ReplicationError::Unsorted);
                }
                for child in children {
                    if child.first_key.len() > max_key_bytes
                        || child
                            .end_key
                            .as_ref()
                            .is_some_and(|key| key.len() > max_key_bytes)
                        || child.level.saturating_add(1) != self.level
                        || child
                            .end_key
                            .as_ref()
                            .is_some_and(|end| end <= &child.first_key)
                        || child.row_count == 0
                    {
                        return Err(ReplicationError::InvalidWire);
                    }
                }
                // A page continuation must expose the interval boundary of
                // its final child.  Without it, a pair whose page sizes
                // differ cannot know which side to advance and would either
                // repeat or skip an overlap at the boundary.
                if self.next.is_some()
                    && children.last().is_some_and(|child| child.end_key.is_none())
                {
                    return Err(ReplicationError::InvalidWire);
                }
            }
            MerklePageBody::Leaf(entries) => {
                if self.level != 0 || entries.windows(2).any(|pair| pair[0].key >= pair[1].key) {
                    return Err(ReplicationError::Unsorted);
                }
                for entry in entries {
                    if entry.key.len() > max_key_bytes || entry.len == 0 {
                        return Err(ReplicationError::ObjectTooLarge);
                    }
                }
            }
        }
        Ok(())
    }
}

/// A bounded page request accepted by a Merkle source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MerklePageRequest {
    /// Root closure being traversed.
    pub root: MerkleRoot,
    /// Node commitment to read.
    pub node: NodeDigest,
    /// Node-local resume cursor.
    pub cursor: PageCursor,
    /// Maximum entries the source may materialize in one response.
    pub max_items: u16,
}

/// A local or remote provider of bounded immutable Merkle pages.
pub trait MerklePageSource {
    /// Reads one bounded page.  Implementations must authenticate the page's
    /// canonical bytes before returning it, or return an error.
    ///
    /// # Errors
    ///
    /// Returns a transport, storage, or proof error when the requested page
    /// cannot be authenticated and returned within its bound.
    fn page(&mut self, request: MerklePageRequest) -> Result<MerklePage, ReplicationError>;
}

/// Work and memory bounds for one reconciliation step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconcileBudget {
    /// Maximum node pages fetched in one call to
    /// [`crate::reconcile::merkle::MerkleReconciler::step`].
    pub max_pages: usize,
    /// Maximum subtree pairs retained in the continuation frontier.
    pub max_pending: usize,
    /// Maximum changed leaf rows emitted in one delta page.
    pub max_deltas: usize,
    /// Maximum entries in one source page.
    pub max_page_items: u16,
    /// Maximum logical key length accepted from a page source.
    pub max_key_bytes: usize,
}
impl ReconcileBudget {
    /// Validates all bounds before a traversal can allocate its frontier.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::InvalidLimits`] when any bound is zero or
    /// cannot emit at least one complete changed-row comparison.
    pub fn validate(self) -> Result<(), ReplicationError> {
        if self.max_pages == 0
            || self.max_pages < 2
            || self.max_pending == 0
            || self.max_deltas == 0
            || self.max_page_items == 0
            || self.max_key_bytes == 0
            || self.max_deltas < usize::from(self.max_page_items).saturating_mul(2)
        {
            return Err(ReplicationError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SubtreePair {
    pub(super) local: NodeDigest,
    pub(super) remote: NodeDigest,
    pub(super) local_cursor: PageCursor,
    pub(super) remote_cursor: PageCursor,
    pub(super) local_index: usize,
    pub(super) remote_index: usize,
    pub(super) local_exhausted: bool,
    pub(super) remote_exhausted: bool,
    pub(super) kind: Option<NodeKind>,
    pub(super) level: Option<u16>,
    /// The key interval inherited from the branch pair that produced this
    /// work item.  Keeping it on the frontier prevents an uneven fan-out
    /// from comparing one broad child against several narrow children and
    /// emitting the same leaf twice.
    pub(super) range_start: Option<Vec<u8>>,
    pub(super) range_end: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NodeKind {
    Branch,
    Leaf,
}

impl NodeKind {
    pub(super) fn of(body: &MerklePageBody) -> Self {
        match body {
            MerklePageBody::Branch(_) => Self::Branch,
            MerklePageBody::Leaf(_) => Self::Leaf,
        }
    }
}

/// Resumable bounded traversal state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleDiffCursor {
    pub(super) local_root: MerkleRoot,
    pub(super) remote_root: MerkleRoot,
    pub(super) pending: VecDeque<SubtreePair>,
    pub(super) pages: u64,
    pub(super) complete: bool,
}
impl MerkleDiffCursor {
    /// Starts a traversal between two exact root commitments.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicationError::IdentityContext`] when the roots use
    /// different schemas or [`ReplicationError::InvalidLimits`] for an
    /// unusable traversal budget.
    pub fn new(
        local_root: MerkleRoot,
        remote_root: MerkleRoot,
        budget: ReconcileBudget,
    ) -> Result<Self, ReplicationError> {
        budget.validate()?;
        let mut pending = VecDeque::new();
        if local_root.schema() != remote_root.schema() {
            return Err(ReplicationError::IdentityContext);
        }
        let complete = local_root == remote_root;
        if !complete {
            pending.push_back(SubtreePair {
                local: local_root.digest(),
                remote: remote_root.digest(),
                local_cursor: PageCursor::origin(),
                remote_cursor: PageCursor::origin(),
                local_index: 0,
                remote_index: 0,
                local_exhausted: false,
                remote_exhausted: false,
                kind: None,
                level: None,
                range_start: None,
                range_end: None,
            });
        }
        Ok(Self {
            local_root,
            remote_root,
            pending,
            pages: 0,
            complete,
        })
    }

    /// Returns whether no unequal subtree remains.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Returns the number of pages consumed across all resumed steps.
    #[must_use]
    pub const fn pages(&self) -> u64 {
        self.pages
    }

    /// Returns the roots bound to this traversal.
    #[must_use]
    pub const fn roots(&self) -> (MerkleRoot, MerkleRoot) {
        (self.local_root, self.remote_root)
    }
}

/// One exact local/remote object change emitted by a Merkle leaf comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleDelta {
    /// Stable logical object key.
    pub key: Vec<u8>,
    /// Local leaf value, if present.
    pub local: Option<MerkleObject>,
    /// Remote leaf value, if present.
    pub remote: Option<MerkleObject>,
}

/// One bounded changed-only page and its continuation cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleDeltaPage {
    /// Changed object rows in canonical key order within this page.
    pub deltas: Vec<MerkleDelta>,
    /// Resumable traversal state, if work remains.
    pub next: Option<MerkleDiffCursor>,
}
