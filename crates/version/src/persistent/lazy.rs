//! Lazy authenticated reopening and path copy over persisted canonical nodes.

use crate::tree::{anchored_cut_points_children, anchored_cut_points_items};
use crate::{
    CanonicalNode, CanonicalRelation, CheckedCanonicalRoot, ChildCommitment, CommittedChild,
    DEFAULT_CUT_POLICY, IdContext, MapChange, NodeError, PersistentTree, TreeChange, TreeError,
    UntrustedId, canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
use std::{borrow::Borrow, cell::Cell, mem::size_of};

use super::TreeNodeLoader;

#[path = "lazy/helpers.rs"]
mod helpers;
mod types;
use helpers::{
    OverlayLoader, child_claim, child_node_from_result, committed_child, leaf_probe_cuts,
    make_branches, make_leaves,
};
pub use types::{LazyPreparedUpdate, LazyTreePage, LazyTreeWork, PersistedTreeRoot};

const NODE_HEADER_AND_BODY_FRAME_BYTES: usize = 32;
const LENGTH_FRAME_BYTES: usize = 8;
const BRANCH_COMMITMENT_BYTES: usize = 40;
const MAP_NODE_LINK_BYTES: usize = 4 * size_of::<usize>();

/// Error while opening or path copying a lazily loaded canonical tree.
#[derive(Debug, Eq, PartialEq)]
pub enum LazyTreeError<E> {
    /// The loader could not provide a requested node.
    Load(E),
    /// A loaded node failed canonical grammar or path validation.
    Node(NodeError),
    /// A removal targeted a key that was absent.
    MissingKey,
    /// An insertion targeted a key already present in the tree.
    DuplicateKey,
    /// The bounded update could not fit its conservative metadata envelope.
    MetadataBudgetExceeded {
        /// The conservative charge required before preparation.
        required_bytes: usize,
        /// The caller's maximum metadata budget.
        max_bytes: usize,
    },
}

impl<E: std::fmt::Display> std::fmt::Display for LazyTreeError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(error) => write!(f, "lazy tree node load failed: {error}"),
            Self::Node(error) => write!(f, "lazy tree node rejected: {error}"),
            Self::MissingKey => f.write_str("lazy tree change targeted an absent key"),
            Self::DuplicateKey => f.write_str("lazy tree insertion targeted an existing key"),
            Self::MetadataBudgetExceeded {
                required_bytes,
                max_bytes,
            } => write!(
                f,
                "lazy tree metadata budget exceeded: {required_bytes} bytes required, {max_bytes} allowed"
            ),
        }
    }
}
impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for LazyTreeError<E> {}

/// Caller-verified encoded-width and canonical fanout bounds for one relation.
///
/// Bounded preparation re-encodes every edit key/value and rejects values
/// outside this shape before path-copy work begins. The store composer uses
/// the fixed-width manifest relation shape; general `LazyTree` callers can
/// continue using the existing unbounded API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LazyTreeMetadataShape {
    /// Maximum canonical encoded key bytes.
    pub max_key_bytes: usize,
    /// Maximum canonical encoded value bytes.
    pub max_value_bytes: usize,
    /// Minimum entries in a cut before an anchored boundary may be selected.
    pub min_entries_per_node: usize,
    /// Hard maximum entries in a canonical leaf or branch.
    pub max_entries_per_node: usize,
    /// Maximum adjacent sibling nodes that can be included in one spill region.
    pub max_spill_nodes_per_level: usize,
    /// Conservative maximum number of levels in the target tree.
    pub max_tree_levels: usize,
}

impl LazyTreeMetadataShape {
    /// Creates an explicit encoded-width and canonical-tree shape bound.
    #[must_use]
    pub const fn new(
        max_key_bytes: usize,
        max_value_bytes: usize,
        min_entries_per_node: usize,
        max_entries_per_node: usize,
        max_spill_nodes_per_level: usize,
        max_tree_levels: usize,
    ) -> Self {
        Self {
            max_key_bytes,
            max_value_bytes,
            min_entries_per_node,
            max_entries_per_node,
            max_spill_nodes_per_level,
            max_tree_levels,
        }
    }

    fn valid(self) -> bool {
        let valid_counts = self.min_entries_per_node > 0
            && self.min_entries_per_node <= self.max_entries_per_node
            && self.max_entries_per_node > 0
            && self.max_tree_levels > 0
            && self.max_node_bytes().is_some_and(|bytes| {
                bytes <= DEFAULT_CUT_POLICY.max_encoded_bytes()
                    && self.max_entries_per_node <= usize::from(DEFAULT_CUT_POLICY.max_entries)
            });
        let Some(leaf_min) = entries_before_anchor_limit(
            self.min_entries_per_node,
            LENGTH_FRAME_BYTES
                .checked_add(self.max_key_bytes)
                .and_then(|bytes| bytes.checked_add(LENGTH_FRAME_BYTES))
                .and_then(|bytes| bytes.checked_add(self.max_value_bytes))
                .unwrap_or(usize::MAX),
        ) else {
            return false;
        };
        let Some(branch_min) = entries_before_anchor_limit(
            self.min_entries_per_node,
            LENGTH_FRAME_BYTES
                .checked_add(self.max_key_bytes)
                .and_then(|bytes| bytes.checked_add(BRANCH_COMMITMENT_BYTES))
                .and_then(|bytes| bytes.checked_add(size_of::<u64>()))
                .unwrap_or(usize::MAX),
        ) else {
            return false;
        };
        let required_spill = self
            .max_entries_per_node
            .checked_div(leaf_min.min(branch_min))
            .and_then(|count| count.checked_add(2));
        valid_counts
            && required_spill.is_some_and(|required| self.max_spill_nodes_per_level >= required)
    }

    fn max_node_bytes(self) -> Option<usize> {
        let leaf_entry = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(LENGTH_FRAME_BYTES)?
            .checked_add(self.max_value_bytes)?;
        let branch_entry = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(BRANCH_COMMITMENT_BYTES)?
            .checked_add(size_of::<u64>())?;
        let entry = leaf_entry.max(branch_entry);
        let by_entry_count = NODE_HEADER_AND_BODY_FRAME_BYTES
            .checked_add(self.max_entries_per_node.checked_mul(entry)?)?;
        // The cut policy has an independent hard encoded-byte ceiling. A
        // shape may declare the policy's full entry-count maximum even when
        // that many maximum-width branch entries cannot fit in one node; the
        // actual encoded node is still bounded by this canonical ABI ceiling.
        Some(by_entry_count.min(DEFAULT_CUT_POLICY.max_encoded_bytes()))
    }

