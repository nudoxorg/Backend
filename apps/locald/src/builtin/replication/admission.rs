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
        let mut transport_failed = false;
        if !self.transport_ready {
            self.ensure_transport(daemon);
        }
        if self.transport_ready && self.pending_closure.is_some() {
            if let Ok(changed) = self.poll_closure(daemon) {
                progress |= changed;
            } else {
                self.transport_ready = false;
                self.remote_root = None;
                self.pending_closure = None;
                if let Some(pending) = self.pending_dispatch.take() {
                    let _ = Self::complete_pending_locally(daemon, pending);
                }
                progress = true;
            }
        }
        if self.transport_ready {
            // `receive_remote` is a pure inbox poll for the daemon-owned
            // transport. It either admits one exact result or reports that
            // the next frame is a control message, leaving it in the bounded
            // replication queue for the prelude consumer.
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
                        | backend_engine::DaemonError::RemoteUnavailable,
                    ) => {
                        self.transport_ready = false;
                        self.remote_root = None;
                        transport_failed = true;
                        progress = true;
                        break;
                    }
                    Err(backend_engine::DaemonError::Dispatch(_)) => {
                        // A correlated result that fails exact ticket,
                        // authority, semantic, or output admission fences the
                        // peer and activates the owner-held local fallback.
                        self.transport_ready = false;
                        self.remote_root = None;
                        transport_failed = true;
                        progress = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }

        if transport_failed {
            // A disconnected stream cannot safely be rebound to an affine
            // ticket without replaying its exact request and fence. Consume
            // each owner-held ticket through its checked local fallback now;
            // a later request may use the reconnecting transport afresh.
            let failed = self.pending.take_all();
            for (key, pending) in failed {
                let bytes =
                    output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot);
                if daemon
                    .engine_mut()
                    .daemon_mut()
                    .fallback_remote_pending(key, bytes.into(), BuiltinReplication::owner_now())
                    .is_ok()
                {
                    progress = true;
                }
            }
        }

        let now = BuiltinReplication::owner_instant();
        while let Some((key, pending)) = self.pending.pop_expired(now) {
            let bytes = output_for_snapshot(pending.ids, pending.input_basis, &pending.snapshot);
            if daemon
                .engine_mut()
                .daemon_mut()
                .fallback_remote_pending(key, bytes.into(), BuiltinReplication::owner_now())
                .is_ok()
            {
                progress = true;
            }
        }
        progress
    }
}
