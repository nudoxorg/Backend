mod serve;

use super::query::encode_cursor;
use super::remote::capability_fingerprint;
use super::{
    Arc, BTreeMap, BTreeSet, COMPLETION_RETAINED_BYTES, CompletionNotice, Daemon, DaemonConfig,
    DaemonError, DaemonHandle, DaemonProtocolConfig, DaemonReply, DaemonRequest, DispatchJournal,
    DispatchRecoveryAction, Dispatcher, FairQueues, HeadExpectation, Library, QueryState,
    QueueSized, ReplicationReply, TransportMessage, VecDeque, ViewBinding, ViewBindingState,
    WorkspaceModel, WorkspaceOwner, fmt, pending,
};

impl<M: WorkspaceModel, V, A> fmt::Debug for Daemon<M, V, A>
where
    M::Intent: QueueSized,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Daemon")
            .field("owner", &self.owner)
            .field("dispatcher", &self.dispatcher)
            .field("library", &self.library)
            .field("queued_replication", &self.replication.len())
            .field("queued_result_inbox", &self.result_inbox.len())
            .field("quarantined_remote", &self.quarantined_remote.len())
            .field("pending_remote", &self.pending_remote.len())
            .field("pending_remote_state", &self.pending_remote.snapshot())
            .finish_non_exhaustive()
    }
}

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    /// Creates the complete local-first composition around a configured
    /// dispatcher. Validator and authority capabilities are mandatory.
    #[must_use]
    pub fn new(
        owner: WorkspaceOwner<M>,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
    ) -> Self {
        Self::new_with_protocol(owner, dispatcher, config, DaemonProtocolConfig::default())
    }

    /// Creates the composition root with explicit versioned transport and
    /// subscription limits.
    #[must_use]
    pub fn new_with_protocol(
        owner: WorkspaceOwner<M>,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        protocol: DaemonProtocolConfig,
    ) -> Self {
        let queues = Arc::new(FairQueues::new([
            config.commands,
            config.replication,
            config.completions,
            config.subscriptions,
        ]));
        let library = Library::new();
        let cursor = library.cursor();
        let mut cursor_history = BTreeMap::new();
        let cursor_key = encode_cursor(cursor);
        cursor_history.insert(cursor_key.clone(), cursor);
        let mut cursor_history_order = VecDeque::new();
        cursor_history_order.push_back((cursor.sequence(), cursor_key));
        let view_binding = ViewBinding::unbound(owner.head().root(), library.view().basis().root);
        Self {
            owner,
            dispatcher,
            library,
            queues,
            config,
            protocol,
            remote: None,
            remote_session: None,
            replication: VecDeque::new(),
            replication_bytes: 0,
            result_inbox: BTreeMap::new(),
            result_inbox_bytes: 0,
            quarantined_remote: VecDeque::new(),
            quarantined_remote_bytes: 0,
            completions: BTreeMap::new(),
            completion_bytes: 0,
            completion_order: BTreeMap::new(),
            completion_sequences: BTreeMap::new(),
            completion_sequence: 0,
            subscriptions: BTreeMap::new(),
            subscription_bytes: 0,
            peer_capabilities: BTreeSet::new(),
            peer_capability_manifests: BTreeMap::new(),
            peer_capability_bytes: 0,
            peer_capability_connection: None,
            transport_generation: 0,
            transport_connection: None,
            pending_remote: pending::PendingAttemptRelation::default(),
            view_binding,
            view_events: Vec::new(),
            view_events_base_sequence: cursor.sequence(),
            cursor_history,
            cursor_history_order,
            dispatch_journal: None,
            recovered_dispatch: VecDeque::new(),
            view_persistence: None,
        }
    }

    /// Creates a daemon with a durable remote-dispatch sidecar and performs
    /// bounded restart classification before the owner accepts new work.
    ///
    /// The generic [`Self::new`] constructor remains useful for purely
    /// in-memory compositions. Filesystem-backed engines should use this
    /// constructor through [`crate::Engine::open`], which opens the sidecar
    /// beside the workspace journal while the owner lease is held.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new_with_dispatch_journal(
        owner: WorkspaceOwner<M>,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        journal: DispatchJournal,
        now: u64,
    ) -> Result<Self, DaemonError> {
        let mut daemon =
            Self::new_with_protocol(owner, dispatcher, config, DaemonProtocolConfig::default());
        daemon.dispatch_journal = Some(journal);
        daemon.recover_dispatch_actions(now)?;
        Ok(daemon)
    }

    /// Creates a daemon with a durable remote-dispatch sidecar and explicit
    /// transport/subscription limits.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn new_with_protocol_and_dispatch_journal(
        owner: WorkspaceOwner<M>,
        dispatcher: Dispatcher<V, A>,
        config: DaemonConfig,
        protocol: DaemonProtocolConfig,
        journal: DispatchJournal,
        now: u64,
    ) -> Result<Self, DaemonError> {
        let mut daemon = Self::new_with_protocol(owner, dispatcher, config, protocol);
        daemon.dispatch_journal = Some(journal);
        daemon.recover_dispatch_actions(now)?;
        Ok(daemon)
    }

    /// Returns the durable remote-dispatch journal when this daemon was
    /// composed with one.
    #[must_use]
    pub const fn dispatch_journal(&self) -> Option<&DispatchJournal> {
        self.dispatch_journal.as_ref()
    }

    /// Returns restart actions retained for the owner loop.
    pub fn recovered_dispatch(&self) -> impl Iterator<Item = &DispatchRecoveryAction> {
        self.recovered_dispatch.iter()
    }

    /// Transfers the bounded restart actions to the caller for replay,
    /// rebind, or local-fallback execution.
    #[must_use]
    pub fn take_recovered_dispatch(&mut self) -> Vec<DispatchRecoveryAction> {
        self.recovered_dispatch.drain(..).collect()
    }

    /// Re-evaluates durable nonterminal attempts under the caller's current
    /// authority snapshot. Fenced or expired attempts are converted to local
    /// fallback by the journal before actions are returned.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn recover_dispatch(
        &mut self,
        now: u64,
        authority: &crate::dispatch::OwnerRestartAuthority,
    ) -> Result<usize, DaemonError> {
        let owner_current = self.owner.lease().assert_current().is_ok();
        if !owner_current
            || !authority.matches_owner(
                self.owner.head().root().to_bytes(),
                self.owner.lease().epoch(),
                self.owner.lease().fence(),
            )
        {
            return Err(DaemonError::dispatch_journal(
                crate::dispatch::DispatchJournalError::Record(
                    crate::dispatch::DispatchRecordError::InvalidIdentifier,
                ),
            ));
        }
        let Some(journal) = self.dispatch_journal.take() else {
            return Ok(0);
        };
        let mut count = 0usize;
        let result = journal
            .visit_restart(now, authority, |action| {
                count = count.saturating_add(1);
                self.recovered_dispatch.push_back(action);
            })
            .map_err(DaemonError::dispatch_journal);
        self.dispatch_journal = Some(journal);
        result.map(|()| count)
    }

    fn recover_dispatch_actions(&mut self, now: u64) -> Result<(), DaemonError> {
        let journal = self.dispatch_journal.as_ref().ok_or_else(|| {
            DaemonError::dispatch_journal(crate::dispatch::DispatchJournalError::Journal(
                crate::journal::JournalError::Corrupt("missing dispatch journal"),
            ))
        })?;
        let (revocation_version, notification_cursor) = journal.authority_observation();
        let authority = self
            .owner
            .restart_authority(revocation_version, notification_cursor);
        self.recover_dispatch(now, &authority).map(|_| ())
    }

    /// Returns a bounded client handle.
    #[must_use]
    pub fn handle(&self) -> DaemonHandle<M::Intent> {
        DaemonHandle {
            queues: Arc::clone(&self.queues),
        }
    }

    /// Returns the composed dispatcher.
    #[must_use]
    pub const fn dispatcher(&self) -> &Dispatcher<V, A> {
        &self.dispatcher
    }

    /// Returns the coherent library projection.
    #[must_use]
    pub const fn library(&self) -> &Library {
        &self.library
    }
    /// Returns the number of bytes retained by the daemon's replication work
    /// queue.  This is separate from ingress queue usage because a message may
    /// outlive the request that admitted it.
    #[must_use]
    pub fn replication_backlog_bytes(&self) -> usize {
        self.replication_bytes
            .saturating_add(self.result_inbox_bytes)
            .saturating_add(self.quarantined_remote_bytes)
    }

    /// Returns the number of control messages waiting for the owner loop.
    #[must_use]
    pub fn replication_backlog_len(&self) -> usize {
        self.replication
            .len()
            .saturating_add(self.result_inbox.len())
            .saturating_add(self.quarantined_remote.len())
    }

    /// Takes one retained replication/control message for the owner loop.
    ///
    /// Every message returned here has already passed the transport shape
    /// validation performed by the daemon's replication path.  Typed root, range, and
    /// resume admission remains the responsibility of the operation that
    /// consumes the message.
    pub fn drain_replication(&mut self) -> Option<TransportMessage> {
        let message = self.replication.pop_front()?;
        let bytes = message.estimated_size();
        let Some(next) = self.replication_bytes.checked_sub(bytes) else {
            self.replication.push_front(message);
            return None;
        };
        self.replication_bytes = next;
        Some(message)
    }

    pub(super) fn enqueue_replication(
        &mut self,
        message: TransportMessage,
    ) -> Result<usize, DaemonError> {
        let bytes = message.estimated_size();
        let budget = self.config.replication;
        if self.replication.len() >= budget.count {
            return Err(DaemonError::Backpressure);
        }
        let next = self
            .replication_bytes
            .checked_add(bytes)
            .ok_or(DaemonError::Backpressure)?;
        if next > budget.bytes {
            return Err(DaemonError::Backpressure);
        }
        self.replication.push_back(message);
        self.replication_bytes = next;
        Ok(bytes)
    }
    /// Returns the owner for controlled shutdown/recovery inspection.
    #[must_use]
    pub const fn owner(&self) -> &WorkspaceOwner<M> {
        &self.owner
    }
}
