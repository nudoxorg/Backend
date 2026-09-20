//! Changed-frontier overlay and deterministic node construction helpers.

use super::{
    CanonicalNode, CanonicalRelation, CanonicalRootAdmissionError, CheckedCanonicalRoot,
    ChildCommitment, CommittedChild, DEFAULT_CUT_POLICY, IdContext, LazyTreeError, NodeError,
    RewriteResult, TreeNodeLoader, UntrustedId, admit_canonical_root_claim,
    anchored_cut_points_children, anchored_cut_points_items, canonical_branch_from_commitments,
    canonical_empty, canonical_leaf,
};
use std::collections::BTreeMap;

/// Checked-node write overlay used only while building a multi-key lazy
/// update. Its lifetime is tied to the backing loader.
pub(super) struct OverlayLoader<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> {
    base: &'a L,
    nodes: BTreeMap<[u8; crate::ID_BYTES], CheckedCanonicalRoot<R>>,
}

impl<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> OverlayLoader<'a, R, L> {
    pub(super) fn new(base: &'a L) -> Self {
        Self {
            base,
            nodes: BTreeMap::new(),
        }
    }

    pub(super) fn insert(
        &mut self,
        node: &CanonicalNode<R>,
    ) -> Result<(), LazyTreeError<L::Error>> {
        let version = node.commitment().to_bytes();
        if self.nodes.contains_key(&version) {
            return Ok(());
        }
        let claim = UntrustedId::from_wire(&version, IdContext::relation::<R>())
            .map_err(|_| LazyTreeError::Node(NodeError::SchemaMismatch))?;
        let checked = admit_canonical_root_claim(claim, node.as_bytes()).map_err(|error| {
            LazyTreeError::Node(match error {
                CanonicalRootAdmissionError::Node(error) => error,
                CanonicalRootAdmissionError::Identity(_) => NodeError::AnchorMismatch,
            })
        })?;
        self.nodes.insert(version, checked);
        Ok(())
    }
}

impl<R: CanonicalRelation, L: TreeNodeLoader<R>> TreeNodeLoader<R> for OverlayLoader<'_, R, L> {
    type Error = L::Error;

    fn load(&self, claim: UntrustedId<R>) -> Result<CheckedCanonicalRoot<R>, Self::Error> {
        self.nodes
            .get(claim.as_bytes())
            .cloned()
            .map_or_else(|| self.base.load(claim), Ok)
    }
}

pub(super) fn child_claim<R: CanonicalRelation>(
    child: &CommittedChild<R>,
) -> Result<UntrustedId<R>, NodeError> {
    UntrustedId::from_wire(child.commitment.as_bytes(), IdContext::relation::<R>())
        .map_err(|_| NodeError::MalformedEncoding)
}

pub(super) fn committed_child<R: CanonicalRelation>(
    node: &CanonicalNode<R>,
) -> Result<CommittedChild<R>, NodeError> {
    Ok(CommittedChild {
        first_key: node.first_key().cloned().ok_or(NodeError::AnchorMismatch)?,
        commitment: ChildCommitment::from(node.commitment()),
        level: node.level(),
        row_count: node.row_count(),
    })
}

pub(super) fn child_node_from_result<R: CanonicalRelation>(
    result: &RewriteResult<R>,
    child: &CommittedChild<R>,
) -> Option<CanonicalNode<R>> {
    result
        .roots
        .iter()
        .find(|node| node.commitment().as_bytes() == child.commitment.as_bytes())
        .cloned()
}

pub(super) fn make_leaves<R: CanonicalRelation>(
    entries: &[(R::Key, R::Value)],
) -> Result<Vec<CanonicalNode<R>>, NodeError> {
    if entries.is_empty() {
        return Ok(vec![canonical_empty::<R>()]);
    }
    let cuts = anchored_cut_points_items::<R, _>(entries.iter(), DEFAULT_CUT_POLICY, 0)
        .map_err(|_| NodeError::InvalidBranch)?;
    let mut leaves = Vec::with_capacity(cuts.len());
    let mut start = 0;
    for end in cuts {
        leaves.push(canonical_leaf::<R>(&entries[start..end])?);
        start = end;
    }
    Ok(leaves)
}

pub(super) fn leaf_probe_cuts<R: CanonicalRelation>(
    entries: &[(R::Key, R::Value)],
    suffix: Option<&R::Key>,
) -> Result<Vec<usize>, crate::CutError> {
    let mut sized = entries
        .iter()
        .map(|(key, value)| {
            let mut encoded = Vec::new();
            R::encode_value(value, &mut encoded);
            (key, 8usize.saturating_add(encoded.len()))
        })
        .collect::<Vec<_>>();
    if let Some(suffix) = suffix {
        sized.push((suffix, 0));
    }
    crate::tree::anchored_cut_points_sized::<R, _>(sized, DEFAULT_CUT_POLICY, 0)
}

pub(super) fn make_branches<R: CanonicalRelation>(
    level: u16,
    children: &[CommittedChild<R>],
) -> Result<Vec<CanonicalNode<R>>, NodeError> {
    if children.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<_> = children
        .iter()
        .map(|child| child.first_key.clone())
        .collect();
    let cuts = anchored_cut_points_children::<R, _>(keys.iter(), DEFAULT_CUT_POLICY, level)
        .map_err(|_| NodeError::InvalidBranch)?;
    let mut branches = Vec::with_capacity(cuts.len());
    let mut start = 0;
    for end in cuts {
        branches.push(canonical_branch_from_commitments::<R>(
            level,
            &children[start..end],
        )?);
        start = end;
    }
    Ok(branches)
}
