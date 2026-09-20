//! Bounded reconnect and authenticated worker connection state.
//!
//! Connection attempts run off the owner loop; the owner observes only a
//! bounded result channel and keeps local admission available while offline.

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinValidator, TryRecvError,
    connect_worker, mpsc, thread,
};

const MAX_RECONNECT_ATTEMPTS: u8 = 3;

impl BuiltinReplication {
    pub(super) fn ensure_transport(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) {
        self.poll_reconnect(daemon);
        if self.transport_ready {
            return;
        }
        if self.reconnect_attempts >= MAX_RECONNECT_ATTEMPTS {
            return;
        }
        if self.reconnect.is_some() {
            return;
        }
        self.start_reconnect();
    }

    pub(crate) fn start_reconnect(&mut self) {
        if self.reconnect.is_some() || self.transport_ready {
            return;
        }
        let Some(endpoint) = self
            .worker_endpoint
            .as_ref()
            .map(backend_engine::UnixEndpointPath::as_path)
        else {
            return;
        };
        let endpoint = endpoint.to_owned();
        let secret = self.worker_secret;
        let capabilities = self.capabilities.clone();
        let timeout = self.worker_timeout;
        let limits = self.limits;
        let (sender, receiver) = mpsc::sync_channel(1);
        let spawned = thread::Builder::new()
            .name("backend-locald-worker-connect".to_owned())
            .spawn(move || {
                let result = connect_worker(&endpoint, secret, &capabilities, timeout, limits)
                    .map_err(|error| error.to_string());
                let _ = sender.send(result);
            });
        if spawned.is_ok() {
            self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
            self.reconnect = Some(receiver);
        } else {
            self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
            self.transport_ready = false;
        }
    }

    pub(super) fn poll_reconnect(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> bool {
        let Some(receiver) = self.reconnect.take() else {
            return false;
        };
        match receiver.try_recv() {
            Ok(Ok(transport)) => {
                daemon.set_remote_transport(transport);
                self.transport_ready = true;
                self.reconnect_attempts = 0;
                // A transport connection is a fresh peer session. The
                // worker's durable CAS may be warm, but that fact is not
                // authenticated until the new closure prelude completes.
                self.remote_root = None;
                true
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.transport_ready = false;
                self.remote_root = None;
                false
            }
            Err(TryRecvError::Empty) => {
                self.reconnect = Some(receiver);
                false
            }
        }
    }
}
