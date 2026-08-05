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
        // These values have typed sources and must not be overridden by a
        // duplicate in OTEL_RESOURCE_ATTRIBUTES. `environment` has already
        // resolved its intended deployment value in TelemetryConfig.
        if matches!(
            key.as_str(),
            "service.name" | "service.version" | "deployment.environment"
        ) {
            continue;
        }
        attributes.push(KeyValue::new(key.clone(), value.clone()));
    }

    Resource::builder().with_attributes(attributes).build()
}

#[cfg(test)]
mod tests {
    use super::build;
    use crate::telemetry::TelemetryConfig;
    use std::time::Duration;

    #[test]
    fn typed_resource_attributes_take_precedence_over_free_form_values() {
        let config = TelemetryConfig {
            disabled: false,
            otlp_endpoint: "http://127.0.0.1:4318".to_owned(),
            service_name: "typed-service".to_owned(),
            service_version: "typed-version".to_owned(),
            environment: "typed-environment".to_owned(),
            resource_attributes: vec![
                ("service.name".to_owned(), "raw-service".to_owned()),
                ("service.version".to_owned(), "raw-version".to_owned()),
                (
                    "deployment.environment".to_owned(),
                    "raw-environment".to_owned(),
                ),
                ("host.name".to_owned(), "test-host".to_owned()),
            ],
            sampler: crate::telemetry::Sampler::AlwaysOn,
            log_level: tracing::Level::INFO,
            log_format: crate::telemetry::LogFormat::Json,
            pyroscope_endpoint: None,
            export_timeout: Duration::from_secs(5),
        };

        let resource = build(&config);
        let attributes = resource
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<std::collections::HashMap<_, _>>();

        assert_eq!(
            attributes.get("service.name").map(String::as_str),
            Some("typed-service")
        );
        assert_eq!(
            attributes.get("service.version").map(String::as_str),
            Some("typed-version")
        );
        assert_eq!(
            attributes.get("deployment.environment").map(String::as_str),
            Some("typed-environment")
        );
        assert_eq!(
            attributes.get("host.name").map(String::as_str),
            Some("test-host")
        );
    }
}
