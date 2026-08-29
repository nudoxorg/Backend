//! Local trace/log subscriber composition.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::{logs::SdkLoggerProvider, trace::SdkTracerProvider};
use tracing_subscriber::{filter::Targets, prelude::*};

/// Installs trace and log providers into a local dispatch without touching global state.
///
/// The logger provider correlates each event with the active OpenTelemetry request context. The
/// caller owns force-flush and shutdown ordering for all three providers.
#[must_use]
pub fn dispatch(
    trace_provider: &SdkTracerProvider,
    logger_provider: &SdkLoggerProvider,
    interest: Targets,
) -> tracing::Dispatch {
    let otel_tracer = trace_provider.tracer("nudox-server");
    let trace_layer = tracing_opentelemetry::layer().with_tracer(otel_tracer);
    let logs = OpenTelemetryTracingBridge::new(logger_provider);
    tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(interest)
            .with(trace_layer)
            .with(logs),
    )
}
