//! Typed, content-addressed version primitives for the v2 engine.
//!
//! The crate has no storage or executor.  It owns the canonical ID and
//! encoding grammar, key-anchored ordered tree, coverage capabilities, exact
//! relation transitions, and atomic workspace history.  The public surface is
//! re-exported here as a small facade; implementation details stay in the
//! focused modules beside this file.
#![forbid(unsafe_code)]

mod coverage;
mod delta;
mod ids;
mod persistent;
mod tree;
mod workspace;

pub use coverage::{
    AdmittedProducerObservation, AuthorityScopeClaim, AuthorizedCompleteCoverage,
    ClosedRelationScope, Coverage, CoverageAdmissionError, CoverageWitness,
    ProducerObservationAdmissionError, ProducerObservationIdentity, ProducerObservationVerifier,
    ScopeEqualityBinding, ScopeRoot, UntrustedCompleteCoverage, UntrustedCoverageScope,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
    bind_scope_equality, partial_coverage,
};
pub use delta::{
    BoundDelta, CheckedStateObject, CheckedStateObjectRef, Delta, DeltaError, DeltaView, DeltaWork,
    MapChange, PreparedDelta, RelationEntry, RelationState, StateError, ValueSchema, apply_delta,
    apply_delta_with_work, canonical_delta_id, checked_canonical_delta_id, prepare_delta,
    prepare_delta_with_state, prepare_delta_with_work, prepare_internal_update_with_work,
};
pub use ids::{
    CANONICAL_CUT_POLICY_VERSION, CANONICAL_TREE_ABI, CANONICAL_VERSION, CanonicalRelation,
    CommitId, DeltaId, ID_BYTES, IdAdmissionError, IdContext, IdDecodeError, ObjectKey,
    ObjectVersion, ObjectVersionHasher, Relation, RelationDecodeError, RuntimeIdentityError,
    Schema, StateRoot, UntrustedId, WireId, WorkspaceRoot, admit_object_version_bytes,
    admit_state_root_bytes, object_version_digest, state_root_digest,
};
pub use persistent::{
    LazyPreparedUpdate, LazyTree, LazyTreeError, LazyTreePage, LazyTreeWork, NoInterner,
    PersistedTreeRoot, PersistentTree, PreparedUpdate, TreeChange, TreeError, TreeInterner,
    TreeIter, TreeNodeChildren, TreeNodeClosure, TreeNodeClosureWork, TreeNodeHandle, TreeNodeId,
    TreeNodeLoader, TreeNodeSummary, TreeNodeTraversalError, TreeNodeView, TreePath, TreeRangeIter,
    TreeWork, TreeZipper, WeakTreeNodeHandle,
};
pub use tree::{
    CanonicalChildIter, CanonicalChildWireIter, CanonicalChildWireRef, CanonicalLeafField,
    CanonicalNode, CanonicalNodeView, CanonicalRootAdmissionError, CheckedCanonicalRoot, Child,
    ChildCommitment, CommittedChild, CutError, CutPolicy, DEFAULT_CUT_POLICY, NodeError,
    admit_canonical_root, admit_canonical_root_claim, anchored_cut_points, canonical_branch,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf, canonical_root, cut_points,
};
pub use workspace::{
    BasisBinding, CheckedCommit, CheckedWorkspaceManifest, CheckedWorkspaceTransition, ClosureKind,
    ClosureRef, Commit, CommitError, CommitProvenance, MAX_WORKSPACE_WIRE_BYTES,
    MAX_WORKSPACE_WIRE_ITEMS, ObjectClosure, RelationBinding, RelationTransition,
    RelationTransitionHeader, SchemaIdentity, UntrustedClosureRef, UntrustedCommit,
    UntrustedCommitProvenance, UntrustedRelationTransition, UntrustedWorkspaceDelta,
    UntrustedWorkspaceDeltaHeader, UntrustedWorkspaceManifest, WORKSPACE_WIRE_VERSION,
    WorkspaceDecodeError, WorkspaceDelta, WorkspaceError, WorkspaceManifest, commit,
    commit_capability, commit_checked, workspace_delta,
};

#[cfg(test)]
mod tests;
