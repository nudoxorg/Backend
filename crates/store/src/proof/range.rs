//! Verifies and builds authenticated range proofs.

use super::*;

impl RangeProof {
    /// Consumes this bounded-range observation and admits it against an exact
    /// canonical root.
    ///
    /// # Errors
    ///
    /// Returns the unchanged observed proof when authentication fails.
    #[allow(
        clippy::result_large_err,
        reason = "rejection deliberately returns the caller's complete proof without allocation"
    )]
    pub fn admit_root(self, root: StateRoot<RawRelation>) -> Result<VerifiedRangeProof, Self> {
        if self.verify_root(root) {
            Ok(VerifiedRangeProof { proof: self, root })
        } else {
            Err(self)
        }
    }

    /// Checks all returned entries and their authenticated leaves.
    #[must_use]
    pub fn verify_root(&self, root: StateRoot<RawRelation>) -> bool {
        if self.root != root || !valid_range_entries(self) {
            return false;
        }
        if !valid_boundary_proofs(self, root) {
            return false;
        }
        if self.entries.is_empty() {
            return valid_empty_range(self, root);
        }
        if !valid_nonempty_range(self) {
            return false;
        }
        let mut authenticated = Vec::new();
        for proof in &self.leaf_proofs {
            if !proof.verify_root(root) {
                return false;
            }
            for entry in &proof.leaf_entries {
                if in_range(&entry.0, self.start.as_deref(), self.end.as_deref()) {
                    authenticated.push(entry.clone());
                }
            }
        }
        authenticated.sort_by(|left, right| left.0.cmp(&right.0));
        authenticated.dedup_by(|left, right| left.0 == right.0);
        authenticated == self.entries
    }

    /// Checks the range against an in-memory map.
    #[must_use]
    pub fn verify(&self, map: &OrderedMap) -> bool {
        map.state_root() == self.root
            && map
                .iter()
                .filter(|(key, _)| in_range(key, self.start.as_deref(), self.end.as_deref()))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>()
                == self.entries
            && self.verify_root(self.root)
    }
}

fn valid_range_entries(proof: &RangeProof) -> bool {
    !(proof
        .start
        .as_ref()
        .zip(proof.end.as_ref())
        .is_some_and(|(start, end)| start >= end)
        || proof
            .entries
            .windows(2)
            .any(|window| window[0].0 >= window[1].0)
        || proof
            .entries
            .iter()
            .any(|(key, _)| !in_range(key, proof.start.as_deref(), proof.end.as_deref()))
        || validate_entries(&proof.entries).is_err())
}

fn valid_boundary_proofs(proof: &RangeProof, root: StateRoot<RawRelation>) -> bool {
    if proof.start.is_some() != proof.start_proof.is_some()
        || proof.end.is_some() != proof.end_proof.is_some()
    {
        return false;
    }
    proof.start_proof.as_ref().is_none_or(|boundary| {
        proof.start.as_deref() == Some(boundary.key.as_slice()) && boundary.verify_root(root)
    }) && proof.end_proof.as_ref().is_none_or(|boundary| {
        proof.end.as_deref() == Some(boundary.key.as_slice()) && boundary.verify_root(root)
    })
}

fn valid_empty_range(proof: &RangeProof, root: StateRoot<RawRelation>) -> bool {
    if !proof.leaf_proofs.is_empty()
        || proof.start_proof.as_ref().is_some_and(|boundary| {
            has_range_entry(boundary, proof.start.as_deref(), proof.end.as_deref())
        })
        || proof.end_proof.as_ref().is_some_and(|boundary| {
            has_range_entry(boundary, proof.start.as_deref(), proof.end.as_deref())
        })
    {
        return false;
    }
    match (proof.start_proof.as_ref(), proof.end_proof.as_ref()) {
        (Some(start), Some(end)) => {
            same_leaf_path(&start.path, &end.path) || adjacent_leaf_paths(&start.path, &end.path)
        }
        (Some(start), None) => path_is_last(&start.path),
        (None, Some(end)) => path_is_first(&end.path),
        (None, None) => canonical_empty::<RawRelation>().commitment() == root,
    }
}

fn valid_nonempty_range(proof: &RangeProof) -> bool {
    if proof.leaf_proofs.is_empty()
        || proof
            .leaf_proofs
            .iter()
            .any(|leaf| !has_range_entry(leaf, proof.start.as_deref(), proof.end.as_deref()))
        || !proof
            .leaf_proofs
            .windows(2)
            .all(|window| adjacent_leaf_paths(&window[0].path, &window[1].path))
    {
        return false;
    }
    let first = &proof.leaf_proofs[0];
    let Some(last) = proof.leaf_proofs.last() else {
        return false;
    };
    if !valid_start_boundary(proof, first) || !valid_end_boundary(proof, last) {
        return false;
    }
    true
}

