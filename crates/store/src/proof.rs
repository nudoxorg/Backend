use super::{
    CommittedChild, Node, OrderedMap, RawRelation, StateRoot, StoreError, StoredValue,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf, raw_value,
    validate_entries,
};

mod range;

pub(crate) use range::range_proof;

#[derive(Clone, Debug, Eq, PartialEq)]
/// Authenticated observation of one map entry.
pub struct Proof {
    /// Canonical map root being proved.
    pub root: StateRoot<RawRelation>,
    /// Key observed by the proof.
    pub key: Vec<u8>,
    /// Value observed at [`Proof::key`].
    pub version: StoredValue,
    /// Complete canonical leaf payload containing [`Proof::key`].
    pub leaf_entries: Vec<(Vec<u8>, StoredValue)>,
    /// Sibling commitments and anchors from the searched path.
    pub path: Vec<ProofLevel>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
/// One internal-node level in an authenticated proof path.
pub struct ProofLevel {
    /// Canonical tree level represented by this path element.
    pub level: u16,
    /// Child anchors, commitments, levels, and row counts in canonical order.
    pub children: Vec<(Vec<u8>, StateRoot<RawRelation>, u16, u64)>,
    /// Child selected by the proof key.
    pub selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Authenticated membership or nonmembership observation for one key.
pub struct KeyProof {
    /// Canonical map root being proved.
    pub root: StateRoot<RawRelation>,
    /// Key observed by the proof.
    pub key: Vec<u8>,
    /// Value at the key, or `None` when the proof establishes nonmembership.
    pub value: Option<StoredValue>,
    /// Complete canonical leaf selected by the key.
    pub leaf_entries: Vec<(Vec<u8>, StoredValue)>,
    /// Ordered authenticated path to the selected leaf.
    pub path: Vec<ProofLevel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Authenticated bounded range of canonical map entries.
pub struct RangeProof {
    /// Canonical map root being proved.
    pub root: StateRoot<RawRelation>,
    /// Inclusive lower bound, when present.
    pub start: Option<Vec<u8>>,
    /// Exclusive upper bound, when present.
    pub end: Option<Vec<u8>>,
    /// Entries returned by the range query in canonical order.
    pub entries: Vec<(Vec<u8>, StoredValue)>,
    /// Full leaves and paths authenticating the returned entries.
    pub leaf_proofs: Vec<KeyProof>,
    /// Boundary proof for the inclusive lower bound, when present.
    pub start_proof: Option<KeyProof>,
    /// Boundary proof for the exclusive upper bound, when present.
    pub end_proof: Option<KeyProof>,
}

macro_rules! verified_proof {
    ($name:ident, $raw:ty) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        /// Proof bytes admitted against one exact canonical root.
        pub struct $name {
            proof: $raw,
            root: StateRoot<RawRelation>,
        }

        impl $name {
            /// Returns the root against which this proof was admitted.
            #[must_use]
            pub const fn root(&self) -> StateRoot<RawRelation> {
                self.root
            }

            /// Borrows the authenticated observation.
            #[must_use]
            pub const fn proof(&self) -> &$raw {
                &self.proof
            }

            /// Deliberately drops verified state for wire transport or mutation.
            #[must_use]
            pub fn into_observed(self) -> $raw {
                self.proof
            }
        }
    };
}

verified_proof!(VerifiedProof, Proof);
verified_proof!(VerifiedKeyProof, KeyProof);
verified_proof!(VerifiedRangeProof, RangeProof);

impl Proof {
    /// Consumes this untrusted observation and admits it against an exact root.
    ///
    /// On failure the unchanged observation is returned, so rejected wire
    /// evidence cannot accidentally retain the verified type.
    ///
    /// # Errors
    ///
    /// Returns the unchanged observed proof when authentication fails.
    #[allow(
        clippy::result_large_err,
        reason = "rejection deliberately returns the caller's complete proof without allocation"
    )]
    pub fn admit_root(self, root: StateRoot<RawRelation>) -> Result<VerifiedProof, Self> {
        if self.verify_root(root) {
            Ok(VerifiedProof { proof: self, root })
        } else {
            Err(self)
        }
    }

    /// Checks the observation against an in-memory map.
    #[must_use]
    pub fn verify(&self, map: &OrderedMap) -> bool {
        map.state_root() == self.root
            && map.get(&self.key) == Some(&self.version)
            && self.verify_root(self.root)
    }
    /// Checks the proof's claimed root against a supplied root commitment.
    #[must_use]
    pub fn verify_root(&self, root: StateRoot<RawRelation>) -> bool {
        verify_key_parts(
            self.root,
            root,
            &self.key,
            Some(&self.version),
            &self.leaf_entries,
            &self.path,
        )
    }
}

