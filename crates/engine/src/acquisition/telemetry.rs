use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct TelemetryCounters {
    leaders: u64,
    followers: u64,
    reused_bytes: u64,
    downloaded_bytes: u64,
    retries: u64,
    breaker_transitions: u64,
    typed_rejects: u64,
    delta_rows: u64,
}

/// Bounded acquisition telemetry. Counters saturate and the snapshot is a
/// fixed-size value suitable for export without retaining labels/queues.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcquisitionTelemetry {
    /// Number of leader effects.
    pub leaders: u64,
    /// Number of coalesced followers.
    pub followers: u64,
    /// Bytes served from an admitted object.
    pub reused_bytes: u64,
    /// Bytes read from a source stream.
    pub downloaded_bytes: u64,
    /// Retry attempts.
    pub retries: u64,
    /// Breaker state transitions.
    pub breaker_transitions: u64,
    /// Typed rejects.
    pub typed_rejects: u64,
    /// Published delta rows.
    pub delta_rows: u64,
}

/// Shared telemetry handle.
#[derive(Clone, Default, Debug)]
pub struct Telemetry {
    counters: Arc<Mutex<TelemetryCounters>>,
}

impl Telemetry {
    pub(super) fn update(&self, update: impl FnOnce(&mut TelemetryCounters)) {
        let mut counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        update(&mut counters);
    }

    pub(super) fn record_follower(&self) {
        self.update(|counters| counters.followers = counters.followers.saturating_add(1));
    }

    pub(super) fn record_leader(&self) {
        self.update(|counters| counters.leaders = counters.leaders.saturating_add(1));
    }

    /// Returns the current bounded snapshot.
    #[must_use]
    pub fn snapshot(&self) -> AcquisitionTelemetry {
        let counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AcquisitionTelemetry {
            leaders: counters.leaders,
            followers: counters.followers,
            reused_bytes: counters.reused_bytes,
            downloaded_bytes: counters.downloaded_bytes,
            retries: counters.retries,
            breaker_transitions: counters.breaker_transitions,
            typed_rejects: counters.typed_rejects,
            delta_rows: counters.delta_rows,
        }
    }
}
