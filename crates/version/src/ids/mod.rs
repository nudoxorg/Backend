//! Domain separated schemas and fixed width version identities.

/// Width of every canonical digest in bytes.
pub const ID_BYTES: usize = 32;
/// Current canonical value encoding version.
pub const CANONICAL_VERSION: u8 = 1;
/// ABI tag for canonical tree nodes and cut anchors.
pub const CANONICAL_TREE_ABI: u8 = 1;
/// Version of the canonical key anchored cut policy.
pub const CANONICAL_CUT_POLICY_VERSION: u8 = 2;

const HASH_DOMAIN: &[u8] = b"backend.version.v2\0";
const CLASS_OBJECT_KEY: u8 = 0x4b;
const CLASS_OBJECT_VERSION: u8 = 0x56;
const CLASS_STATE_ROOT: u8 = 0x52;
const CLASS_DELTA: u8 = 0x44;

mod context;
mod hash;
mod identity;
mod schema;
mod wire;

pub use context::IdContext;
pub use hash::{
    ObjectVersionHasher, RuntimeIdentityError, admit_object_version_bytes, admit_state_root_bytes,
    object_version_digest, state_root_digest,
};
pub use identity::{CommitId, DeltaId, ObjectKey, ObjectVersion, StateRoot, WorkspaceRoot};
pub use schema::{CanonicalRelation, Relation, RelationDecodeError, Schema};
pub use wire::{IdAdmissionError, IdDecodeError, UntrustedId, WireId};

pub(crate) use hash::{
    append_field, canonical_version_delta_id, commit_id_from_digest, digest,
    state_root_from_digest, workspace_root_from_digest,
};