impl KeyProof {
    /// Consumes this membership/nonmembership observation and admits it
    /// against an exact canonical root.
    ///
    /// # Errors
    ///
    /// Returns the unchanged observed proof when authentication fails.
    #[allow(
        clippy::result_large_err,
        reason = "rejection deliberately returns the caller's complete proof without allocation"
    )]
    pub fn admit_root(self, root: StateRoot<RawRelation>) -> Result<VerifiedKeyProof, Self> {
        if self.verify_root(root) {
            Ok(VerifiedKeyProof { proof: self, root })
        } else {
            Err(self)
        }
    }

    /// Returns whether this proof contains a value for its key.
    #[must_use]
    pub const fn is_membership(&self) -> bool {
        self.value.is_some()
    }

    /// Returns whether this proof establishes that its key is absent.
    #[must_use]
    pub const fn is_nonmembership(&self) -> bool {
        self.value.is_none()
    }

    /// Checks the observation against an in-memory map.
    #[must_use]
    pub fn verify(&self, map: &OrderedMap) -> bool {
        map.state_root() == self.root
            && map.get(&self.key) == self.value.as_ref()
            && self.verify_root(self.root)
    }

    /// Checks the membership or nonmembership claim against a root.
    #[must_use]
    pub fn verify_root(&self, root: StateRoot<RawRelation>) -> bool {
        verify_key_parts(
            self.root,
            root,
            &self.key,
            self.value.as_ref(),
            &self.leaf_entries,
            &self.path,
        )
    }
}

fn verify_key_parts(
    claimed_root: StateRoot<RawRelation>,
    root: StateRoot<RawRelation>,
    key: &[u8],
    value: Option<&StoredValue>,
    leaf_entries: &[(Vec<u8>, StoredValue)],
    path: &[ProofLevel],
) -> bool {
    if claimed_root != root
        || leaf_entries
            .windows(2)
            .any(|window| window[0].0 >= window[1].0)
        || leaf_entries
            .iter()
            .find(|(entry_key, _)| entry_key.as_slice() == key)
            .map_or(value.is_some(), |(_, entry_value)| {
                Some(entry_value) != value
            })
    {
        return false;
    }
    if validate_entries(leaf_entries).is_err() {
        return false;
    }
    if leaf_entries.is_empty() {
        if !path.is_empty() {
            return false;
        }
    } else {
        let first = &leaf_entries[0].0;
        let last = &leaf_entries[leaf_entries.len() - 1].0;
        let before_leaf = key < first;
        let after_leaf = key > last;
        let first_leaf = path.iter().all(|level| level.selected == 0);
        let final_leaf = path
            .iter()
            .all(|level| level.selected + 1 == level.children.len());
        if (before_leaf && !first_leaf) || (after_leaf && !final_leaf) {
            return false;
        }
    }
    let raw_entries = leaf_entries
        .iter()
        .map(|(entry_key, entry_value)| (entry_key.clone(), raw_value(entry_value)))
        .collect::<Vec<_>>();
    let Ok(mut node) = canonical_leaf::<RawRelation>(&raw_entries) else {
        return false;
    };
    for level in path {
        if level.selected >= level.children.len() {
            return false;
        }
        let Some((selected_key, _, selected_level, selected_rows)) =
            level.children.get(level.selected)
        else {
            return false;
        };
        let before_first_child = level.selected == 0 && selected_key.as_slice() > key;
        if node.first_key() != Some(selected_key)
            || *selected_level != node.level()
            || *selected_rows != node.row_count()
            || (selected_key.as_slice() > key && !before_first_child)
            || level
                .children
                .get(level.selected + 1)
                .is_some_and(|(next_key, _, _, _)| next_key.as_slice() <= key)
        {
            return false;
        }
        let children = level
            .children
            .iter()
            .map(
                |(child_key, commitment, child_level, row_count)| CommittedChild {
                    first_key: child_key.clone(),
                    commitment: (*commitment).into(),
                    level: *child_level,
                    row_count: *row_count,
                },
            )
            .collect::<Vec<_>>();
        let mut children = children;
        children[level.selected].commitment = node.commitment().into();
        children[level.selected].row_count = node.row_count();
        let Ok(parent) = canonical_branch_from_commitments::<RawRelation>(level.level, &children)
        else {
            return false;
        };
        node = parent;
    }
    node.commitment() == root
}

pub(crate) fn key_proof(root: &Node, key: &[u8]) -> KeyProof {
    let (leaf_entries, path) = proof_path(root, key);
    let value = leaf_entries
        .iter()
        .find(|(entry_key, _)| entry_key.as_slice() == key)
        .map(|(_, value)| value.clone());
    KeyProof {
        root: root.canonical().commitment(),
        key: key.to_vec(),
        value,
        leaf_entries,
        path,
    }
}

pub(crate) fn proof_path(
    node: &Node,
    key: &[u8],
) -> (Vec<(Vec<u8>, StoredValue)>, Vec<ProofLevel>) {
    if let Some(entries) = node.entries() {
        return (entries.to_vec(), Vec::new());
    }
    let children = node.children().collect::<Vec<_>>();
    if children.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let selected = children
        .partition_point(|child| {
            child
                .summary()
                .first_key
                .is_some_and(|first| first.as_slice() <= key)
        })
        .saturating_sub(1)
        .min(children.len().saturating_sub(1));
    let Some(selected_child) = children.get(selected) else {
        return (Vec::new(), Vec::new());
    };
    let (leaf_entries, mut path) = proof_path(selected_child, key);
    let summaries = children
        .iter()
        .map(|child| {
            let summary = child.summary();
            summary.first_key.map(|first_key| {
                (
                    first_key,
                    summary.commitment,
                    summary.level,
                    summary.len as u64,
                )
            })
        })
        .collect::<Option<Vec<_>>>();
    let Some(summaries) = summaries else {
        return (Vec::new(), Vec::new());
    };
    path.push(ProofLevel {
        level: node.summary().level,
        children: summaries,
        selected,
    });
    (leaf_entries, path)
}
