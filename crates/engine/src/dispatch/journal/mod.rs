//! Durable remote-dispatch lifecycle journal.
//!
//! The public façade keeps the record grammar and streaming replay private to
//! this boundary. Callers receive typed attempt intents, proofs, and restart
//! actions; they never manipulate a raw journal frame or a partially
//! validated phase.

mod replay;

pub use replay::{
    AcceptedResultProof, DISPATCH_RECORD_VERSION, DispatchAttemptKey, DispatchCursor,
    DispatchJournalLimits, DispatchLog, DispatchPhase, DispatchRecord, DispatchRecordError,
    MAX_DISPATCH_CURSOR_BYTES, MAX_DISPATCH_PROOF_BYTES, MAX_DISPATCH_REQUEST_BYTES,
    NotificationCursor, PublicationAck, RemoteAttemptIntent, StorePublicationReceipt,
    TerminalState, TransferCheckpointRef,
};
pub use replay::{
    AuthoritySnapshot, DispatchJournal, DispatchJournalError, DispatchRecovery,
    DispatchRecoveryAction, DispatchRestartAuthority, DispatchRestartDecision,
    OwnerRestartAuthority, PublicationRecoveryMode, RESTART_ALREADY_FENCED,
    RESTART_AUTHORITY_REVOKED, RESTART_LEASE_EXPIRED, RESTART_OWNER_TAKEOVER, RecoveredAttempt,
    RestartAuthoritySnapshot,
};

/// Descriptive alias for the bounded replay snapshot.
pub type DispatchJournalReplay = DispatchRecovery;
/// Descriptive alias for the remote-attempt journal façade.
pub type RemoteDispatchJournal = DispatchJournal;
/// Descriptive alias for one remote-dispatch transition.
pub type RemoteDispatchRecord = DispatchRecord;

#[cfg(test)]
mod tests;
