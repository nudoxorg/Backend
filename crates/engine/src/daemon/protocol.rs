//! Versioned daemon protocol vocabulary and error algebra.

use super::{
    Arc, CursorResetReason, DispatchError, FairQueues, HeadExpectation, PendingRemoteKey,
    QueryState, QueueBudget, QueueError, QueueLane, QueueSized, Receiver, ReplicationError,
    SyncSender, TransportLimits, TransportMessage, WorkspaceError, WorkspaceHead, fmt, mpsc,
};

/// A daemon operation lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Durable workspace intent publication.
    Commit,
    /// Read-only coherent workspace/library query.
    Query,
    /// Replication negotiation, transfer, and result admission envelope.
    Replicate,
    /// An opaque completion capability issued by the dispatcher.
    Complete,
    /// Versioned bounded cursor subscription.
    Subscribe,
}

/// A completion capability created only from a scheduler-admitted receipt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionNotice {
    pub(super) work_key: [u8; 32],
    pub(super) output: [u8; 32],
    pub(super) ordinal: u32,
    pub(super) fence: [u8; 32],
}

impl CompletionNotice {
    /// Creates a daemon completion capability from an accepted scheduler
    /// receipt.  Its fields are private so callers cannot forge a notice.
    #[must_use]
    pub fn from_receipt(receipt: &backend_execution::ScheduleReceipt) -> Self {
        Self {
            work_key: receipt.key().to_bytes(),
            output: receipt.output().to_bytes(),
            ordinal: receipt.ordinal(),
            fence: *receipt.fence().as_bytes(),
        }
    }

    /// Returns the admitted work key for diagnostics.
    #[must_use]
    pub const fn work_key(&self) -> [u8; 32] {
        self.work_key
    }

    /// Returns the admitted output identity.
    #[must_use]
    pub const fn output(&self) -> [u8; 32] {
        self.output
    }

    /// Returns the winning attempt ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the full-width attempt fence.
    #[must_use]
    pub const fn fence(&self) -> [u8; 32] {
        self.fence
    }
}

/// A bounded replication response describing the operation actually admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicationReply {
    /// A capability or root summary was structurally validated.
    Validated {
        /// Encoded bytes admitted by the transport limits.
        bytes: usize,
    },
    /// An immutable pack was admitted against the local layout and stored.
    PackAdmitted {
        /// Encoded bytes written to the owner store.
        bytes: usize,
    },
    /// A validated control message was admitted to the bounded owner queue.
    ///
    /// Queueing is an explicit protocol result.  The daemon never reports a
    /// control frame as processed merely because it copied it into an
    /// unbounded private buffer; the owner loop must drain this queue.
    Queued {
        /// Encoded bytes retained in the bounded owner queue.
        bytes: usize,
    },
}

/// A bounded cursor response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionReply {
    /// The cursor matches the current stream and was registered.
    Accepted {
        /// Number of events the caller may consume.
        credit: usize,
    },
    /// A bounded contiguous delta batch admitted from the requested cursor.
    Events {
        /// Subscription credit retained for the next batch.
        credit: usize,
        /// Cursor after the returned events.
        cursor: Box<[u8]>,
        /// Canonically checked event suffix.
        events: Box<[backend_library::CursorEvent]>,
    },
    /// The cursor is stale or cannot be resumed; caller must query/reset.
    Reset {
        /// Credit retained for a fresh subscription.
        credit: usize,
    },
    /// A reset carrying the replacement cursor/root and the typed reason.
    ResetWithRoot {
        /// Subscription credit retained after reset.
        credit: usize,
        /// Cursor of the replacement root.
        cursor: Box<[u8]>,
        /// Complete checked replacement view.
        root: Box<backend_library::ViewRoot>,
        /// Why the requested suffix could not be resumed.
        reason: CursorResetReason,
    },
}

