//! Public façade for streaming dispatch replay and restart decisions.

#[path = "error.rs"]
mod error;
#[path = "fold.rs"]
mod fold;
#[path = "journal.rs"]
mod journal;
#[path = "model.rs"]
mod model;
#[path = "record.rs"]
mod record;
#[path = "restart.rs"]
mod restart;

pub use error::{
    DispatchJournalError, RESTART_ALREADY_FENCED, RESTART_AUTHORITY_REVOKED, RESTART_LEASE_EXPIRED,
    RESTART_OWNER_TAKEOVER,
};
pub use journal::DispatchJournal;
pub use model::{
    AuthoritySnapshot, DispatchRecovery, DispatchRecoveryAction, DispatchRestartAuthority,
    DispatchRestartDecision, OwnerRestartAuthority, PublicationRecoveryMode, RecoveredAttempt,
    RestartAuthoritySnapshot,
};
pub use record::{
    AcceptedResultProof, DISPATCH_RECORD_VERSION, DispatchAttemptKey, DispatchCursor,
    DispatchJournalLimits, DispatchLog, DispatchPhase, DispatchRecord, DispatchRecordError,
    MAX_DISPATCH_CURSOR_BYTES, MAX_DISPATCH_PROOF_BYTES, MAX_DISPATCH_REQUEST_BYTES,
    NotificationCursor, PublicationAck, RemoteAttemptIntent, StorePublicationReceipt,
    TerminalState, TransferCheckpointRef,
};
