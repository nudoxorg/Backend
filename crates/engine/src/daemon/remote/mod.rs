//! Remote dispatch façade.
//!
//! The owner-facing methods are split by durable responsibility: transport
//! generation and bounded inbox state, journal transitions, request
//! negotiation, and result/fallback completion. Each child contains typed
//! `Daemon` impls; no child can construct a daemon or bypass its owner fields.

use super::{
    Arc, CapabilityManifest, Daemon, DaemonError, DispatchAttemptKey, DispatchCompletion,
    DispatchError, DispatchPlan, DispatchTicket, NegotiatedCapabilities, PendingRemoteEnvelope,
    PendingRemoteKey, QueueSized, Relation, RemoteCorrelationKey, RemoteDispatchContract,
    RemoteTransport, ReplicationError, TerminalState, TransportLimits, TransportMessage,
    WireRecipeRequest, WorkspaceModel,
};
use crate::dispatch::{
    AcceptedResultProof, DispatchPhase, NotificationCursor, PublicationAck, RemoteAttemptIntent,
    StorePublicationReceipt, TransferCheckpointRef,
};
use backend_execution::OutputVersion;
use backend_replication::WireRecipeResult;

const JOURNAL_CANCEL_SEND_FAILED: u16 = 1;
const JOURNAL_CANCEL_REQUESTED: u16 = 2;
const JOURNAL_FALLBACK_SELECTED: u16 = 3;

#[derive(Clone, Debug)]
pub(super) struct RemoteSession {
    connection: u64,
    local_fingerprint: [u8; 32],
    negotiated: NegotiatedCapabilities,
}

mod journal;
mod result;
mod state;
mod transport;

// These are the only cross-module seams: runtime needs the session state and
// the canonical capability fingerprint, while all lifecycle methods remain
// on the daemon façade.
pub(crate) use transport::capability_fingerprint;
