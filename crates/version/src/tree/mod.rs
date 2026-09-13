//! Canonical key anchored ordered Merkle tree.
//!
//! The facade intentionally exposes only the checked node and builder
//! contracts.  Encoding, admission, and boundary selection live in the
//! implementation module while this file remains the stable package seam.

const CUT_HASH_DOMAIN: &[u8] = b"backend.version.cut\0";
const NODE_MAGIC: &[u8] = b"V2NODE\0";
const DEFAULT_MAX_ENCODED_BYTES: usize = 64 * 1024;

mod admission;
mod build;
mod cut;
mod node;
mod view;

pub use admission::{CanonicalChildIter, admit_canonical_root, admit_canonical_root_claim};
pub use build::{
    canonical_branch, canonical_branch_from_commitments, canonical_empty, canonical_leaf,
    canonical_root,
};
pub use cut::{CutError, CutPolicy, DEFAULT_CUT_POLICY, anchored_cut_points, cut_points};
pub use node::{
    CanonicalNode, CanonicalRootAdmissionError, CheckedCanonicalRoot, Child, ChildCommitment,
    CommittedChild, NodeError,
};
pub use view::{
    CanonicalChildWireIter, CanonicalChildWireRef, CanonicalLeafField, CanonicalNodeView,
};

pub(crate) use cut::{
    anchored_cut_points_children, anchored_cut_points_items, anchored_cut_points_sized,
};
