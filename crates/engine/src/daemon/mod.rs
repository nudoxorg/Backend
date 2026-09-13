//! Single-owner daemon composition and bounded request protocol.
//!
//! The daemon is the only component that owns a workspace owner, dispatcher,
//! library cursor state, and replication transport.  Clients enqueue typed
//! envelopes; the fair queues derive their byte charge from the owned value.
//! Every lane validates and processes its message before sending a one-shot
//! reply, so replication, completion, and subscription traffic cannot be
//! successful no-op stubs.

use crate::dispatch::{
    CompleteSemanticCoverage, DispatchAttemptKey, DispatchCompletion, DispatchError,
    DispatchJournal, DispatchPlan, DispatchRecoveryAction, DispatchTicket, Dispatcher,
    PendingRemoteEnvelope, PendingRemoteKey, RemoteCorrelationKey, RemoteDispatchContract,
    RemoteTransport, TerminalState,
};
use crate::queue::{FairQueues, QueueBudget, QueueError, QueueLane, QueueSized};
use crate::workspace::{
    DerivedOutputEntry, HeadExpectation, WorkspaceError, WorkspaceHead, WorkspaceModel,
    WorkspaceOwner, WorkspaceSnapshot,
};
use backend_execution::ScheduleRequest;
use backend_library::{Cursor, CursorRead, CursorResetReason, CursorSub, Library};
use backend_replication::{
    CapabilityManifest, NegotiatedCapabilities, ReplicationError, TransportLimits,
    TransportMessage, WireRecipeRequest, WireRecipeResult,
};
use backend_version::Relation;
use backend_version::WorkspaceRoot;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};

const COMPLETION_RETAINED_BYTES: usize = 128;
/// Durable sidecar containing remote intent and publication state.
pub const DISPATCH_JOURNAL_FILE: &str = "dispatch.journal";

mod completion;
mod pending;
mod protocol;
mod query;
mod remote;
mod runtime;

/// Sole owner event loop and composition root.
pub struct Daemon<M: WorkspaceModel, V, A>
where
    M::Intent: QueueSized,
{
    owner: WorkspaceOwner<M>,
    dispatcher: Dispatcher<V, A>,
    library: Library,
    queues: Arc<FairQueues<RequestEnvelope<M::Intent>>>,
    config: DaemonConfig,
    protocol: DaemonProtocolConfig,
    remote: Option<Box<dyn RemoteTransport>>,
    remote_session: Option<remote::RemoteSession>,
    replication: VecDeque<TransportMessage>,
    replication_bytes: usize,
    /// Results routed by exact work/attempt correlation. Result frames never
    /// share the control queue, so a control prelude cannot head-of-line block
    /// a pending completion and an out-of-order result is retained for its
    /// own waiter.
    result_inbox: BTreeMap<RemoteCorrelationKey, Box<WireRecipeResult>>,
    result_inbox_bytes: usize,
    /// Bounded diagnostics for unowned, duplicate, or correlation-invalid
    /// result frames. Quarantine is single-consumption: frames are never
    /// re-enqueued onto a live dispatch path.
    quarantined_remote: VecDeque<TransportMessage>,
    quarantined_remote_bytes: usize,
    completions: BTreeMap<[u8; 32], CompletionNotice>,
    completion_bytes: usize,
    completion_order: BTreeMap<u64, [u8; 32]>,
    completion_sequences: BTreeMap<[u8; 32], u64>,
    completion_sequence: u64,
    subscriptions: BTreeMap<u64, CursorSub>,
    subscription_bytes: usize,
    peer_capabilities: BTreeSet<[u8; 32]>,
    peer_capability_manifests: BTreeMap<[u8; 32], CapabilityManifest>,
    peer_capability_bytes: usize,
    peer_capability_connection: Option<u64>,
    /// Locally observed transport generation. It is independent of the
    /// peer's numeric connection id so replacing a transport with an
    /// implementation that starts at the same id still fences its inbox.
    transport_generation: u64,
    transport_connection: Option<u64>,
    pending_remote: pending::PendingAttemptRelation<V, A>,
    view_binding: ViewBinding,
    view_events: Vec<backend_library::CursorEvent>,
    view_events_base_sequence: u64,
    cursor_history: BTreeMap<Vec<u8>, Cursor>,
    cursor_history_order: VecDeque<(u64, Vec<u8>)>,
    /// Durable remote lifecycle state. Direct daemon compositions may opt in
    /// through [`Daemon::new_with_dispatch_journal`]; [`Engine::open`] always
    /// installs this sidecar for filesystem-backed operation.
    dispatch_journal: Option<DispatchJournal>,
    /// Bounded restart work selected from the durable sidecar. The owner loop
    /// drains these actions before accepting new remote work.
    recovered_dispatch: VecDeque<DispatchRecoveryAction>,
    /// Optional product-owned durable view/event sink. It is called before
    /// the in-memory library advances, making notification publication
    /// restartable without baking a product wire grammar into the engine.
    view_persistence: Option<Box<dyn ViewPersistence>>,
}

pub use protocol::{
    CompletionNotice, DaemonConfig, DaemonError, DaemonHandle, DaemonProtocolConfig, DaemonReply,
    DaemonRequest, Operation, ReplicationReply, RequestEnvelope, SubscriptionReply,
};
pub use query::{QueryState, ViewBinding, ViewBindingAdmission, ViewBindingState, ViewPersistence};
