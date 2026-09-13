//! Defines pack error store behavior for `server-index-publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack error store invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Filesystem publication, visibility, and reopen failures.

use std::{io, path::PathBuf};

use backend_version::GenerationId;
use server_index_vocabulary::{IndexPackId, IndexSnapshotId};

use super::{IndexPackEncodeError, IndexPackOpenError};

/// One storage location used by a filesystem failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexPackPathRole {
    /// Directory containing immutable content-addressed packs.
    Directory,
    /// Unique not-yet-visible temporary file.
    Temporary,
    /// Final content-addressed immutable pack path.
    Final,
}

/// One concrete filesystem operation in immutable pack publication or reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexPackStorePhase {
    /// Create the store directory.
    CreateDirectory,
    /// Create one exclusive temporary file.
    CreateTemporary,
    /// Write canonical pack bytes to the temporary file.
    WriteTemporary,
    /// Synchronize the complete temporary file before visibility.
    SyncTemporary,
    /// Link a fully stable temporary inode to its content-addressed name.
    LinkFinal,
    /// Synchronize the parent directory after a visibility transition.
    SyncDirectory,
    /// Remove a temporary path after publication or a failed attempt.
    RemoveTemporary,
    /// Read a final content-addressed immutable pack.
    ReadFinal,
}

/// Result of post-publication temporary cleanup.
pub enum IndexPackCleanup {
    /// The temporary pathname was removed after its final link was durable.
    Removed,
    /// The immutable final pack is visible, but a temporary pathname remains for later cleanup.
    Retained {
        /// Exact leftover temporary pathname.
        path: PathBuf,
        /// Exact filesystem failure from the cleanup attempt.
        source: io::Error,
    },
}

/// Immutable pack facts returned only after the final path is durable and visible.
pub struct StoredIndexPack {
    /// Content identity derived from every final pack byte.
    pub id: IndexPackId,
    /// Canonical generation selected by the pack snapshot.
    pub generation: GenerationId,
    /// Canonical immutable index snapshot stored by the pack.
    pub snapshot: IndexSnapshotId,
    /// Exact content-addressed visible pathname.
    pub path: PathBuf,
    /// Whether temporary cleanup completed after durable visibility.
    pub cleanup: IndexPackCleanup,
}

/// Exact semantic disagreement between an existing content-addressed pack and one attempted plan.
#[derive(Debug)]
pub struct IndexPackConflict {
    /// Existing pack generation.
    pub existing_generation: GenerationId,
    /// Attempted pack generation.
    pub attempted_generation: GenerationId,
    /// Existing immutable snapshot identity.
    pub existing_snapshot: IndexSnapshotId,
    /// Attempted immutable snapshot identity.
    pub attempted_snapshot: IndexSnapshotId,
}

