//! Bounded, optional runtime telemetry with closed dimensions.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const FAMILY_COUNT: usize = 7;

/// Stable operational families. Closed variants prevent unbounded label cardinality.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MetricFamily {
    /// Scheduler admission, completion, cancellation, and failure.
    Lifecycle = 0,
    /// Journal replay, repair, and restart recovery.
    Recovery = 1,
    /// Queue occupancy, rejection, and wait work.
    Queue = 2,
    /// Lexical, graph, and semantic search work.
    Search = 3,
    /// Parser, compiler, and language-oracle work.
    Compiler = 4,
    /// Remote dispatch, replication, and fallback work.
    Remote = 5,
    /// Local-service and worker transport work.
    Transport = 6,
}

/// Closed terminal dimension shared by every metric family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricOutcome {
    /// Work completed and its result was accepted.
    Completed,
    /// Work was rejected before execution.
    Rejected,
    /// Work failed after admission.
    Failed,
    /// Work was cancelled or became stale.
    Cancelled,
}

/// One allocation-free observation supplied lazily by a caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Observation {
    /// Semantic family of the operation.
    pub family: MetricFamily,
    /// Closed terminal outcome.
    pub outcome: MetricOutcome,
    /// Monotonic elapsed time.
    pub latency: Duration,
    /// Bounded work units such as bytes, rows, or queue entries.
    pub units: u64,
}

/// Point-in-time counters for one family.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FamilySnapshot {
    /// Accepted completions.
    pub completed: u64,
    /// Pre-execution rejections.
    pub rejected: u64,
    /// Post-admission failures.
    pub failed: u64,
    /// Cancellation or stale terminals.
    pub cancelled: u64,
    /// Sum of observed work units.
    pub units: u64,
    /// Sum of latency in microseconds.
    pub latency_micros: u64,
    /// Largest observed latency in microseconds.
    pub max_latency_micros: u64,
}

/// Constant-cardinality process telemetry snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TelemetrySnapshot {
    families: [FamilySnapshot; FAMILY_COUNT],
    /// Export attempts that failed without affecting product work.
    pub exporter_failures: u64,
}

impl TelemetrySnapshot {
    /// Returns counters for one closed family.
    #[must_use]
    pub const fn family(&self, family: MetricFamily) -> FamilySnapshot {
        self.families[family as usize]
    }
}

/// Optional snapshot destination. Export is never called from correctness paths.
pub trait TelemetryExporter {
    /// Exporter-specific failure retained only as telemetry health.
    type Error;

    /// Publishes one already bounded snapshot.
    ///
    /// # Errors
    ///
    /// Returns an exporter-specific error when the destination rejects the snapshot.
    fn export(&mut self, snapshot: TelemetrySnapshot) -> Result<(), Self::Error>;
}

/// Result of an isolated export attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportStatus {
    /// Telemetry is disabled, so the exporter was not invoked.
    Disabled,
    /// Snapshot was accepted by the exporter.
    Exported,
    /// Exporter failed; product work remains unaffected.
    Failed,
}

/// Cloneable optional telemetry capability.
#[derive(Clone, Default)]
pub struct Telemetry(Option<Arc<Counters>>);

impl Telemetry {
    /// Creates behaviorally inert telemetry.
    #[must_use]
    pub const fn disabled() -> Self {
        Self(None)
    }

    /// Creates fixed-cardinality lock-free counters.
    #[must_use]
    pub fn enabled() -> Self {
        Self(Some(Arc::new(Counters::new())))
    }

    /// Records an observation lazily. Disabled telemetry never evaluates the builder.
    pub fn record_with(&self, build: impl FnOnce() -> Observation) {
        let Some(counters) = &self.0 else {
            return;
        };
        counters.record(build());
    }

    /// Reads a fixed-size snapshot. Disabled telemetry returns explicit zero counters.
    #[must_use]
    pub fn snapshot(&self) -> TelemetrySnapshot {
        self.0
            .as_deref()
            .map_or_else(TelemetrySnapshot::default, Counters::snapshot)
    }

    /// Attempts export and isolates exporter failure from all product state.
    pub fn export<E: TelemetryExporter>(&self, exporter: &mut E) -> ExportStatus {
        if self.0.is_none() {
            return ExportStatus::Disabled;
        }
        if exporter.export(self.snapshot()).is_ok() {
            return ExportStatus::Exported;
        }
        if let Some(counters) = &self.0 {
            increment(&counters.exporter_failures, 1);
        }
        ExportStatus::Failed
    }
}