    fn minimum_fanout(self) -> Option<usize> {
        let leaf = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(LENGTH_FRAME_BYTES)?
            .checked_add(self.max_value_bytes)?;
        let branch = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(BRANCH_COMMITMENT_BYTES)?
            .checked_add(size_of::<u64>())?;
        Some(
            entries_before_anchor_limit(self.min_entries_per_node, leaf)?.min(
                entries_before_anchor_limit(self.min_entries_per_node, branch)?,
            ),
        )
    }

    fn update_metadata_bytes<R: CanonicalRelation>(self, change_count: usize) -> Option<usize> {
        let node_bytes = self.max_node_bytes()?;
        // Charge the caller's edit structs plus the before/after records that
        // can be returned. Accumulated path copies are measured from their
        // actual encoded sizes before each node is retained; charging every
        // possible maximum-sized node for every edit rejects small realistic
        // updates by several orders of magnitude.
        let per_change = size_of::<TreeChange<R>>()
            .checked_add(size_of::<MapChange<R>>())?
            .checked_add(size_of::<R::Key>())?
            .checked_mul(2)?
            .checked_add(size_of::<R::Value>().checked_mul(2)?)?
            .checked_add(self.max_key_bytes.checked_mul(2)?)?
            .checked_add(self.max_value_bytes.checked_mul(3)?)?;
        let changes = change_count.checked_mul(per_change)?;

        // A single edit can spill across several sibling nodes at any tree
        // level. Bound recursive working sets by the declared spill factor,
        // canonical fanout and encoded widths. The prior-step frontier and
        // overlay are added dynamically as each step is admitted.
        let spill_entries = self
            .max_entries_per_node
            .checked_mul(self.max_spill_nodes_per_level)?;
        let row_scratch = size_of::<R::Key>()
            .checked_mul(2)?
            .checked_add(size_of::<R::Value>())?
            .checked_add(self.max_key_bytes.checked_mul(2)?)?
            .checked_add(self.max_value_bytes)?;
        let branch_scratch = size_of::<CommittedChild<R>>()
            .checked_add(size_of::<R::Key>())?
            .checked_add(self.max_key_bytes)?;
        let scratch_per_level = spill_entries
            .checked_mul(row_scratch.max(branch_scratch))?
            .checked_add(self.max_spill_nodes_per_level.checked_mul(node_bytes)?)?
            .checked_add(node_bytes)?;
        let scratch = self.max_tree_levels.checked_mul(scratch_per_level)?;
        changes.checked_add(scratch)
    }

    fn bulk_metadata_bytes<R: CanonicalRelation>(self, member_count: usize) -> Option<usize> {
        let node_bytes = self.max_node_bytes()?;
        let key_and_value = size_of::<R::Key>()
            .checked_add(size_of::<R::Value>())?
            .checked_add(self.max_key_bytes)?
            .checked_add(self.max_value_bytes)?;
        let leaf_entry = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(LENGTH_FRAME_BYTES)?
            .checked_add(self.max_value_bytes)?;
        let branch_entry = LENGTH_FRAME_BYTES
            .checked_add(self.max_key_bytes)?
            .checked_add(BRANCH_COMMITMENT_BYTES)?
            .checked_add(size_of::<u64>())?;
        let leaf_min = entries_before_anchor_limit(self.min_entries_per_node, leaf_entry)?;
        let branch_min = entries_before_anchor_limit(self.min_entries_per_node, branch_entry)?;
        let (node_count, encoded_bytes, branch_entries, levels) =
            bulk_tree_bound(member_count, leaf_min, branch_min, leaf_entry, branch_entry)?;
        if levels > self.max_tree_levels {
            return None;
        }

        let rows = member_count.checked_mul(key_and_value)?;
        let map_changes = member_count.checked_mul(
            size_of::<MapChange<R>>()
                .checked_add(self.max_key_bytes)?
                .checked_add(self.max_value_bytes)?,
        )?;
        let nodes = node_count
            .checked_mul(size_of::<CanonicalNode<R>>().checked_mul(2)?)?
            .checked_add(encoded_bytes.checked_mul(2)?)?
            .checked_add(
                branch_entries.checked_mul(
                    size_of::<CommittedChild<R>>()
                        .checked_add(size_of::<R::Key>())?
                        .checked_add(self.max_key_bytes)?,
                )?,
            )?;
        let scratch = self.max_tree_levels.checked_mul(
            self.max_entries_per_node
                .checked_mul(key_and_value)?
                .checked_add(node_bytes)?,
        )?;
        rows.checked_add(map_changes)?
            .checked_add(nodes)?
            .checked_add(scratch)
    }
}

/// Conservative memory envelope for one bounded multi-key lazy update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LazyTreeUpdateBudget {
    /// Maximum number of changes admitted by this update.
    pub max_changes: usize,
    /// Maximum conservative metadata charge for the whole update.
    pub max_metadata_bytes: usize,
    /// Relation-specific encoded width and tree-shape limits.
    pub shape: LazyTreeMetadataShape,
}

impl LazyTreeUpdateBudget {
    /// Creates one explicit bounded lazy-update policy.
    #[must_use]
    pub const fn new(
        max_changes: usize,
        max_metadata_bytes: usize,
        shape: LazyTreeMetadataShape,
    ) -> Self {
        Self {
            max_changes,
            max_metadata_bytes,
            shape,
        }
    }

    /// Returns the conservative path-copy charge for a relation and change count.
    #[must_use]
    pub fn update_charge<R: CanonicalRelation>(self, change_count: usize) -> Option<usize> {
        self.shape.update_metadata_bytes::<R>(change_count)
    }

    /// Returns the conservative genesis bulk-build charge for a relation and member count.
    #[must_use]
    pub fn bulk_charge<R: CanonicalRelation>(self, member_count: usize) -> Option<usize> {
        self.shape.bulk_metadata_bytes::<R>(member_count)
    }
}

fn entries_before_anchor_limit(
    configured_minimum: usize,
    encoded_entry_bytes: usize,
) -> Option<usize> {
    // The cut policy may select byte anchors once one quarter of the node's
    // maximum body is populated, even before its entry-count minimum.
    let minimum_body = DEFAULT_CUT_POLICY
        .max_encoded_bytes()
        .checked_div(4)?
        .checked_sub(NODE_HEADER_AND_BODY_FRAME_BYTES)?;
    let available = minimum_body;
    let byte_limited = available.checked_div(encoded_entry_bytes)?;
    Some(configured_minimum.min(byte_limited.max(1)))
}

