#![forbid(unsafe_code)]
//! Server-only `tracing` to OpenTelemetry adapter for portable Nudox probe events.

mod batch;
mod dispatch;
mod metrics;
mod names;
mod probe;

pub use batch::{BatchLimits, BatchLimitsError, batch_logger_provider, batch_provider};
pub use dispatch::dispatch;
pub use metrics::{AdapterRunError, periodic_meter_provider};
pub use probe::TracingProbe;