struct Counters {
    families: [FamilyCounters; FAMILY_COUNT],
    exporter_failures: AtomicU64,
}

impl Counters {
    fn new() -> Self {
        Self {
            families: std::array::from_fn(|_| FamilyCounters::default()),
            exporter_failures: AtomicU64::new(0),
        }
    }

    fn record(&self, observation: Observation) {
        let family = &self.families[observation.family as usize];
        increment(family.terminal(observation.outcome), 1);
        increment(&family.units, observation.units);
        let micros = u64::try_from(observation.latency.as_micros()).unwrap_or(u64::MAX);
        increment(&family.latency_micros, micros);
        family
            .max_latency_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    fn snapshot(&self) -> TelemetrySnapshot {
        TelemetrySnapshot {
            families: std::array::from_fn(|index| self.families[index].snapshot()),
            exporter_failures: self.exporter_failures.load(Ordering::Relaxed),
        }
    }
}

fn increment(counter: &AtomicU64, value: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(value))
    });
}

#[derive(Default)]
struct FamilyCounters {
    completed: AtomicU64,
    rejected: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    units: AtomicU64,
    latency_micros: AtomicU64,
    max_latency_micros: AtomicU64,
}

impl FamilyCounters {
    fn terminal(&self, outcome: MetricOutcome) -> &AtomicU64 {
        match outcome {
            MetricOutcome::Completed => &self.completed,
            MetricOutcome::Rejected => &self.rejected,
            MetricOutcome::Failed => &self.failed,
            MetricOutcome::Cancelled => &self.cancelled,
        }
    }

    fn snapshot(&self) -> FamilySnapshot {
        FamilySnapshot {
            completed: self.completed.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            units: self.units.load(Ordering::Relaxed),
            latency_micros: self.latency_micros.load(Ordering::Relaxed),
            max_latency_micros: self.max_latency_micros.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn disabled_telemetry_does_not_build_an_observation() {
        let invoked = Cell::new(false);
        Telemetry::disabled().record_with(|| {
            invoked.set(true);
            Observation {
                family: MetricFamily::Lifecycle,
                outcome: MetricOutcome::Completed,
                latency: Duration::ZERO,
                units: 1,
            }
        });
        assert!(!invoked.get());

        struct MustNotExport<'a>(&'a Cell<bool>);
        impl TelemetryExporter for MustNotExport<'_> {
            type Error = ();

            fn export(&mut self, _snapshot: TelemetrySnapshot) -> Result<(), Self::Error> {
                self.0.set(true);
                Ok(())
            }
        }
        let mut exporter = MustNotExport(&invoked);
        assert_eq!(
            Telemetry::disabled().export(&mut exporter),
            ExportStatus::Disabled
        );
        assert!(!invoked.get());
    }

    struct BrokenExporter;

    impl TelemetryExporter for BrokenExporter {
        type Error = ();

        fn export(&mut self, _snapshot: TelemetrySnapshot) -> Result<(), Self::Error> {
            Err(())
        }
    }

    #[test]
    fn exporter_failure_is_counted_and_does_not_disable_recording() {
        let telemetry = Telemetry::enabled();
        assert_eq!(telemetry.export(&mut BrokenExporter), ExportStatus::Failed);
        telemetry.record_with(|| Observation {
            family: MetricFamily::Remote,
            outcome: MetricOutcome::Failed,
            latency: Duration::from_micros(7),
            units: 11,
        });
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.exporter_failures, 1);
        assert_eq!(snapshot.family(MetricFamily::Remote).failed, 1);
        assert_eq!(snapshot.family(MetricFamily::Remote).units, 11);
        assert_eq!(snapshot.family(MetricFamily::Remote).max_latency_micros, 7);
    }

    #[test]
    fn work_and_latency_saturate_instead_of_wrapping() {
        let telemetry = Telemetry::enabled();
        for _ in 0..2 {
            telemetry.record_with(|| Observation {
                family: MetricFamily::Recovery,
                outcome: MetricOutcome::Completed,
                latency: Duration::MAX,
                units: u64::MAX,
            });
        }
        let recovery = telemetry.snapshot().family(MetricFamily::Recovery);
        assert_eq!(recovery.completed, 2);
        assert_eq!(recovery.units, u64::MAX);
        assert_eq!(recovery.latency_micros, u64::MAX);
        assert_eq!(recovery.max_latency_micros, u64::MAX);
    }
}
