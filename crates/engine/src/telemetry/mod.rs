//! The `heart-telemetry` crate exists to batch typed product signals into tracing and OpenTelemetry backends.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Server-only `tracing` to OpenTelemetry adapter for portable product probe events.

mod batch;
mod dispatch;
mod metrics;
mod names;
mod probe;

pub use batch::{BatchLimits, BatchLimitsError, batch_logger_provider, batch_provider};
pub use dispatch::dispatch;
pub use metrics::{MetricReportError, periodic_meter_provider};
pub use probe::TracingProbe;
