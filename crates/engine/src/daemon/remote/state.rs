//! Transport generation, inbox routing, and bounded quarantine.

use super::{
    Daemon, DaemonError, PendingRemoteEnvelope, PendingRemoteKey, QueueSized, RemoteCorrelationKey,
    RemoteTransport, TransportMessage, WireRecipeResult, WorkspaceModel,
};

impl<M, V, A> Daemon<M, V, A>
where
    M: WorkspaceModel,
    M::Intent: QueueSized,
    V: crate::dispatch::OutputAdmissionValidator + crate::dispatch::SemanticCoverageValidator,
    A: backend_replication::AttestationVerifier + Send + Sync + 'static,
{
    pub(crate) fn quarantine_result_inbox(&mut self) {
        let results = std::mem::take(&mut self.result_inbox);
        self.result_inbox_bytes = 0;
        for (correlation, result) in results {
            if let Some(key) = self.pending_remote.key_for_correlation(&correlation)
                && let Ok(bytes) = Self::wire_result_size(&result)
            {
                let _ = self.pending_remote.release_result_bytes(key, bytes);
            }
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
        }
    }

    pub(crate) fn observe_transport_generation(&mut self, connection: Option<u64>) {
        if self.transport_connection == connection {
            return;
        }
        self.transport_connection = connection;
        self.transport_generation = self.transport_generation.saturating_add(1);
        self.remote_session = None;
        // Preserve pending affine envelopes for resend or local fallback, but
        // never let a result received under the previous connection consume a
        // ticket on the new one.
        self.quarantine_result_inbox();
    }

    pub(crate) fn wire_result_size(result: &WireRecipeResult) -> Result<usize, DaemonError> {
        result.retained_size().map_err(|_| DaemonError::Accounting)
    }

    fn retained_message_size(message: &TransportMessage) -> Result<usize, DaemonError> {
        match message {
            TransportMessage::WireRecipeResult(result) => Self::wire_result_size(result),
            // Control frames are already bounded by the negotiated transport
            // limits. Their protocol estimate is sufficient for diagnostics,
            // while result frames use the exact Arc/vector allocation above.
            other => Ok(other.estimated_size()),
        }
    }

    /// Retains one invalid or unowned frame for bounded diagnostics. The
    /// frame is consumed exactly once; when the quarantine is full its oldest
    /// diagnostic is evicted before the new one is retained.
    pub(crate) fn quarantine_remote(&mut self, message: TransportMessage) {
        let Ok(bytes) = Self::retained_message_size(&message) else {
            return;
        };
        let budget = self.config.replication;
        if budget.count == 0 || bytes > budget.bytes {
            return;
        }
        while self.quarantined_remote.len() >= budget.count
            || self
                .quarantined_remote_bytes
                .checked_add(bytes)
                .is_some_and(|next| next > budget.bytes)
        {
            let Some(oldest) = self.quarantined_remote.pop_front() else {
                break;
            };
            let Ok(old_bytes) = Self::retained_message_size(&oldest) else {
                self.quarantined_remote.clear();
                self.quarantined_remote_bytes = 0;
                break;
            };
            if let Some(next) = self.quarantined_remote_bytes.checked_sub(old_bytes) {
                self.quarantined_remote_bytes = next;
            } else {
                // The queue and its byte projection are one owner state. If
                // an externally corrupted diagnostic size is observed, drop
                // the retained diagnostics together rather than silently
                // saturating the accounting projection.
                self.quarantined_remote.clear();
                self.quarantined_remote_bytes = 0;
                break;
            }
        }
        if let Some(next) = self.quarantined_remote_bytes.checked_add(bytes)
            && next <= budget.bytes
        {
            self.quarantined_remote.push_back(message);
            self.quarantined_remote_bytes = next;
        }
    }

    /// Routes one received frame into its single owner queue. Result frames
    /// are admitted to the inbox only when an exact pending work/attempt has
    /// already been registered and its fence/cancellation match. Everything
    /// else goes to bounded quarantine and can never head-of-line block a
    /// later valid frame.
    pub(crate) fn route_remote_message(
        &mut self,
        message: TransportMessage,
    ) -> Result<(), DaemonError> {
        let TransportMessage::WireRecipeResult(result) = message else {
            self.enqueue_replication(message)?;
            return Ok(());
        };
        let correlation = RemoteCorrelationKey::new(result.work_key, result.attempt);
        let Some(key) = self.pending_remote.key_for_correlation(&correlation) else {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        };
        let Some(pending) = self.pending_remote.get(&key) else {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        };
        if !pending.matches(&result) || self.result_inbox.contains_key(&correlation) {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        }
        let bytes = Self::wire_result_size(&result)?;
        let budget = self.config.replication;
        let Some(next) = self.result_inbox_bytes.checked_add(bytes) else {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        };
        if self.result_inbox.len() >= budget.count || next > budget.bytes {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        }
        if self
            .pending_remote
            .charge_result_bytes(key, bytes, budget.bytes)
            .is_err()
        {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
            return Ok(());
        }
        self.result_inbox.insert(correlation, result);
        self.result_inbox_bytes = next;
        Ok(())
    }

    pub(crate) fn take_result(
        &mut self,
        correlation: RemoteCorrelationKey,
    ) -> Option<Box<WireRecipeResult>> {
        let result = self.result_inbox.remove(&correlation)?;
        let Ok(bytes) = Self::wire_result_size(&result) else {
            self.result_inbox.insert(correlation, result);
            return None;
        };
        let Some(next) = self.result_inbox_bytes.checked_sub(bytes) else {
            self.result_inbox.insert(correlation, result);
            return None;
        };
        self.result_inbox_bytes = next;
        Some(result)
    }

    pub(crate) fn take_any_result(&mut self) -> Option<(PendingRemoteKey, Box<WireRecipeResult>)> {
        loop {
            let correlation = self.result_inbox.keys().next().copied()?;
            let Some(key) = self.pending_remote.key_for_correlation(&correlation) else {
                // A cancellation or fallback may retire an envelope while a
                // result is already buffered. Consume that stale frame once
                // so it cannot head-of-line block another pending attempt.
                if let Some(result) = self.take_result(correlation) {
                    self.quarantine_remote(TransportMessage::WireRecipeResult(result));
                }
                continue;
            };
            let result = self.take_result(correlation)?;
            return Some((key, result));
        }
    }

    pub(crate) fn recv_remote_message(&mut self) -> Result<Option<TransportMessage>, DaemonError> {
        let connection = self.remote.as_ref().map(|remote| remote.connection_id());
        self.observe_transport_generation(connection);
        let Some(remote) = self.remote.as_mut() else {
            return Err(DaemonError::RemoteUnavailable);
        };
        remote.recv().map_err(DaemonError::Replication)
    }

    /// Returns a quarantined frame for diagnostics without putting it back on
    /// a dispatch queue. This is intentionally separate from
    /// [`Self::drain_replication`], which exposes control messages only.
    pub fn drain_remote_quarantine(&mut self) -> Option<TransportMessage> {
        let message = self.quarantined_remote.pop_front()?;
        let Ok(bytes) = Self::retained_message_size(&message) else {
            self.quarantined_remote.clear();
            self.quarantined_remote_bytes = 0;
            return Some(message);
        };
        if let Some(next) = self.quarantined_remote_bytes.checked_sub(bytes) {
            self.quarantined_remote_bytes = next;
        } else {
            self.quarantined_remote.clear();
            self.quarantined_remote_bytes = 0;
        }
        Some(message)
    }

    /// Installs a negotiated remote transport path.  The daemon still owns
    /// all result admission and publication decisions.
    pub fn set_remote_transport(&mut self, transport: Box<dyn RemoteTransport>) {
        self.remote = Some(transport);
        let connection = self.remote.as_ref().map(|remote| remote.connection_id());
        self.transport_connection = None;
        self.observe_transport_generation(connection);
        self.remote_session = None;
        self.clear_peer_capabilities();
    }
    pub(crate) fn clear_peer_capabilities(&mut self) {
        self.peer_capabilities.clear();
        self.peer_capability_manifests.clear();
        self.peer_capability_bytes = 0;
        self.peer_capability_connection = self.remote.as_ref().map(|remote| remote.connection_id());
    }

    pub(crate) fn ensure_peer_capability_connection(&mut self, connection: Option<u64>) {
        if self.peer_capability_connection != connection {
            self.peer_capabilities.clear();
            self.peer_capability_manifests.clear();
            self.peer_capability_bytes = 0;
            self.peer_capability_connection = connection;
        }
    }
    pub(crate) fn remove_pending_remote(
        &mut self,
        key: PendingRemoteKey,
    ) -> Result<PendingRemoteEnvelope<V, A>, DaemonError> {
        self.discard_buffered_result(key);
        self.pending_remote.remove(key)
    }

    pub(crate) fn discard_buffered_result(&mut self, key: PendingRemoteKey) {
        let Some(correlation) = self
            .pending_remote
            .get(&key)
            .map(PendingRemoteEnvelope::correlation)
        else {
            return;
        };
        if let Some(result) = self.take_result(correlation) {
            self.quarantine_remote(TransportMessage::WireRecipeResult(result));
        }
    }
}