/// Generic request accepted by the local daemon.
#[derive(Clone, Debug)]
pub enum DaemonRequest<I> {
    /// Commit one explicit durable intent.
    Commit {
        /// Client request identity.
        request: [u8; 32],
        /// Exact root/sequence observed by the client.
        expected: HeadExpectation,
        /// Checked model intent.
        intent: I,
    },
    /// Read a coherent snapshot.
    Query,
    /// Replication/control message.
    Replicate(Box<TransportMessage>),
    /// Already-admitted completion capability.
    Complete(CompletionNotice),
    /// Subscribe from a versioned cursor with bounded credit.
    Subscribe {
        /// Canonical cursor envelope. Empty selects the current cursor.
        cursor: Box<[u8]>,
        /// Maximum events retained for this subscription.
        credit: usize,
    },
}

impl<I> DaemonRequest<I> {
    /// Returns the operation lane.
    #[must_use]
    pub const fn operation(&self) -> Operation {
        match self {
            Self::Commit { .. } => Operation::Commit,
            Self::Query => Operation::Query,
            Self::Replicate(_) => Operation::Replicate,
            Self::Complete(_) => Operation::Complete,
            Self::Subscribe { .. } => Operation::Subscribe,
        }
    }
}

impl<I: QueueSized> QueueSized for DaemonRequest<I> {
    fn queue_bytes(&self) -> usize {
        self.checked_queue_bytes().unwrap_or(usize::MAX)
    }

    fn checked_queue_bytes(&self) -> Result<usize, QueueError> {
        let bytes = match self {
            Self::Commit { intent, .. } => 96usize
                .checked_add(intent.checked_queue_bytes()?)
                .ok_or(QueueError::Accounting)?,
            Self::Query => 16,
            Self::Replicate(message) => message.estimated_size(),
            Self::Complete(_) => 128,
            Self::Subscribe { cursor, .. } => 32usize
                .checked_add(cursor.len())
                .ok_or(QueueError::Accounting)?,
        };
        Ok(bytes)
    }
}

/// Daemon response.  Each variant carries the result of actual processing.
#[derive(Clone, Debug)]
pub enum DaemonReply {
    /// Commit outcome with a checked head.
    Commit(Result<WorkspaceHead, DaemonError>),
    /// Coherent workspace and library roots.
    Query(Box<Result<QueryState, DaemonError>>),
    /// Replication operation outcome.
    Replicated(Result<ReplicationReply, DaemonError>),
    /// Completion admission outcome.
    Completed(Result<(), DaemonError>),
    /// Cursor registration outcome.
    Subscribed(Result<SubscriptionReply, DaemonError>),
}

/// Bounded request envelope with one reply port.
pub struct RequestEnvelope<I: QueueSized> {
    /// Monotonic client request identity.
    pub request_id: u64,
    /// Request body.
    pub body: DaemonRequest<I>,
    pub(super) reply: SyncSender<DaemonReply>,
}

impl<I: QueueSized> fmt::Debug for RequestEnvelope<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestEnvelope")
            .field("request_id", &self.request_id)
            .field("operation", &self.body.operation())
            .field("reply", &self.reply)
            .finish()
    }
}

impl<I: QueueSized> QueueSized for RequestEnvelope<I> {
    fn queue_bytes(&self) -> usize {
        self.checked_queue_bytes().unwrap_or(usize::MAX)
    }

    fn checked_queue_bytes(&self) -> Result<usize, QueueError> {
        8usize
            .checked_add(self.body.checked_queue_bytes()?)
            .ok_or(QueueError::Accounting)
    }
}

/// Client enqueue capability. It never owns a workspace or dispatcher.
pub struct DaemonHandle<I: QueueSized> {
    pub(super) queues: Arc<FairQueues<RequestEnvelope<I>>>,
}

impl<I: QueueSized> Clone for DaemonHandle<I> {
    fn clone(&self) -> Self {
        Self {
            queues: Arc::clone(&self.queues),
        }
    }
}

impl<I: QueueSized> fmt::Debug for DaemonHandle<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DaemonHandle").finish_non_exhaustive()
    }
}

impl<I: Send + QueueSized + 'static> DaemonHandle<I> {
    /// Enqueues a request with internally derived count/byte accounting.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn submit(
        &self,
        request_id: u64,
        body: DaemonRequest<I>,
    ) -> Result<Receiver<DaemonReply>, QueueError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let lane = match body.operation() {
            Operation::Commit | Operation::Query => QueueLane::Command,
            Operation::Replicate => QueueLane::Replication,
            Operation::Complete => QueueLane::Completion,
            Operation::Subscribe => QueueLane::Subscription,
        };
        self.queues.try_push(
            lane,
            RequestEnvelope {
                request_id,
                body,
                reply,
            },
        )?;
        Ok(receiver)
    }
}

