//! Immutable canonical ordered maps and crash-safe publication primitives.
//!
//! This crate deliberately keeps the storage model small: logical maps use a
//! persistent, key-anchored canonical tree and physical packing is kept out of
//! logical roots. Existing-key edits path-copy one leaf and its ancestors;
//! structural edits rebuild only the canonical materialization needed for the
//! new key sequence and reuse equal immutable nodes from the map's CAS.
#![forbid(unsafe_code)]

extern crate alloc;

pub(crate) use backend_version::{
    CommittedChild, DEFAULT_CUT_POLICY, DeltaId, Relation, StateRoot,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
pub(crate) use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::{Arc, Mutex},
};

/// Fixed-width digest used by physical pack and raw value references.
pub type Hash = [u8; 32];

fn digest(domain: &[u8], bytes: &[u8]) -> Hash {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(&(bytes.len() as u64).to_le_bytes());
    h.update(bytes);
    *h.finalize().as_bytes()
}

mod canonical;
mod closure;
mod delta;
mod durable;
mod pack;
mod proof;
mod residency;
mod tree;

/// Bounded first-write-wins immutable object storage in caller-selected memory.
pub mod memory;
/// Indexed immutable object-pack writing and allocation-free borrowing.
pub mod object_pack;
/// Structural validation witnesses and allocation-free borrowed frame views.
pub mod view;
/// Generic durable workflow event reduction, records, and recovery.
pub mod workflow;

pub use backend_version::CoverageWitness;
pub use canonical::{RawRelation, RawValue, StoredValue};
pub use closure::{
    CheckedRelationNode, ClosureId, ClosureManifest, ManifestChange, ManifestWork, ObjectEdge,
    ObjectId, PreparedManifestDelta, RelationAdmissionRegistry, TypedObject, UntrustedObjectId,
    WorkspaceBinding, WorkspaceClosure, admit_backend_object_version,
    verify_backend_object_version,
};
pub use delta::{Change, DiffStats, MapDelta};
pub use delta::{UpdateStats, WorkBudget};
pub use durable::{
    CheckedWorkspacePublication, DurableManifest, DurableManifestPage, DurableTree, FileDurable,
    FilePrepared, FilePublished, FileStore, GcLimits, GcReport, GcRoot, GcRoots, ManifestReadStats,
    ObjectWriteReceipt, OwnedRelationNodeLoader, PublicationAuthorityError, PublicationBase,
    PublicationDescriptor, RelationNodeChild, RelationNodeRead, RelationNodeWriteStats,
    SelectedHead, StorePublicationAuthority, TransactionId, TreeReadStats, TreeWriteStats,
    WorkspaceFileDurable, WorkspaceFilePrepared, WorkspaceFilePublished,
};
pub use pack::{
    LayoutId, Pack, PackId, WirePack, admit_pack, decode_pack, decode_wire_pack, encode_pack,
};
pub use proof::{
    KeyProof, Proof, ProofLevel, RangeProof, VerifiedKeyProof, VerifiedProof, VerifiedRangeProof,
};
pub use residency::{Pin, Residency, ResidencyConflict, ResidentPack};
pub use tree::{OrderedMap, OrderedMapIter};

pub(crate) use canonical::{
    checked_key_len, checked_wire_len, put_u32, raw_value, record_wire_len, validate_entries,
    validate_value,
};
pub(crate) use delta::{check_budget, collect_changes, delta_id_for, diff_nodes, validate_changes};
pub(crate) use proof::{key_proof, range_proof};
pub(crate) use tree::Node;
#[derive(Clone, Debug, Eq, PartialEq)]
/// Errors raised while validating or preparing store state.
pub enum StoreError {
    /// The supplied delta does not start at the map root.
    WrongBase,
    /// A change's expected before value does not match the map.
    BeforeMismatch(Vec<u8>),
    /// The resulting root differs from the delta target.
    TargetMismatch,
    /// The change sequence is not strictly ordered or canonical.
    MalformedDelta,
    /// A checked length or counter exceeded its representable bound.
    Bounds,
    /// A logical key exceeds the fixed-width canonical key-length field.
    OversizedKey,
    /// Stored canonical bytes or node metadata are inconsistent.
    Corrupt,
    /// Incremental work exceeded its publication budget.
    NeedsScopedRebuild,
    /// A delta needs an authority-produced complete coverage witness.
    IncompleteCoverage,
    /// The selected head changed after a publication was prepared.
    StaleHead,
    /// Another owner currently holds the canonical head-selection lease.
    PublicationAuthorityBusy,
    /// A filesystem operation failed while reading or publishing durable data.
    Io(String),
    /// The journal selected the publication, but synchronizing the redundant
    /// HEAD receipt failed after the selection became durable. Recovery may
    /// be retried with the returned selected head as the authoritative state.
    PublishedWithSyncPending(Box<SelectedHead>),
    /// A prepared journal frame is complete, but one of its durability syncs
    /// returned an error. Retrying may discover this exact frame by sequence
    /// and transaction identity instead of appending a duplicate.
    PreparedWithSyncPending {
        /// Sequence of the complete prepared frame in the journal.
        sequence: u64,
        /// Transaction identity bound by that frame.
        transaction: TransactionId,
    },
}

#[cfg(test)]
mod tests;
