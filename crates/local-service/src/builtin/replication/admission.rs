//! Product admission and polling implementation for the owner dispatch session.

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinValidator, EngineStatus,
    ReplicationAdmission, TransportMessage, daemon_replicate, output_for_snapshot,
};

impl ReplicationAdmission<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>
    for BuiltinReplication
{
    fn admit(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        request_id: u64,
        message: TransportMessage,
    ) -> Result<EngineStatus, crate::protocol::ProtocolError> {
        match message {
            TransportMessage::WireRecipeRequest(request) => self.dispatch(daemon, &request),
            TransportMessage::WireRecipeResult(_) => {
                Err(crate::protocol::ProtocolError::InvalidControl(
                    "a worker result requires an owner-retained dispatch ticket",
                ))
            }
            message => daemon_replicate(daemon, request_id, message),
        }
    }

    fn poll(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> bool {
        let mut progress = self.poll_recovered(daemon);
        progress |= self.poll_reconnect(daemon);
        if !self.transport_ready {
            self.ensure_transport(daemon);
        }
        let mut transport_failed = false;
        if self.transport_ready && self.pending_closure.is_some() {
            match self.poll_closure(daemon) {
                Ok(changed) => progress |= changed,
                Err(_error) => {
                    self.transport_ready = false;
                    self.remote_root = None;
                    self.pending_closure = None;
                    if let Some(pending) = self.pending_dispatch.take() {
                        let _ = self.fallback_pending_dispatch(daemon, pending);
                    }
                    transport_failed = true;
                    progress = true;
                }
            }
        }
        let (remote_progress, remote_failed) = self.poll_remote_results(daemon);
        progress |= remote_progress;
        transport_failed |= remote_failed;
        if transport_failed {
            progress |= self.fallback_failed_transport(daemon);
        }
        progress |= self.fallback_expired(daemon);
        progress
    }
}

impl BuiltinReplication {
    fn poll_remote_results(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> (bool, bool) {
        if !self.transport_ready {
            return (false, false);
        }
        // `receive_remote` is a pure inbox poll. Control messages stay in the
        // bounded replication queue for the closure consumer.
        let mut progress = false;
        for _ in 0..self.pending.len().max(1) {
            match daemon
                .engine_mut()
                .daemon_mut()
                .receive_remote(BuiltinReplication::owner_now())
            {
                Ok(completion) => {
                    progress = true;
                    if let backend_engine::DispatchCompletion::Accepted(receipt) = completion {
                        let _ = self.pending.remove_for_work(receipt.key());
                    }
                }
                Err(
                    backend_engine::DaemonError::Replication(
                        backend_engine::ReplicationError::Disconnected,
                    )
                    | backend_engine::DaemonError::RemoteUnavailable
                    | backend_engine::DaemonError::Dispatch(_),
                ) => {
                    // A disconnected stream or an invalid correlated result
                    // fences the peer before local fallback consumes tickets.
                    self.transport_ready = false;
                    self.remote_root = None;
                    return (true, true);
                }
                Err(_) => break,
            }
        }
        (progress, false)
    }

    fn fallback_failed_transport(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> bool {
        // An affine ticket cannot move to a new stream without replaying its
        // exact request and fence. Consume each through checked local fallback;
        // a later request can use a freshly negotiated generation.
        let mut progress = false;
        for key in self.pending.keys() {
            let Some(pending) = self.pending.get(key) else {
                continue;
            };
            let Ok(bytes) =
                output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot)
            else {
                continue;
            };
            if daemon
                .engine_mut()
                .fail_remote_pending(key, bytes.into(), BuiltinReplication::owner_now())
                .is_ok()
            {
                let _ = self.pending.remove(key);
                progress = true;
            }
        }
        progress
    }

    fn fallback_expired(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> bool {
        let now = BuiltinReplication::owner_instant();
        let mut progress = false;
        while let Some(key) = self.pending.next_expired(now) {
            let Some(pending) = self.pending.get(key) else {
                continue;
            };
            let Ok(bytes) =
                output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot)
            else {
                break;
            };
            if daemon
                .engine_mut()
                .fallback_remote_pending(key, bytes.into(), BuiltinReplication::owner_now())
                .is_err()
            {
                break;
            }
            let _ = self.pending.remove(key);
            progress = true;
        }
        progress
    }
}
