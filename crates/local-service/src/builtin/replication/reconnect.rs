//! Bounded reconnect and authenticated worker connection state.
//!
//! Connection attempts run off the owner loop; the owner observes only a
//! bounded result channel and keeps local admission available while offline.

use std::time::{Duration, Instant};

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinValidator, TryRecvError,
    connect_worker, mpsc, thread,
};

const MAX_RECONNECT_ATTEMPTS: u8 = 3;
const RECONNECT_BASE_DELAY: Duration = Duration::from_millis(25);
const RECONNECT_GENERATION_COOLDOWN: Duration = Duration::from_millis(250);

/// Owner-thread circuit for bounded reconnect generations.
///
/// The counter limits churn within one burst. Crossing the limit opens the
/// circuit only until a finite cooldown; the following poll starts a new
/// generation, so a worker that appears later is eventually discovered.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReconnectCircuit {
    failures: u8,
    generation: u64,
    not_before: Instant,
}

impl ReconnectCircuit {
    pub(super) const fn new(now: Instant) -> Self {
        Self {
            failures: 0,
            generation: 0,
            not_before: now,
        }
    }

    fn can_probe(self, now: Instant) -> bool {
        now >= self.not_before
    }

    fn failed(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= MAX_RECONNECT_ATTEMPTS {
            self.failures = 0;
            self.generation = self.generation.saturating_add(1);
            self.not_before = now + RECONNECT_GENERATION_COOLDOWN;
        } else {
            let shift = u32::from(self.failures.saturating_sub(1));
            let multiplier = 1_u32.checked_shl(shift).unwrap_or(u32::MAX);
            self.not_before = now + RECONNECT_BASE_DELAY.saturating_mul(multiplier);
        }
    }

    fn connected(&mut self, now: Instant) {
        self.failures = 0;
        self.not_before = now;
    }
}

impl BuiltinReplication {
    pub(super) fn ensure_transport(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) {
        self.poll_reconnect(daemon);
        if self.transport_ready {
            return;
        }
        if self.reconnect.is_some() {
            return;
        }
        self.start_reconnect_at(Instant::now());
    }

    pub(crate) fn start_reconnect(&mut self) {
        self.start_reconnect_at(Instant::now());
    }

    fn start_reconnect_at(&mut self, now: Instant) {
        if self.reconnect.is_some() || self.transport_ready {
            return;
        }
        if !self.reconnect_circuit.can_probe(now) {
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
            self.reconnect = Some(receiver);
        } else {
            self.reconnect_circuit.failed(now);
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
                self.reconnect_circuit.connected(Instant::now());
                // A transport connection is a fresh peer session. The
                // worker's durable CAS may be warm, but that fact is not
                // authenticated until the new closure prelude completes.
                self.remote_root = None;
                true
            }
            Ok(Err(_)) | Err(TryRecvError::Disconnected) => {
                self.transport_ready = false;
                self.remote_root = None;
                self.reconnect_circuit.failed(Instant::now());
                false
            }
            Err(TryRecvError::Empty) => {
                self.reconnect = Some(receiver);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_worker_is_probed_after_every_failed_generation() {
        let start = Instant::now();
        let mut circuit = ReconnectCircuit::new(start);
        let mut now = start;
        for generation in 0..4_u64 {
            for _ in 0..MAX_RECONNECT_ATTEMPTS {
                assert!(circuit.can_probe(now));
                circuit.failed(now);
                now = circuit.not_before;
            }
            assert_eq!(circuit.generation, generation + 1);
            assert!(circuit.can_probe(now));
        }
        circuit.connected(now);
        assert_eq!(circuit.failures, 0);
        assert!(circuit.can_probe(now));
    }
}