/// Filesystem publication or reopen error with concrete phase, role, path, and source.
#[derive(Debug, thiserror::Error)]
pub enum IndexPackStoreError {
    /// One filesystem operation failed before a final pack became visible.
    #[error("index pack store {phase:?} failed for {role:?} path {path:?}")]
    Io {
        /// Concrete failed operation.
        phase: IndexPackStorePhase,
        /// Semantic role of the affected path.
        role: IndexPackPathRole,
        /// Exact affected path.
        path: PathBuf,
        /// Exact operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The primary operation and its temporary cleanup both failed.
    #[error("index pack store {phase:?} failed and temporary cleanup also failed")]
    IoAndCleanup {
        /// Concrete primary failed operation.
        phase: IndexPackStorePhase,
        /// Semantic role of the primary affected path.
        role: IndexPackPathRole,
        /// Exact primary affected path.
        path: PathBuf,
        /// Exact primary operating-system failure.
        #[source]
        source: io::Error,
        /// Exact temporary pathname that could not be cleaned.
        temporary: PathBuf,
        /// Exact cleanup operating-system failure.
        cleanup: io::Error,
    },
    /// A preflighted pack could not be encoded without violating a pack law.
    #[error("index pack encoding failed before durable visibility")]
    Encode {
        /// Exact encoding failure.
        #[source]
        source: IndexPackEncodeError,
    },
    /// Encoding failed and the private temporary pathname could not be removed.
    #[error("index pack encoding failed and its temporary pathname remains")]
    EncodeAndCleanup {
        /// Exact pack-grammar encoding failure.
        #[source]
        source: IndexPackEncodeError,
        /// Private temporary pathname retained for explicit recovery.
        temporary: PathBuf,
        /// Exact filesystem cleanup failure.
        cleanup: io::Error,
    },
    /// Every bounded temporary name was already occupied.
    #[error("index pack temporary-name admission exhausted")]
    TemporaryNameExhausted {
        /// Directory containing the temporary-name namespace.
        directory: PathBuf,
        /// Bounded number of exclusive-name attempts made.
        attempts: usize,
    },
    /// A final path may be visible, but its directory entry was not durably synchronized.
    #[error("index pack final pathname visibility could not be made durable")]
    VisibilityUncertain {
        /// Content identity named by the possibly visible final path.
        id: IndexPackId,
        /// Final content-addressed path whose parent sync failed.
        path: PathBuf,
        /// Private temporary pathname still retained to preserve recovery options.
        temporary: PathBuf,
        /// Exact parent-directory synchronization failure.
        #[source]
        source: io::Error,
    },
    /// Reopen refused a pack whose physical byte count exceeded the bounded local owner limit.
    #[error("index pack final pathname exceeds the local pack byte limit")]
    ByteLimit {
        /// Final path whose length was rejected.
        path: PathBuf,
        /// Complete observed file byte length.
        observed: u64,
        /// Maximum bytes this fixed local reader will allocate for one owner.
        limit: usize,
    },
    /// An already-visible content-addressed pack was malformed or did not match its pathname ID.
    #[error("existing content-addressed index pack is invalid")]
    ExistingInvalid {
        /// Exact final path that was opened.
        path: PathBuf,
        /// Exact pack validation failure.
        #[source]
        source: Box<IndexPackOpenError>,
    },
    /// A pre-existing final pack was invalid and the losing temporary pathname also remained.
    #[error("existing content-addressed index pack is invalid and duplicate cleanup failed")]
    ExistingInvalidAndCleanup {
        /// Exact final path that was opened.
        path: PathBuf,
        /// Exact pack validation failure.
        #[source]
        source: Box<IndexPackOpenError>,
        /// Exact losing temporary pathname retained for recovery.
        temporary: PathBuf,
        /// Exact filesystem failure from duplicate cleanup.
        cleanup: io::Error,
    },
    /// A content-addressed duplicate had valid bytes but named different semantic facts.
    #[error("content-addressed index pack duplicate conflicts with the attempted snapshot")]
    Conflict {
        /// Cold exact facts retained off the hot success/result footprint.
        facts: Box<IndexPackConflict>,
    },
    /// A valid semantic duplicate conflicted and its losing temporary pathname remained.
    #[error("content-addressed index pack duplicate conflicts and duplicate cleanup failed")]
    ConflictAndCleanup {
        /// Cold exact semantic disagreement retained off the hot success/result footprint.
        facts: Box<IndexPackConflict>,
        /// Exact losing temporary pathname retained for recovery.
        temporary: PathBuf,
        /// Exact filesystem failure from duplicate cleanup.
        cleanup: io::Error,
    },
    /// Opening an already-linked duplicate failed and its losing temporary pathname remained.
    #[error("content-addressed duplicate could not be opened and duplicate cleanup failed")]
    DuplicateOpenAndCleanup {
        /// Exact attempted reopening failure, retained without conversion to an I/O surrogate.
        #[source]
        source: Box<IndexPackStoreError>,
        /// Exact losing temporary pathname retained for recovery.
        temporary: PathBuf,
        /// Exact filesystem failure from duplicate cleanup.
        cleanup: io::Error,
    },
}
