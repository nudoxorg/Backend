//! Workspace owner error algebra.

use super::{InjectedCrash, JournalError, PublicationStatus, StoreError, WorkspaceRoot, fmt, io};

/// Workspace failures.
#[derive(Debug)]
pub enum WorkspaceError {
    /// Filesystem failure.
    Io(String),
    /// Generic journal failure.
    Journal(JournalError),
    /// Store closure admission or durability failure.
    Store(String),
    /// Backend-version admission or commit failure.
    Version(String),
    /// Another owner currently holds the lock.
    AlreadyOwned,
    /// This lease is fenced.
    Fenced,
    /// The selected head changed.
    HeadConflict,
    /// A checked transition did not bind to the selected base/target.
    TransitionMismatch,
    /// Closure/root binding failed.
    ClosureMismatch,
    /// The deterministic transaction provenance differed.
    TransactionMismatch,
    /// Persisted bytes or journal history were corrupt.
    Corrupt(&'static str),
    /// Checked arithmetic or bounded encoding failed.
    Bounds,
    /// The model rejected an intent or persisted value.
    Model(String),
    /// A production fault seam fired.
    Injected(InjectedCrash),
    /// A manifest was not an admitted checked descriptor.
    UnverifiedManifest,
    /// Physical publication is visible but its logical acknowledgement could
    /// not yet be reconciled.  Callers must reopen/reconcile before retrying;
    /// this variant prevents the condition from being mistaken for a normal
    /// failed commit.
    PublicationPending {
        /// Checked target root that may already be physically visible.
        target: WorkspaceRoot,
        /// Physical publication generation.
        sequence: u64,
        /// Acknowledgement boundaries that still need reconciliation.
        status: PublicationStatus,
        /// Bounded diagnostic for operators and recovery logs.
        detail: String,
    },
}

impl WorkspaceError {
    pub(crate) fn io(error: io::Error) -> Self {
        let message = error.to_string();
        let _owned = Box::new(error);
        Self::Io(message)
    }

    pub(crate) fn store(error: StoreError) -> Self {
        let message = format!("{error:?}");
        let _owned = Box::new(error);
        Self::Store(message)
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "workspace error: {self:?}")
    }
}
impl std::error::Error for WorkspaceError {}