fn valid_start_boundary(proof: &RangeProof, first: &KeyProof) -> bool {
    let Some(boundary) = proof.start_proof.as_ref() else {
        return path_is_first(&first.path);
    };
    if has_range_entry(boundary, proof.start.as_deref(), proof.end.as_deref()) {
        same_leaf_path(&boundary.path, &first.path)
    } else {
        adjacent_leaf_paths(&boundary.path, &first.path)
    }
}

fn valid_end_boundary(proof: &RangeProof, last: &KeyProof) -> bool {
    let Some(boundary) = proof.end_proof.as_ref() else {
        return path_is_last(&last.path);
    };
    if has_range_entry(boundary, proof.start.as_deref(), proof.end.as_deref()) {
        same_leaf_path(&boundary.path, &last.path)
    } else {
        adjacent_leaf_paths(&last.path, &boundary.path)
    }
}

fn has_range_entry(proof: &KeyProof, start: Option<&[u8]>, end: Option<&[u8]>) -> bool {
    proof
        .leaf_entries
        .iter()
        .any(|(key, _)| in_range(key, start, end))
}

fn same_leaf_path(left: &[ProofLevel], right: &[ProofLevel]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.level == b.level && a.selected == b.selected)
}

fn path_is_first(path: &[ProofLevel]) -> bool {
    path.iter().all(|level| level.selected == 0)
}

fn path_is_last(path: &[ProofLevel]) -> bool {
    path.iter().all(|level| {
        level
            .selected
            .checked_add(1)
            .is_some_and(|selected| selected == level.children.len())
    })
}

fn adjacent_leaf_paths(left: &[ProofLevel], right: &[ProofLevel]) -> bool {
    if left.len() != right.len() || left.iter().zip(right).any(|(a, b)| a.level != b.level) {
        return false;
    }
    let differing_from_root = left
        .iter()
        .rev()
        .zip(right.iter().rev())
        .position(|(a, b)| a.selected != b.selected);
    let Some(differing_from_root) = differing_from_root else {
        return false;
    };
    let root_index = left.len() - 1 - differing_from_root;
    let left_at = &left[root_index];
    let right_at = &right[root_index];
    let Some(next_selected) = left_at.selected.checked_add(1) else {
        return false;
    };
    if right_at.selected != next_selected
        || left[root_index..]
            .iter()
            .zip(&right[root_index..])
            .any(|(a, b)| a.children.len() != b.children.len())
    {
        return false;
    }
    left[..root_index]
        .iter()
        .all(|level| level.selected + 1 == level.children.len())
        && right[..root_index].iter().all(|level| level.selected == 0)
}

fn in_range(key: &[u8], start: Option<&[u8]>, end: Option<&[u8]>) -> bool {
    start.is_none_or(|start| key >= start) && end.is_none_or(|end| key < end)
}

pub(crate) fn range_proof(
    root: &Node,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
) -> Result<RangeProof, StoreError> {
    if start.zip(end).is_some_and(|(start, end)| start >= end) {
        return Err(StoreError::MalformedDelta);
    }
    let mut leaves = Vec::new();
    collect_range_leaves(root, start, end, &mut leaves);
    let mut leaf_proofs = Vec::new();
    let mut entries = Vec::new();
    for leaf in leaves {
        let Some(leaf_entries) = leaf.entries() else {
            return Err(StoreError::Corrupt);
        };
        let Some(query) = leaf_entries
            .iter()
            .find(|(key, _)| in_range(key, start, end))
            .map(|(key, _)| key.as_slice())
            .or_else(|| {
                start.filter(|key| last_key(&leaf).is_some_and(|last| *key <= last.as_slice()))
            })
        else {
            continue;
        };
        let proof = key_proof(root, query);
        entries.extend(
            leaf_entries
                .iter()
                .filter(|(key, _)| in_range(key, start, end))
                .cloned(),
        );
        leaf_proofs.push(proof);
    }
    Ok(RangeProof {
        root: root.canonical().commitment(),
        start: start.map(Vec::from),
        end: end.map(Vec::from),
        entries,
        leaf_proofs,
        start_proof: start.map(|key| key_proof(root, key)),
        end_proof: end.map(|key| key_proof(root, key)),
    })
}

fn collect_range_leaves(
    node: &Node,
    start: Option<&[u8]>,
    end: Option<&[u8]>,
    leaves: &mut Vec<Node>,
) {
    let first = node.summary().first_key;
    let last = last_key(node);
    if first
        .as_ref()
        .zip(end)
        .is_some_and(|(first, end)| first.as_slice() >= end)
        || last
            .as_ref()
            .zip(start)
            .is_some_and(|(last, start)| last.as_slice() < start)
    {
        return;
    }
    if node.entries().is_some() {
        leaves.push(node.clone());
        return;
    }
    for child in node.children() {
        collect_range_leaves(&child, start, end, leaves);
    }
}
fn last_key(node: &Node) -> Option<Vec<u8>> {
    if let Some(entries) = node.entries() {
        return entries.last().map(|(key, _)| key.clone());
    }
    node.children().last().and_then(|child| last_key(&child))
}
