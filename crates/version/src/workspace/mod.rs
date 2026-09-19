//! Atomic workspace manifests, relation transitions, and commit history.
//!
//! A workspace root is a canonical root of selected relation roots, basis
//! inputs, authority identity, and coverage.  A [`Commit`] adds provenance and
//! parent history around that state.  Keeping the two identities separate
//! means convergent state can share a root while distinct histories retain
//! distinct commit IDs.

use core::fmt;

use crate::{
    CANONICAL_VERSION, CanonicalRelation, CommitId, Coverage, CoverageWitness, Delta, ID_BYTES,
    ObjectVersion, PersistedTreeRoot, ProducerObservationIdentity, Relation, RelationState, Schema,
    ScopeRoot, WorkspaceRoot,
    ids::{append_field, commit_id_from_digest, digest, workspace_root_from_digest},
};

include!("core.rs");
include!("manifest.rs");
include!("transition.rs");
include!("errors.rs");
include!("delta.rs");
include!("provenance.rs");
include!("commit.rs");
include!("wire.rs");