/// Four independent bounded operation budgets for daemon traffic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DaemonConfig {
    /// Command lane budget.
    pub commands: QueueBudget,
    /// Replication lane budget.
    pub replication: QueueBudget,
    /// Completion lane budget.
    pub completions: QueueBudget,
    /// Subscription lane budget.
    pub subscriptions: QueueBudget,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            commands: QueueBudget::new(256, 4 * 1024 * 1024),
            replication: QueueBudget::new(256, 16 * 1024 * 1024),
            completions: QueueBudget::new(256, 16 * 1024 * 1024),
            subscriptions: QueueBudget::new(256, 4 * 1024 * 1024),
        }
    }
}

/// Versioned protocol limits kept separate from queue budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DaemonProtocolConfig {
    /// Maximum encoded replication envelope and object-transfer bounds.
    pub transport_limits: TransportLimits,
    /// Maximum cursor event credit retained by one subscription.
    pub max_subscription_credit: usize,
}

impl Default for DaemonProtocolConfig {
    fn default() -> Self {
        Self {
            transport_limits: TransportLimits::default(),
            max_subscription_credit: backend_library::MAX_SUBSCRIPTION_EVENTS,
        }
    }
}

/// Daemon-level errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DaemonError {
    /// Workspace owner failure.
    Workspace(String),
    /// Replication envelope failure.
    Replication(ReplicationError),
    /// Queue or daemon is closed.
    Closed,
    /// A retained daemon state budget (count or bytes) is exhausted.
    Backpressure,
    /// Internal retained-state accounting did not match its owned value.
    Accounting,
    /// A completion capability disagreed with an earlier admission.
    CompletionConflict,
    /// Credit exceeded the bounded subscription envelope.
    SubscriptionCredit,
    /// Cursor schema/root could not be admitted.
    CursorInvalid,
    /// Immutable object/closure storage rejected a replication payload.
    Store(String),
    /// A remote result arrived without an owner-created pending dispatch.
    RemoteResultUnmatched,
    /// A valid remote control/capability frame was retained for the owner
    /// protocol instead of being consumed as a recipe result.
    RemoteControlBuffered,
    /// A materialized view failed its independent workspace binding proof.
    ViewAdmission(String),
    /// The lower library rejected a checked view/cursor pair.
    Library(String),
    /// No negotiated remote transport is installed.
    RemoteUnavailable,
    /// The request was admitted into the daemon's pending relation, but the
    /// current transport could not send it. The key remains live and can be
    /// resent after reconnect or switched to the owner-held local fallback.
    RemoteSendPending {
        /// Exact owner-generation key for the retained affine envelope.
        key: PendingRemoteKey,
        /// Transport failure observed while attempting the send.
        error: String,
    },
    /// Dispatcher admission or scheduler completion failed.
    Dispatch(String),
    /// Durable remote-dispatch journal failed.
    DispatchJournal(String),
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "daemon error: {self:?}")
    }
}
impl std::error::Error for DaemonError {}

impl From<WorkspaceError> for DaemonError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error.to_string())
    }
}

impl From<DispatchError> for DaemonError {
    fn from(error: DispatchError) -> Self {
        Self::Dispatch(error.to_string())
    }
}

impl From<crate::dispatch::DispatchJournalError> for DaemonError {
    fn from(error: crate::dispatch::DispatchJournalError) -> Self {
        Self::DispatchJournal(error.to_string())
    }
}

impl DaemonError {
    pub(super) fn dispatch(error: DispatchError) -> Self {
        error.into()
    }

    pub(super) fn dispatch_journal(error: crate::dispatch::DispatchJournalError) -> Self {
        error.into()
    }

    /// Returns the retained pending key when request transmission failed
    /// after owner admission. Callers can keep their product metadata and
    /// select resend or local fallback without parsing an error string.
    #[must_use]
    pub const fn retained_remote_key(&self) -> Option<PendingRemoteKey> {
        match self {
            Self::RemoteSendPending { key, .. } => Some(*key),
            _ => None,
        }
    }
}
