//! Sole durable workspace owner façade.
//!
//! The owner data layout and pin lifetime live here; state transitions are
//! split into private modules by responsibility.  Construction/restart, GC,
//! checked transaction admission, durable publication, and diagnostic
//! recovery each have their own implementation module while the public type
//! remains one capability boundary.

pub(crate) use super::catalog::{
    CatalogState, DerivedOutputProof, DerivedOutputPublication, LatestQuery, StagedDerivedOutput,
    append_to_closure, find_latest_in_state, find_proof_in_state,
};
pub(crate) use super::head::{HeadExpectation, WorkspaceHead, WorkspaceSnapshot};
pub(crate) use super::model::WorkspaceModel;
pub(crate) use super::pack::verify_workspace_pack;
pub(crate) use super::publication::{
    DurablePublication, PreparedPublication, Publication, PublicationData, PublicationStatus,
    PublishedPublication,
};
pub(crate) use super::record::{encode_prepared, encode_published, encode_select};
pub(crate) use super::transition::{PreparedTransition, TransactionId};
pub(crate) use crate::fault::{Boundary, Faults, InjectedCrash};
pub(crate) use crate::journal::{
    ChainHash, HashChainJournal, JournalCodec, JournalError, JournalLimits,
};
pub(crate) use crate::schema::{RecordId, WorkspaceLog};
pub(crate) use backend_store::{
    FileStore, GcLimits, GcReport, GcRoot, GcRoots, ObjectId, RelationAdmissionRegistry,
    SelectedHead, StoreError, TypedObject, UntrustedObjectId,
};
pub(crate) use backend_version::{
    CommitProvenance, ObjectClosure as VersionObjectClosure, WorkspaceRoot, commit_capability,
    commit_checked, workspace_delta,
};
pub(crate) use std::collections::BTreeMap;
pub(crate) use std::fmt;
pub(crate) use std::fs::{self, OpenOptions};
pub(crate) use std::io;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::atomic::AtomicU64;
pub(crate) use std::sync::{Arc, Mutex};

mod error;
pub use error::WorkspaceError;
mod authority;
mod gc;
mod lease;
mod lifecycle;
mod publication;
mod recovery;
mod transaction;
pub(crate) use super::recovery::{open_diagnostic_journal, recover_store_head};
pub use lease::OwnerLease;
pub(crate) use recovery::{store_head_matches, sync_directory, write_diagnostic};

const DIAGNOSTIC_FILE: &str = "workspace.diagnostic";
const DIAGNOSTIC_MAGIC: &[u8] = b"BENGINE_DIAGNOSTIC1\0";
const JOURNAL_FILE: &str = "workspace.journal";
const MAX_STORE_BYTES: usize = 64 * 1024 * 1024;
const MAX_JOURNAL_FRAMES: usize = 1_000_000;
const MAX_JOURNAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_OWNER_GC_PINS: usize = 16_384;

/// A bounded physical root retained by one live engine reader, transfer,
/// checkpoint, replication claim, or pending operation. Dropping the token
/// releases the root for the next complete owner snapshot.
#[must_use]
pub struct WorkspaceGcPin {
    id: u64,
    roots: Arc<Mutex<BTreeMap<u64, GcRoot>>>,
}

impl fmt::Debug for WorkspaceGcPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceGcPin")
            .field("id", &self.id)
            .field("roots", &self.roots)
            .finish()
    }
}

impl Drop for WorkspaceGcPin {
    fn drop(&mut self) {
        if let Ok(mut roots) = self.roots.lock() {
            roots.remove(&self.id);
        }
    }
}

/// Filesystem-backed sole owner.
pub struct WorkspaceOwner<M: WorkspaceModel> {
    model: M,
    directory: PathBuf,
    lease: OwnerLease,
    store: Arc<FileStore>,
    journal: HashChainJournal<WorkspaceLog>,
    head: WorkspaceHead,
    catalog: CatalogState,
    faults: Arc<Faults>,
    gc_roots: Arc<Mutex<BTreeMap<u64, GcRoot>>>,
    next_gc_pin: AtomicU64,
}

impl<M: WorkspaceModel> fmt::Debug for WorkspaceOwner<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkspaceOwner")
            .field("directory", &self.directory)
            .field("head", &self.head)
            .field("epoch", &self.lease.epoch())
            .finish_non_exhaustive()
    }
}
