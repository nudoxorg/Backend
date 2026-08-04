//! The single [`opentelemetry_sdk::Resource`] describing this process,
//! attached to every span/metric/log so all three signals join under one
//! `service.name` in Grafana (OBSERVABILITY-PLAN.md §1).

use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;

use super::config::TelemetryConfig;

/// Build the process [`Resource`]: `service.name`, `service.version`,
/// `deployment.environment`, plus whatever else
/// [`TelemetryConfig::resource_attributes`] carries (already includes
/// `host.name` / `nudox.build_system` — see [`TelemetryConfig::resolve`]).
pub(crate) fn build(config: &TelemetryConfig) -> Resource {
	let mut attributes = vec![
		KeyValue::new(
			opentelemetry_semantic_conventions::resource::SERVICE_NAME,
			config.service_name.clone(),
		),
		KeyValue::new(
			opentelemetry_semantic_conventions::resource::SERVICE_VERSION,
			config.service_version.clone(),
		),
		KeyValue::new("deployment.environment", config.environment.clone()),
	];

	for (key, value) in &config.resource_attributes {
		// `deployment.environment` is already set (typed) above; avoid a
		// duplicate attribute with possibly-conflicting value.
		if key == "deployment.environment" {
			continue;
		}
		attributes.push(KeyValue::new(key.clone(), value.clone()));
	}

	Resource::builder().with_attributes(attributes).build()
}
