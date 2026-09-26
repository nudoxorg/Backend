//! Serves one fair daemon lane and closes the owner runtime.

use super::*;

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
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