fn tree_height(member_count: usize, minimum_fanout: usize) -> Option<usize> {
    if minimum_fanout == 0 {
        return None;
    }
    let mut nodes = member_count.max(1).div_ceil(minimum_fanout);
    let mut height = 1usize;
    while nodes > 1 {
        nodes = nodes.div_ceil(minimum_fanout);
        height = height.checked_add(1)?;
    }
    Some(height)
}

fn bulk_tree_bound(
    member_count: usize,
    leaf_minimum: usize,
    branch_minimum: usize,
    leaf_entry_bytes: usize,
    branch_entry_bytes: usize,
) -> Option<(usize, usize, usize, usize)> {
    if member_count == 0 {
        return Some((1, NODE_HEADER_AND_BODY_FRAME_BYTES, 0, 1));
    }
    let mut level_nodes = member_count.div_ceil(leaf_minimum);
    let mut node_count = level_nodes;
    let mut encoded_bytes = member_count
        .checked_mul(leaf_entry_bytes)?
        .checked_add(level_nodes.checked_mul(NODE_HEADER_AND_BODY_FRAME_BYTES)?)?;
    let mut branch_entries = 0usize;
    let mut levels = 1usize;
    while level_nodes > 1 {
        let parent_nodes = level_nodes
            .div_ceil(branch_minimum)
            .min(level_nodes.saturating_sub(1));
        branch_entries = branch_entries.checked_add(level_nodes)?;
        encoded_bytes = encoded_bytes
            .checked_add(level_nodes.checked_mul(branch_entry_bytes)?)?
            .checked_add(parent_nodes.checked_mul(NODE_HEADER_AND_BODY_FRAME_BYTES)?)?;
        node_count = node_count.checked_add(parent_nodes)?;
        level_nodes = parent_nodes;
        levels = levels.checked_add(1)?;
    }
    Some((node_count, encoded_bytes, branch_entries, levels))
}

fn validate_node_shape<R: CanonicalRelation>(
    node: &CheckedCanonicalRoot<R>,
    shape: LazyTreeMetadataShape,
) -> Result<(), NodeError> {
    if usize::from(node.node().level()).saturating_add(1) > shape.max_tree_levels {
        return Err(NodeError::OversizedNode);
    }
    if node.node().as_bytes().len() > shape.max_node_bytes().ok_or(NodeError::OversizedNode)? {
        return Err(NodeError::OversizedNode);
    }
    let mut encoded = Vec::new();
    if node.node().level() == 0 {
        for (key, value) in node.leaf_entries()? {
            encoded.clear();
            R::encode_key(&key, &mut encoded);
            if encoded.len() > shape.max_key_bytes {
                return Err(NodeError::OversizedNode);
            }
            encoded.clear();
            R::encode_value(&value, &mut encoded);
            if encoded.len() > shape.max_value_bytes {
                return Err(NodeError::OversizedNode);
            }
        }
    } else {
        for child in node.child_summaries()? {
            encoded.clear();
            R::encode_key(&child.first_key, &mut encoded);
            if encoded.len() > shape.max_key_bytes {
                return Err(NodeError::OversizedNode);
            }
        }
    }
    Ok(())
}

