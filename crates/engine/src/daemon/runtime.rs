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

    /// Runs one fair queued operation. `false` means all lanes are empty.
    pub fn serve_one(&mut self) -> bool {
        let Some((_lane, envelope)) = self.queues.try_pop() else {
            return false;
        };
        let reply = match envelope.body {
            DaemonRequest::Commit {
                request: _request,
                expected,
                intent,
            } => self.commit(expected, intent),
            DaemonRequest::Query => {
                let workspace = self.owner.snapshot();
                let binding = if self.view_binding.workspace_root() == workspace.root()
                    && self.view_binding.is_current()
                {
                    self.view_binding.clone()
                } else if self.view_binding.state() == ViewBindingState::Unbound {
                    ViewBinding::unbound(workspace.root(), self.library.view().basis().root)
                } else {
                    ViewBinding::stale(workspace.root(), self.library.view().basis().root)
                };
                DaemonReply::Query(Box::new(Ok(QueryState {
                    workspace,
                    view: self.library.view().clone(),
                    binding,
                })))
            }
            DaemonRequest::Replicate(message) => self.replicate(*message),
            DaemonRequest::Complete(notice) => self.complete(notice),
            DaemonRequest::Subscribe { cursor, credit } => {
                self.subscribe(envelope.request_id, &cursor, credit)
            }
        };
        let _ = envelope.reply.send(reply);
        true
    }

    fn commit(&mut self, expected: HeadExpectation, intent: M::Intent) -> DaemonReply {
        let result = self
            .owner
            .prepare(expected, intent)
            .and_then(|prepared| self.owner.durable(prepared))
            .and_then(|durable| self.owner.publish(durable))
            .map(|_| self.owner.head().clone())
            .map_err(DaemonError::from);
        if let Ok(head) = &result {
            self.view_binding = ViewBinding::stale(head.root(), self.library.view().basis().root);
        }
        DaemonReply::Commit(result)
    }

    fn replicate(&mut self, message: TransportMessage) -> DaemonReply {
        let bytes = message.estimated_size();
        let result = (|| {
            message
                .validate(self.protocol.transport_limits)
                .map_err(DaemonError::Replication)?;
            let reply = match message {
                TransportMessage::WirePack(claim) => {
                    let expected_layout = crate::workspace::pack::workspace_pack_layout();
                    let pack = claim
                        .admit_against(expected_layout, self.protocol.transport_limits)
                        .map_err(DaemonError::Replication)?;
                    self.owner
                        .write_admitted_pack(&pack)
                        .map_err(DaemonError::from)?;
                    ReplicationReply::PackAdmitted { bytes }
                }
                TransportMessage::Capabilities(capabilities) => {
                    let connection = self.remote.as_ref().map(|remote| remote.connection_id());
                    self.ensure_peer_capability_connection(connection);
                    let key = capability_fingerprint(&capabilities, self.protocol.transport_limits);
                    if !self.peer_capabilities.contains(&key) {
                        let budget = self.config.replication;
                        let retained = bytes;
                        if self.peer_capabilities.len() >= budget.count
                            || self
                                .peer_capability_bytes
                                .checked_add(retained)
                                .is_none_or(|next| next > budget.bytes)
                        {
                            return Err(DaemonError::Backpressure);
                        }
                        self.peer_capabilities.insert(key);
                        self.peer_capability_manifests.insert(key, capabilities);
                        self.peer_capability_bytes = self
                            .peer_capability_bytes
                            .checked_add(retained)
                            .ok_or(DaemonError::Backpressure)?;
                    }
                    ReplicationReply::Validated { bytes }
                }
                TransportMessage::WireRecipeResult(_) => {
                    return Err(DaemonError::RemoteResultUnmatched);
                }
                message => ReplicationReply::Queued {
                    bytes: self.enqueue_replication(message)?,
                },
            };
            Ok(reply)
        })();
        DaemonReply::Replicated(result)
    }

    fn complete(&mut self, notice: CompletionNotice) -> DaemonReply {
        if let Err(error) = self.dispatcher.admit_completion_fields(
            notice.work_key,
            notice.output,
            notice.ordinal,
            notice.fence,
        ) {
            return DaemonReply::Completed(Err(DaemonError::dispatch(error)));
        }
        let result = match self.completions.get(&notice.work_key) {
            Some(previous) if previous == &notice => Ok(()),
            Some(_) => Err(DaemonError::CompletionConflict),
            None => {
                let budget = self.config.completions;
                // Completion capabilities are an LRU/FIFO retention cache.
                // Evicting an old capability revokes it in the dispatcher,
                // so an unacknowledged flood cannot wedge the daemon forever.
                loop {
                    let next = self
                        .completion_bytes
                        .checked_add(COMPLETION_RETAINED_BYTES)
                        .ok_or(DaemonError::Backpressure);
                    if self.completions.len() < budget.count
                        && next.is_ok_and(|bytes| bytes <= budget.bytes)
                    {
                        break;
                    }
                    if !self.evict_oldest_completion() {
                        return DaemonReply::Completed(Err(DaemonError::Backpressure));
                    }
                }
                let work_key = notice.work_key;
                let sequence = self
                    .completion_sequence
                    .checked_add(1)
                    .ok_or(DaemonError::Accounting);
                let Ok(sequence) = sequence else {
                    return DaemonReply::Completed(Err(DaemonError::Accounting));
                };
                self.completions.insert(work_key, notice);
                let Some(next_bytes) = self.completion_bytes.checked_add(COMPLETION_RETAINED_BYTES)
                else {
                    self.completions.remove(&work_key);
                    return DaemonReply::Completed(Err(DaemonError::Accounting));
                };
                self.completion_sequence = sequence;
                self.completion_order.insert(sequence, work_key);
                self.completion_sequences.insert(work_key, sequence);
                self.completion_bytes = next_bytes;
                Ok(())
            }
        };
        DaemonReply::Completed(result)
    }
    /// Closes all request lanes and drops remote transport ownership.
    pub fn close(&mut self) {
        self.queues.close();
        self.remote = None;
        self.remote_session = None;
        self.transport_connection = None;
        self.transport_generation = self.transport_generation.saturating_add(1);
        let _ = self.pending_remote.clear();
        self.recovered_dispatch.clear();
        self.replication.clear();
        self.replication_bytes = 0;
        self.result_inbox.clear();
        self.result_inbox_bytes = 0;
        self.quarantined_remote.clear();
        self.quarantined_remote_bytes = 0;
        for notice in self.completions.values() {
            self.dispatcher.revoke_completion_fields(
                notice.work_key,
                notice.output,
                notice.ordinal,
                notice.fence,
            );
        }
        self.completions.clear();
        self.completion_bytes = 0;
        self.completion_order.clear();
        self.completion_sequences.clear();
        self.subscriptions.clear();
        self.subscription_bytes = 0;
        self.peer_capabilities.clear();
        self.peer_capability_manifests.clear();
        self.peer_capability_bytes = 0;
        self.peer_capability_connection = None;
        self.cursor_history.clear();
        self.cursor_history_order.clear();
    }
}
