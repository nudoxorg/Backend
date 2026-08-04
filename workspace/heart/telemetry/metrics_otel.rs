//! The OTLP-backed [`opentelemetry_sdk::metrics::SdkMeterProvider`].
//!
//! This stands up the OTLP metrics pipeline (exporter + `Resource`, periodic
//! push). The provider's [`Meter`](opentelemetry::metrics::Meter) is handed to
//! [`crate::metrics_bridge`], which fans the `metrics`-crate facade out to
//! *both* this OTLP pipeline and the Prometheus `/metrics` pull recorder — so
//! every `metrics::counter!`/`gauge!`/`histogram!` call site (outbox, CAS,
//! index, and the HTTP RED metrics) reaches both sinks through one installed
//! global recorder. (This is the plan's "path (b)"; the earlier revision of
//! this crate left the facade unbridged.)

use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;

use super::config::TelemetryConfig;

/// Build the OTLP metrics exporter + periodic-reader `SdkMeterProvider`, same
/// endpoint/protocol/timeout discipline as the tracer provider.
pub(crate) fn build_provider(
	config: &TelemetryConfig,
	resource: Resource,
) -> anyhow::Result<SdkMeterProvider> {
	let exporter = opentelemetry_otlp::MetricExporter::builder()
		.with_http()
		.with_endpoint(format!("{}/v1/metrics", config.otlp_endpoint.trim_end_matches('/')))
		.with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
		.with_timeout(config.export_timeout)
		.build()?;

	let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter).build();

	let provider = SdkMeterProvider::builder().with_reader(reader).with_resource(resource).build();

	Ok(provider)
}