/// Where a lazy page begins.
#[derive(Clone, Copy)]
enum PageStart<'a, K> {
    /// The first key in this subtree.
    Beginning,
    /// The first key strictly after this one.
    After(&'a K),
    /// The first key greater than or equal to this one.
    From(&'a K),
}

struct RewriteResult<R: CanonicalRelation> {
    roots: Vec<CanonicalNode<R>>,
    changed: Vec<CanonicalNode<R>>,
    split_nodes: usize,
    removed_entries: usize,
    change: MapChange<R>,
}

struct BoundedUpdateCharge {
    budget_bytes: usize,
    base_bytes: usize,
    peak_bytes: usize,
    retained_frontier_bytes: usize,
    retained_overlay_bytes: usize,
    retained_root_bytes: usize,
}

fn step_metadata_bytes<R: CanonicalRelation>(
    nodes: &[CanonicalNode<R>],
) -> Option<(usize, usize, usize)> {
    let mut in_flight = size_of::<CanonicalNode<R>>().checked_mul(nodes.len())?;
    let mut frontier = in_flight.checked_mul(2)?;
    let mut overlay = size_of::<CheckedCanonicalRoot<R>>()
        .checked_add(size_of::<[u8; crate::ID_BYTES]>())?
        .checked_add(MAP_NODE_LINK_BYTES)?
        .checked_mul(nodes.len())?;
    for node in nodes {
        let capacity_bound = node.as_bytes().len().checked_mul(2)?;
        in_flight = in_flight.checked_add(capacity_bound)?;
        frontier = frontier.checked_add(capacity_bound)?;
        overlay = overlay.checked_add(capacity_bound)?;
    }
    Some((in_flight, frontier, overlay))
}

fn root_metadata_bytes<R: CanonicalRelation>(root: &CheckedCanonicalRoot<R>) -> Option<usize> {
    size_of::<CheckedCanonicalRoot<R>>().checked_add(root.node().as_bytes().len().checked_mul(2)?)
}

/// A lazily opened canonical root with a store supplied node loader.
pub struct LazyTree<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> {
    root: CheckedCanonicalRoot<R>,
    loader: &'a L,
    loaded_nodes: Cell<usize>,
    bounded_shape: Cell<Option<LazyTreeMetadataShape>>,
}
impl<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> LazyTree<'a, R, L> {
    /// Opens a root through the loader after preserving its typed claim.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::Load`] for loader failures.
    pub fn open(loader: &'a L, claim: UntrustedId<R>) -> Result<Self, LazyTreeError<L::Error>> {
        // A loader is allowed to use the claim as its storage lookup key, but
        // a matching digest alone is not enough to establish the relation
        // identity.  Reject a caller-supplied object/delta/foreign-schema
        // context before invoking storage so the typed `R` marker is also
        // enforced at this admission boundary.
        if claim.context() != IdContext::relation::<R>() {
            return Err(LazyTreeError::Node(NodeError::SchemaMismatch));
        }
        let root = loader.load(claim).map_err(LazyTreeError::Load)?;
        if root.root().as_bytes() != claim.as_bytes() {
            return Err(LazyTreeError::Node(NodeError::AnchorMismatch));
        }
        Ok(Self {
            root,
            loader,
            loaded_nodes: Cell::new(1),
            bounded_shape: Cell::new(None),
        })
    }

    /// Opens an already admitted persisted root without fetching its node.
    ///
    /// This is O(1) in relation rows and is useful when closure admission has
    /// already retained the root evidence.
    #[must_use]
    pub fn from_admitted(loader: &'a L, root: PersistedTreeRoot<R>) -> Self {
        Self {
            root: root.evidence,
            loader,
            loaded_nodes: Cell::new(0),
            bounded_shape: Cell::new(None),
        }
    }

    /// Returns the checked root loaded at open time.
    #[must_use]
    pub const fn root(&self) -> &CheckedCanonicalRoot<R> {
        &self.root
    }

    /// Returns an owned checked root handle for a later loader handoff.
    #[must_use]
    pub fn root_handle(&self) -> PersistedTreeRoot<R> {
        PersistedTreeRoot {
            evidence: self.root.clone(),
        }
    }

    /// Looks up one key by loading only its authenticated root-to-leaf path.
    ///
    /// # Errors
    /// Returns a loader or canonical-node error if an affected path node
    /// cannot be fetched or admitted.
    pub fn lookup<Q: ?Sized + Ord>(
        &self,
        key: &Q,
    ) -> Result<Option<R::Value>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        let mut node = self.root.clone();
        loop {
            if node.node().level() == 0 {
                let entries = node.leaf_entries().map_err(LazyTreeError::Node)?;
                return Ok(entries
                    .binary_search_by(|(candidate, _)| candidate.borrow().cmp(key))
                    .ok()
                    .map(|index| entries[index].1.clone()));
            }
            let summaries = node.child_summaries().map_err(LazyTreeError::Node)?;
            if summaries.is_empty() {
                return Err(LazyTreeError::Node(NodeError::InvalidBranch));
            }
            let index = summaries
                .partition_point(|child| child.first_key.borrow() <= key)
                .saturating_sub(1);
            let claim = child_claim(&summaries[index]).map_err(LazyTreeError::Node)?;
            node = self.load(claim)?;
        }
    }

    /// Reads at most `limit` rows after an optional canonical key.
    ///
    /// The cursor seeks to the first leaf that can hold a later key, then
    /// reads forward. Untouched left siblings stay unloaded, so one page is
    /// O(tree height + page size) authenticated work.
    ///
    /// # Errors
    /// Returns a loader or canonical-node error if an affected path node
    /// cannot be fetched or admitted.
    pub fn page(
        &self,
        after: Option<&R::Key>,
        limit: usize,
    ) -> Result<LazyTreePage<R>, LazyTreeError<L::Error>> {
        if limit == 0 {
            return Ok(LazyTreePage {
                entries: Vec::new(),
                next: None,
            });
        }
        let mut entries = Vec::with_capacity(limit.min(256));
        let mut pending = Vec::new();
        let start = match after {
            Some(after) => PageStart::After(after),
            None => PageStart::Beginning,
        };
        self.take_page(&self.root, start, limit, &mut entries, &mut pending)?;
        self.finish_page(limit, &mut entries, &mut pending)
    }

    /// Reads at most `limit` rows starting at `start`, including `start`.
    ///
    /// The same seek as [`Self::page`] applies: left siblings of the first
    /// matching leaf stay unloaded.
    ///
    /// # Errors
    /// Returns a loader or canonical-node error if an affected path node
    /// cannot be fetched or admitted.
    pub fn page_from(
        &self,
        start: &R::Key,
        limit: usize,
    ) -> Result<LazyTreePage<R>, LazyTreeError<L::Error>> {
        if limit == 0 {
            return Ok(LazyTreePage {
                entries: Vec::new(),
                next: None,
            });
        }
        let mut entries = Vec::with_capacity(limit.min(256));
        let mut pending = Vec::new();
        self.take_page(
            &self.root,
            PageStart::From(start),
            limit,
            &mut entries,
            &mut pending,
        )?;
        self.finish_page(limit, &mut entries, &mut pending)
    }

    fn finish_page(
        &self,
        limit: usize,
        entries: &mut Vec<(R::Key, R::Value)>,
        pending: &mut Vec<UntrustedId<R>>,
    ) -> Result<LazyTreePage<R>, LazyTreeError<L::Error>> {
        while entries.len() < limit {
            let Some(claim) = pending.pop() else {
                break;
            };
            let node = self.load(claim)?;
            self.take_page(&node, PageStart::Beginning, limit, entries, pending)?;
        }
        let next = (entries.len() == limit)
            .then(|| entries.last().map(|(key, _)| key.clone()))
            .flatten();
        Ok(LazyTreePage {
            entries: std::mem::take(entries),
            next,
        })
    }

    /// Collects rows from `node`, queueing later siblings without loading them.
    ///
    /// Right siblings are entirely past the cursor, so the caller resumes them
    /// from their first key.
    fn take_page<'k>(
        &self,
        node: &CheckedCanonicalRoot<R>,
        start: PageStart<'k, R::Key>,
        limit: usize,
        entries: &mut Vec<(R::Key, R::Value)>,
        pending: &mut Vec<UntrustedId<R>>,
    ) -> Result<(), LazyTreeError<L::Error>> {
        if entries.len() == limit {
            return Ok(());
        }
        if node.node().level() == 0 {
            let leaf = node.leaf_entries().map_err(LazyTreeError::Node)?;
            for (key, value) in leaf {
                let skip = match start {
                    PageStart::Beginning => false,
                    PageStart::After(after) => key <= *after,
                    PageStart::From(from) => key < *from,
                };
                if skip {
                    continue;
                }
                if entries.len() == limit {
                    return Ok(());
                }
                entries.push((key.clone(), value.clone()));
            }
            return Ok(());
        }
        let children = node.child_summaries().map_err(LazyTreeError::Node)?;
        if children.is_empty() {
            return Ok(());
        }
        let probe = match start {
            PageStart::Beginning => None,
            PageStart::After(after) => Some(after),
            PageStart::From(from) => Some(from),
        };
        let index = match probe {
            None => 0,
            Some(probe) => children
                .partition_point(|child| child.first_key.borrow() <= probe)
                .saturating_sub(1),
        };
        for child in children[index + 1..].iter().rev() {
            pending.push(child_claim(child).map_err(LazyTreeError::Node)?);
        }
        let child = self.load(child_claim(&children[index]).map_err(LazyTreeError::Node)?)?;
        self.take_page(&child, start, limit, entries, pending)
    }

    /// Replaces an existing value by loading its root-to-leaf path.
    ///
    /// # Errors
    /// Returns an error if the key is absent, a path node fails admission, or
    /// the loader cannot fetch an affected node.
    pub fn prepare_replace<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        self.finish(key, Some(value), None, false)
    }

    /// Inserts a key, splitting affected leaves and propagating branch splits.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::DuplicateKey`] for an existing key, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_insert(
        &self,
        key: &R::Key,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.finish(key, Some(value), Some(key), true)
    }

    /// Applies one replacement or removal and propagates canonical rebalances.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::MissingKey`] when the key is absent, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_change(
        &self,
        key: &R::Key,
        after: Option<R::Value>,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.finish(key, after, None, false)
    }

    /// Removes an existing key, shrinking an empty or single-child root.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::MissingKey`] when the key is absent, or a
    /// loader/admission error for an affected persisted node.
    pub fn prepare_remove(
        &self,
        key: &R::Key,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.prepare_change(key, None)
    }

    /// Preflights and prepares an ordered batch under a conservative typed
    /// metadata envelope. The existing [`Self::prepare_update`] remains
    /// unbounded for callers whose memory policy is managed elsewhere.
    ///
    /// Every edit key/value is encoded before path-copy work and checked
    /// against `budget.shape`. The preflight covers the edit records and a
    /// conservative single-step spill/scratch envelope. The cumulative
    /// frontier, overlay and in-flight copies are then charged using their
    /// encoded node sizes before any step is retained. Genesis bulk building
    /// is preflighted across all rows and branch levels before construction.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::MetadataBudgetExceeded`] when the preflight
    /// charge exceeds the budget or a loaded/output node violates its declared
    /// relation shape. Other load and canonical errors are forwarded.
    pub fn prepare_update_bounded(
        &self,
        changes: &[TreeChange<R>],
        budget: LazyTreeUpdateBudget,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(LazyTreeError::Node(NodeError::UnsortedOrDuplicate));
        }
        let root_levels = usize::from(self.root.node().level()).saturating_add(1);
        let shape = budget.shape;
        if shape.valid() {
            validate_node_shape(&self.root, shape).map_err(LazyTreeError::Node)?;
        }
        let additions = changes
            .iter()
            .filter(|change| change.after.is_some())
            .count();
        let target_rows = usize::try_from(self.root.row_count())
            .ok()
            .and_then(|rows| rows.checked_add(additions));
        let required_levels = shape
            .minimum_fanout()
            .and_then(|fanout| tree_height(target_rows?, fanout));
        let charge = if changes.len() > budget.max_changes
            || !shape.valid()
            || shape.max_tree_levels < root_levels
            || required_levels.is_none_or(|levels| shape.max_tree_levels < levels)
        {
            None
        } else {
            for change in changes {
                let mut encoded = Vec::new();
                R::encode_key(&change.key, &mut encoded);
                if encoded.len() > shape.max_key_bytes {
                    return Err(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: encoded.len(),
                        max_bytes: shape.max_key_bytes,
                    });
                }
                encoded.clear();
                if let Some(value) = &change.after {
                    R::encode_value(value, &mut encoded);
                }
                if encoded.len() > shape.max_value_bytes {
                    return Err(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: encoded.len(),
                        max_bytes: shape.max_value_bytes,
                    });
                }
            }
            if self.root.row_count() == 0 && changes.iter().all(|change| change.after.is_some()) {
                shape.bulk_metadata_bytes::<R>(changes.len())
            } else {
                shape.update_metadata_bytes::<R>(changes.len())
            }
        }
        .ok_or(LazyTreeError::MetadataBudgetExceeded {
            required_bytes: usize::MAX,
            max_bytes: budget.max_metadata_bytes,
        })?;
        if charge > budget.max_metadata_bytes {
            return Err(LazyTreeError::MetadataBudgetExceeded {
                required_bytes: charge,
                max_bytes: budget.max_metadata_bytes,
            });
        }
        let retained_root_bytes =
            root_metadata_bytes(&self.root).ok_or(LazyTreeError::MetadataBudgetExceeded {
                required_bytes: usize::MAX,
                max_bytes: budget.max_metadata_bytes,
            })?;
        let initial_peak = charge.checked_add(retained_root_bytes).ok_or(
            LazyTreeError::MetadataBudgetExceeded {
                required_bytes: usize::MAX,
                max_bytes: budget.max_metadata_bytes,
            },
        )?;
        if initial_peak > budget.max_metadata_bytes {
            return Err(LazyTreeError::MetadataBudgetExceeded {
                required_bytes: initial_peak,
                max_bytes: budget.max_metadata_bytes,
            });
        }
        let previous_shape = self.bounded_shape.replace(Some(shape));
        let bounded = BoundedUpdateCharge {
            budget_bytes: budget.max_metadata_bytes,
            base_bytes: charge,
            peak_bytes: initial_peak,
            retained_frontier_bytes: 0,
            retained_overlay_bytes: 0,
            retained_root_bytes,
        };
        let prepared = self.prepare_update_inner(changes, Some(bounded));
        self.bounded_shape.set(previous_shape);
        prepared
    }

    /// Prepares an ordered set of independent key changes against one exact
    /// persisted root.
    ///
    /// Each step reuses the prior step's immutable frontier through an
    /// in-memory checked-node overlay. Unchanged descendants continue to load
    /// from the original store, so a batch retains O(changed frontier) memory
    /// and never hydrates the full relation. The resulting delta is rooted at
    /// the original source and contains one canonical before/after entry per
    /// effective key.
    ///
    /// # Errors
    /// Returns [`LazyTreeError::Node`] when keys are not strictly ordered or a
    /// constructed frontier fails its own canonical admission, and forwards
    /// lookup/path-copy errors from the underlying loader.
    pub fn prepare_update(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.prepare_update_inner(changes, None)
    }

    fn prepare_update_inner(
        &self,
        changes: &[TreeChange<R>],
        mut bounded: Option<BoundedUpdateCharge>,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        if changes
            .windows(2)
            .any(|window| window[0].key >= window[1].key)
        {
            return Err(LazyTreeError::Node(NodeError::UnsortedOrDuplicate));
        }

        // A first ingest is a construction, not a sequence of insertions. Build
        // its canonical tree once from the already sorted relation instead of
        // repeatedly path-copying an ever-growing temporary frontier. Besides
        // removing O(n log n) hashing and admission, this emits every immutable
        // node exactly once in child-to-root storage order.
        if self.root.row_count() == 0 && changes.iter().all(|change| change.after.is_some()) {
            let mut prepared = self.prepare_empty_bulk(changes)?;
            if let Some(bounded) = &mut bounded {
                // The bulk charge already includes the input rows, builder
                // levels, node closure and cloned output frontier.
                bounded.peak_bytes = bounded.base_bytes;
                if bounded.peak_bytes > bounded.budget_bytes {
                    return Err(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: bounded.peak_bytes,
                        max_bytes: bounded.budget_bytes,
                    });
                }
                prepared.work.peak_metadata_bytes = bounded.peak_bytes;
            }
            return Ok(prepared);
        }

        let mut overlay = OverlayLoader::<R, L>::new(self.loader);
        let mut root = self.root_handle();
        let mut frontier = Vec::new();
        let mut effective = Vec::with_capacity(changes.len());
        let mut work = LazyTreeWork::default();

        for change in changes {
            let tree = LazyTree::from_admitted(&overlay, root);
            tree.bounded_shape.set(self.bounded_shape.get());
            let before = tree.lookup(&change.key)?;
            if before == change.after {
                work.loaded_nodes = work.loaded_nodes.saturating_add(tree.loaded_nodes.get());
                root = tree.root_handle();
                continue;
            }
            let update = match (&before, &change.after) {
                (None, Some(after)) => tree.prepare_insert(&change.key, after.clone())?,
                (Some(_), after) => tree.prepare_change(&change.key, after.clone())?,
                (None, None) => {
                    root = tree.root_handle();
                    continue;
                }
            };
            let step = update.work();
            work.loaded_nodes = work.loaded_nodes.saturating_add(tree.loaded_nodes.get());
            work.rebuilt_nodes = work.rebuilt_nodes.saturating_add(step.rebuilt_nodes);
            work.split_nodes = work.split_nodes.saturating_add(step.split_nodes);
            work.removed_entries = work.removed_entries.saturating_add(step.removed_entries);
            work.emitted_bytes = work.emitted_bytes.saturating_add(step.emitted_bytes);
            if let Some(bounded) = &mut bounded {
                let max_node_bytes = self
                    .bounded_shape
                    .get()
                    .and_then(LazyTreeMetadataShape::max_node_bytes)
                    .ok_or(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    })?;
                if update
                    .changed_nodes()
                    .iter()
                    .any(|node| node.as_bytes().len() > max_node_bytes)
                {
                    return Err(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    });
                }
                let (step_nodes, frontier_copy, overlay_copy) = step_metadata_bytes::<R>(
                    update.changed_nodes(),
                )
                .ok_or(LazyTreeError::MetadataBudgetExceeded {
                    required_bytes: usize::MAX,
                    max_bytes: bounded.budget_bytes,
                })?;
                let next_frontier = bounded
                    .retained_frontier_bytes
                    .checked_add(frontier_copy)
                    .ok_or(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    })?;
                let next_overlay = bounded
                    .retained_overlay_bytes
                    .checked_add(overlay_copy)
                    .ok_or(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    })?;
                let root_copy = root_metadata_bytes(update.target()).ok_or(
                    LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    },
                )?;
                let required = bounded
                    .base_bytes
                    .checked_add(bounded.retained_frontier_bytes)
                    .and_then(|bytes| bytes.checked_add(bounded.retained_overlay_bytes))
                    .and_then(|bytes| bytes.checked_add(bounded.retained_root_bytes))
                    .and_then(|bytes| bytes.checked_add(step_nodes))
                    .and_then(|bytes| bytes.checked_add(frontier_copy))
                    .and_then(|bytes| bytes.checked_add(overlay_copy))
                    .and_then(|bytes| bytes.checked_add(root_copy.checked_mul(2)?))
                    .ok_or(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: usize::MAX,
                        max_bytes: bounded.budget_bytes,
                    })?;
                if required > bounded.budget_bytes {
                    return Err(LazyTreeError::MetadataBudgetExceeded {
                        required_bytes: required,
                        max_bytes: bounded.budget_bytes,
                    });
                }
                bounded.peak_bytes = bounded.peak_bytes.max(required);
                bounded.retained_frontier_bytes = next_frontier;
                bounded.retained_overlay_bytes = next_overlay;
                bounded.retained_root_bytes = root_copy;
            }
            effective.extend(update.changes.iter().cloned());
            for node in update.changed_nodes() {
                overlay.insert(node)?;
                frontier.push(node.clone());
            }
            root = update.target_root();
        }

        let mut work = work;
        if let Some(bounded) = bounded {
            work.peak_metadata_bytes = bounded.peak_bytes;
        }
        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: root.evidence,
            changed: frontier,
            work,
            changes: effective,
        })
    }

    fn prepare_empty_bulk(
        &self,
        changes: &[TreeChange<R>],
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        let items = changes
            .iter()
            .map(|change| {
                change
                    .after
                    .clone()
                    .map(|value| (change.key.clone(), value))
                    .ok_or(LazyTreeError::Node(NodeError::InvalidBranch))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (tree, tree_work) = PersistentTree::from_sorted_items_with_work(&items)
            .map_err(|error| LazyTreeError::Node(map_eager_tree_error(error)))?;
        let target_node = tree.root().clone();
        let mut changed_nodes = tree
            .node_closure()
            .map(|node| node.canonical().clone())
            .collect::<Vec<_>>();
        changed_nodes.reverse();
        let changes = changes
            .iter()
            .map(|change| MapChange {
                key: change.key.clone(),
                before: None,
                after: change.after.clone(),
            })
            .collect();
        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: CheckedCanonicalRoot::from_parts(target_node.commitment(), target_node),
            changed: changed_nodes,
            work: LazyTreeWork {
                loaded_nodes: 0,
                rebuilt_nodes: tree_work.nodes,
                split_nodes: 0,
                removed_entries: 0,
                emitted_bytes: tree_work.encoded_bytes,
                peak_metadata_bytes: 0,
            },
            changes,
        })
    }

    fn load(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<CheckedCanonicalRoot<R>, LazyTreeError<L::Error>> {
        if claim.context() != IdContext::relation::<R>() {
            return Err(LazyTreeError::Node(NodeError::SchemaMismatch));
        }
        let node = self.loader.load(claim).map_err(LazyTreeError::Load)?;
        self.loaded_nodes
            .set(self.loaded_nodes.get().saturating_add(1));
        if node.root().as_bytes() != claim.as_bytes() {
            return Err(LazyTreeError::Node(NodeError::AnchorMismatch));
        }
        if let Some(shape) = self.bounded_shape.get() {
            validate_node_shape(&node, shape).map_err(LazyTreeError::Node)?;
        }
        Ok(node)
    }

    fn finish<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        after: Option<R::Value>,
        insert_key: Option<&R::Key>,
        insert_only: bool,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        let loaded_before = self.loaded_nodes.get();
        let mut result = self.rewrite(&self.root, key, after, insert_key, insert_only, true)?;
        let mut roots = result.roots;
        let mut level = roots.first().map_or(0, CanonicalNode::level);
        while roots.len() > 1 {
            let summaries = roots
                .iter()
                .map(committed_child)
                .collect::<Result<Vec<_>, _>>()
                .map_err(LazyTreeError::Node)?;
            let keys: Vec<_> = summaries
                .iter()
                .map(|child| child.first_key.clone())
                .collect();
            let cuts = anchored_cut_points_children::<R, _>(
                keys.iter(),
                DEFAULT_CUT_POLICY,
                level.saturating_add(1),
            )
            .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            let mut parents = Vec::new();
            let mut start = 0;
            for end in cuts {
                parents.push(
                    canonical_branch_from_commitments::<R>(
                        level.saturating_add(1),
                        &summaries[start..end],
                    )
                    .map_err(LazyTreeError::Node)?,
                );
                start = end;
            }
            result.split_nodes = result
                .split_nodes
                .saturating_add(parents.len().saturating_sub(1));
            result.changed.extend(parents.iter().cloned());
            roots = parents;
            level = level.saturating_add(1);
        }
        let target = roots
            .pop()
            .ok_or(LazyTreeError::Node(NodeError::InvalidBranch))?;
        let emitted_bytes = result
            .changed
            .iter()
            .map(|node| node.as_bytes().len())
            .sum();
        let work = LazyTreeWork {
            loaded_nodes: self.loaded_nodes.get().saturating_sub(loaded_before),
            rebuilt_nodes: result.changed.len(),
            split_nodes: result.split_nodes,
            removed_entries: result.removed_entries,
            emitted_bytes,
            peak_metadata_bytes: 0,
        };
        Ok(LazyPreparedUpdate {
            base: self.root.root(),
            target: CheckedCanonicalRoot::from_parts(target.commitment(), target),
            changed: result.changed,
            work,
            changes: vec![result.change],
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the recursive path-copy state machine is kept together"
    )]
    fn rewrite<Q: ?Sized + Ord>(
        &self,
        node: &CheckedCanonicalRoot<R>,
        key: &Q,
        after: Option<R::Value>,
        insert_key: Option<&R::Key>,
        insert_only: bool,
        is_root: bool,
    ) -> Result<RewriteResult<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        if node.node().level() == 0 {
            let mut entries = node.leaf_entries().map_err(LazyTreeError::Node)?;
            let mut removed_entries = 0;
            let change = match (
                entries.binary_search_by(|(candidate, _)| candidate.borrow().cmp(key)),
                after,
            ) {
                (Ok(index), Some(value)) if !insert_only => {
                    let change = MapChange {
                        key: entries[index].0.clone(),
                        before: Some(entries[index].1.clone()),
                        after: Some(value.clone()),
                    };
                    entries[index].1 = value;
                    change
                }
                (Ok(_), Some(_)) => return Err(LazyTreeError::DuplicateKey),
                (Ok(index), None) => {
                    let change = MapChange {
                        key: entries[index].0.clone(),
                        before: Some(entries[index].1.clone()),
                        after: None,
                    };
                    entries.remove(index);
                    removed_entries = 1;
                    change
                }
                (Err(index), Some(value)) => {
                    let owned = insert_key.cloned().ok_or(LazyTreeError::MissingKey)?;
                    let change = MapChange {
                        key: owned.clone(),
                        before: None,
                        after: Some(value.clone()),
                    };
                    entries.insert(index, (owned, value));
                    change
                }
                (Err(_), None) => return Err(LazyTreeError::MissingKey),
            };
            if entries.is_empty() && !is_root {
                return Ok(RewriteResult {
                    roots: Vec::new(),
                    changed: Vec::new(),
                    split_nodes: 0,
                    removed_entries,
                    change,
                });
            }
            let roots = make_leaves::<R>(&entries).map_err(LazyTreeError::Node)?;
            let split_nodes = roots.len().saturating_sub(1);
            return Ok(RewriteResult {
                changed: roots.clone(),
                roots,
                split_nodes,
                removed_entries,
                change,
            });
        }

        let mut summaries = node.child_summaries().map_err(LazyTreeError::Node)?;
        if summaries.is_empty() {
            return Err(LazyTreeError::Node(NodeError::InvalidBranch));
        }
        let index = summaries
            .partition_point(|child| child.first_key.borrow() <= key)
            .saturating_sub(1);
        let claim = child_claim(&summaries[index]).map_err(LazyTreeError::Node)?;
        let child = self.load(claim)?;
        let deleting = after.is_none() && !insert_only;
        let mut result = self.rewrite(&child, key, after, insert_key, insert_only, false)?;

        // A changed leaf can move a content-defined boundary into the next
        // leaf. Load only the adjacent region until a complete boundary is
        // reached.
        if node.node().level() == 1 && (deleting || result.roots.len() > 1) {
            let obsolete = result.roots.len();
            let changed_before = result.changed.len();
            let (replacement, loaded_start, loaded_end) =
                self.spill_leaf_region(&mut result, &summaries, index)?;
            // The leaf region helper replaces the provisional split/merge
            // nodes. Descendant nodes, if any, remain in the frontier.
            result
                .changed
                .truncate(changed_before.saturating_sub(obsolete));
            result.changed.extend(replacement.iter().cloned());
            result.roots = replacement;
            summaries.splice(
                loaded_start..=loaded_end,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        } else if node.node().level() > 1 && (deleting || result.roots.len() > 1) {
            let obsolete = result.roots.len();
            let changed_before = result.changed.len();
            let (replacement, loaded_start, loaded_end) =
                self.spill_branch_region(&mut result, &summaries, index, node.node().level())?;
            result
                .changed
                .truncate(changed_before.saturating_sub(obsolete));
            result.changed.extend(replacement.iter().cloned());
            result.roots = replacement;
            summaries.splice(
                loaded_start..=loaded_end,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        } else {
            summaries.splice(
                index..=index,
                result
                    .roots
                    .iter()
                    .map(committed_child)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LazyTreeError::Node)?,
            );
        }
        if summaries.is_empty() {
            if !is_root {
                return Ok(RewriteResult {
                    roots: Vec::new(),
                    changed: result.changed,
                    split_nodes: result.split_nodes,
                    removed_entries: result.removed_entries,
                    change: result.change,
                });
            }
            let empty = canonical_empty::<R>();
            result.changed.push(empty.clone());
            return Ok(RewriteResult {
                roots: vec![empty],
                changed: result.changed,
                split_nodes: result.split_nodes,
                removed_entries: result.removed_entries,
                change: result.change,
            });
        }
        if is_root && summaries.len() == 1 {
            let only = child_node_from_result(&result, &summaries[0]);
            if let Some(only) = only {
                result.roots = vec![only];
                return Ok(result);
            }
        }
        let keys: Vec<_> = summaries
            .iter()
            .map(|child| child.first_key.clone())
            .collect();
        let cuts = anchored_cut_points_children::<R, _>(
            keys.iter(),
            DEFAULT_CUT_POLICY,
            node.node().level(),
        )
        .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
        let mut branches = Vec::new();
        let mut start = 0;
        for end in cuts {
            branches.push(
                canonical_branch_from_commitments::<R>(node.node().level(), &summaries[start..end])
                    .map_err(LazyTreeError::Node)?,
            );
            start = end;
        }
        result.split_nodes = result
            .split_nodes
            .saturating_add(branches.len().saturating_sub(1));
        result.changed.extend(branches.iter().cloned());
        result.roots = branches;
        Ok(result)
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple carries the changed child range"
    )]
    fn spill_leaf_region(
        &self,
        result: &mut RewriteResult<R>,
        summaries: &[CommittedChild<R>],
        index: usize,
    ) -> Result<(Vec<CanonicalNode<R>>, usize, usize), LazyTreeError<L::Error>> {
        let mut entries = result.roots.first().map(|_| Vec::new()).unwrap_or_default();
        for leaf in &result.roots {
            entries.extend(
                CheckedCanonicalRoot::from_parts(leaf.commitment(), leaf.clone())
                    .leaf_entries()
                    .map_err(LazyTreeError::Node)?,
            );
        }
        let mut end = index;
        while end + 1 < summaries.len() {
            let next_claim = child_claim(&summaries[end + 1]).map_err(LazyTreeError::Node)?;
            let next = self.load(next_claim)?;
            entries.extend(next.leaf_entries().map_err(LazyTreeError::Node)?);
            end += 1;
            let probe = summaries.get(end + 1).map(|child| child.first_key.clone());
            let cuts = leaf_probe_cuts::<R>(&entries, probe.as_ref())
                .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            if cuts.contains(&entries.len()) {
                break;
            }
        }
        if entries.is_empty() {
            return Ok((Vec::new(), index, end));
        }
        if end == index && index > 0 {
            let previous_claim = child_claim(&summaries[index - 1]).map_err(LazyTreeError::Node)?;
            let previous = self.load(previous_claim)?;
            let mut combined = previous.leaf_entries().map_err(LazyTreeError::Node)?;
            combined.extend(entries);
            let start = index - 1;
            let leaves = make_leaves::<R>(&combined).map_err(LazyTreeError::Node)?;
            result.split_nodes = result
                .split_nodes
                .saturating_add(leaves.len().saturating_sub(1));
            return Ok((leaves, start, index));
        }
        let leaves = make_leaves::<R>(&entries).map_err(LazyTreeError::Node)?;
        result.split_nodes = result
            .split_nodes
            .saturating_add(leaves.len().saturating_sub(1));
        Ok((leaves, index, end))
    }

    #[allow(
        clippy::type_complexity,
        reason = "the tuple carries the changed child range"
    )]
    fn spill_branch_region(
        &self,
        result: &mut RewriteResult<R>,
        summaries: &[CommittedChild<R>],
        index: usize,
        parent_level: u16,
    ) -> Result<(Vec<CanonicalNode<R>>, usize, usize), LazyTreeError<L::Error>> {
        let child_level = parent_level.saturating_sub(1);
        let mut children = result
            .roots
            .iter()
            .map(|branch| {
                CheckedCanonicalRoot::from_parts(branch.commitment(), branch.clone())
                    .child_summaries()
                    .map_err(LazyTreeError::Node)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let mut end = index;
        while end + 1 < summaries.len() {
            let next = self.load(child_claim(&summaries[end + 1]).map_err(LazyTreeError::Node)?)?;
            children.extend(next.child_summaries().map_err(LazyTreeError::Node)?);
            end += 1;
            let keys: Vec<_> = children
                .iter()
                .map(|child| child.first_key.clone())
                .collect();
            let probe = summaries.get(end + 1).map(|child| child.first_key.clone());
            let mut probe_keys = keys.clone();
            if let Some(probe) = probe {
                probe_keys.push(probe);
            }
            let cuts = anchored_cut_points_children::<R, _>(
                probe_keys.iter(),
                DEFAULT_CUT_POLICY,
                child_level,
            )
            .map_err(|_| LazyTreeError::Node(NodeError::InvalidBranch))?;
            if cuts.contains(&keys.len()) {
                break;
            }
        }
        if children.is_empty() {
            return Ok((Vec::new(), index, end));
        }
        if end == index && index > 0 {
            let previous =
                self.load(child_claim(&summaries[index - 1]).map_err(LazyTreeError::Node)?)?;
            let mut combined = previous.child_summaries().map_err(LazyTreeError::Node)?;
            combined.extend(children);
            children = combined;
            let leaves = make_branches::<R>(child_level, &children).map_err(LazyTreeError::Node)?;
            result.split_nodes = result
                .split_nodes
                .saturating_add(leaves.len().saturating_sub(1));
            return Ok((leaves, index - 1, index));
        }
        let branches = make_branches::<R>(child_level, &children).map_err(LazyTreeError::Node)?;
        result.split_nodes = result
            .split_nodes
            .saturating_add(branches.len().saturating_sub(1));
        Ok((branches, index, end))
    }
}

fn map_eager_tree_error(error: TreeError) -> NodeError {
    match error {
        TreeError::UnsortedOrDuplicate => NodeError::UnsortedOrDuplicate,
        TreeError::Canonical(error) => error,
        TreeError::InvalidRoot | TreeError::Overflow => NodeError::InvalidBranch,
    }
}

#[cfg(test)]
mod metadata_shape_tests {
    use super::LazyTreeMetadataShape;
    use crate::DEFAULT_CUT_POLICY;

    #[test]
    fn full_entry_count_shape_uses_the_canonical_encoded_byte_ceiling() {
        let shape = LazyTreeMetadataShape::new(32, 0, 64, 1024, 18, 4);
        assert!(shape.valid());
        assert_eq!(
            shape.max_node_bytes(),
            Some(DEFAULT_CUT_POLICY.max_encoded_bytes())
        );
    }
}
