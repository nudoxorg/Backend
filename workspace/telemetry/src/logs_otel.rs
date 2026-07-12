//! The OTLP-backed [`opentelemetry_sdk::logs::SdkLoggerProvider`] and the
//! `opentelemetry-appender-tracing` bridge that turns `tracing` events into
//! OTel log records (each carrying the active `trace_id`/`span_id` so
//! Grafana's Loki↔Tempo correlation works — OBSERVABILITY-PLAN.md §5).

use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::SdkLoggerProvider;

use crate::config::TelemetryConfig;

/// Build the OTLP log exporter + `SdkLoggerProvider`.
pub(crate) fn build_provider(
	config: &TelemetryConfig,
	resource: Resource,
) -> anyhow::Result<SdkLoggerProvider> {
	let exporter = opentelemetry_otlp::LogExporter::builder()
		.with_http()
		.with_endpoint(format!("{}/v1/logs", config.otlp_endpoint.trim_end_matches('/')))
		.with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
		.with_timeout(config.export_timeout)
		.build()?;

	let provider =
		SdkLoggerProvider::builder().with_batch_exporter(exporter).with_resource(resource).build();

	Ok(provider)
}

/// The `tracing` → OTel-logs bridge layer over the given provider. Every
/// `tracing::info!`/`warn!`/etc. event flows through this layer *in addition
/// to* the `fmt` (journal) layer — both read from the same `tracing` event,
/// no double-instrumentation at call sites.
pub(crate) fn layer<S>(provider: &SdkLoggerProvider) -> OpenTelemetryTracingBridge<SdkLoggerProvider, S>
where
	S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
	OpenTelemetryTracingBridge::new(provider)
}
