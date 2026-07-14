//! The OTLP-backed [`opentelemetry_sdk::trace::TracerProvider`] and its
//! `tracing-opentelemetry` bridge layer.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler as SdkSampler, SdkTracerProvider};

use crate::config::{Sampler, TelemetryConfig};

/// Build the OTLP trace exporter + `SdkTracerProvider` over http/protobuf, per
/// the shared contract (`OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`, single
/// `:4318` endpoint for every signal).
///
/// Fails open: any exporter-construction error is returned to the caller,
/// which (per `init`'s fail-open contract) logs a warning and continues
/// without the OTLP tracing layer rather than aborting boot. A collector
/// merely being *unreachable at runtime* is not surfaced here at all — the
/// batch exporter retries in the background and never blocks the app; only a
/// hard *construction*-time error (malformed endpoint, TLS setup failure)
/// reaches this `Result`.
pub(crate) fn build_provider(
	config: &TelemetryConfig,
	resource: Resource,
) -> anyhow::Result<SdkTracerProvider> {
	let exporter = opentelemetry_otlp::SpanExporter::builder()
		.with_http()
		.with_endpoint(format!("{}/v1/traces", config.otlp_endpoint.trim_end_matches('/')))
		.with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
		.with_timeout(config.export_timeout)
		.build()?;

	let sampler = match config.sampler {
		Sampler::AlwaysOn => SdkSampler::AlwaysOn,
		Sampler::AlwaysOff => SdkSampler::AlwaysOff,
		Sampler::ParentBasedTraceIdRatio(ratio) => {
			SdkSampler::ParentBased(Box::new(SdkSampler::TraceIdRatioBased(ratio)))
		}
	};

	let provider = SdkTracerProvider::builder()
		.with_batch_exporter(exporter)
		.with_resource(resource)
		.with_sampler(sampler)
		.build();

	Ok(provider)
}

/// The `tracing-opentelemetry` bridge layer over the given provider's default
/// tracer, named after the service so downstream OTel tooling can attribute
/// spans to `nudox-backend` even without the resource attribute round-trip.
pub(crate) fn layer<S>(
	provider: &SdkTracerProvider,
	service_name: &str,
) -> tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::Tracer>
where
	S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
	let tracer = provider.tracer(service_name.to_owned());
	tracing_opentelemetry::layer().with_tracer(tracer)
}
