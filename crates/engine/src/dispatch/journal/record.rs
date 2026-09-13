//! Public façade for versioned remote-dispatch record types and codec.

#[path = "codec.rs"]
mod codec;
#[path = "types.rs"]
mod types;

pub use codec::DispatchLog;
pub use types::{
    AcceptedResultProof, DISPATCH_RECORD_VERSION, DispatchAttemptKey, DispatchCursor,
    DispatchJournalLimits, DispatchPhase, DispatchRecord, DispatchRecordError,
    MAX_DISPATCH_CURSOR_BYTES, MAX_DISPATCH_PROOF_BYTES, MAX_DISPATCH_REQUEST_BYTES,
    NotificationCursor, PublicationAck, RemoteAttemptIntent, StorePublicationReceipt,
    TerminalState, TransferCheckpointRef,
};
