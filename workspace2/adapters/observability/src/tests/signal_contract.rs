use std::collections::BTreeMap;

use opentelemetry::{SpanId, TraceId, Value, logs::AnyValue};
use opentelemetry_sdk::{
    logs::{InMemoryLogExporter, in_memory_exporter::LogDataWithResource},
    metrics::{
        InMemoryMetricExporter,
        data::{AggregatedMetrics, MetricData, ResourceMetrics},
    },
    trace::{InMemorySpanExporterBuilder, SpanData},
};

use crate::{
    BatchLimits, TracingProbe, batch_logger_provider, batch_provider, dispatch,
    periodic_meter_provider,
};

use super::support::{
    AdapterTestError, EVENTS, ExpectedEvent, ExpectedValue, ScenarioSpan, nudox_interest,
};

const TRACE_METADATA_FIELDS: usize = 5;

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the integration test retains exact SDK and scenario sources without boxing"
)]
fn traces_logs_and_periodic_metrics_export_exact_correlated_evidence()
-> Result<(), AdapterTestError> {
    let limits = BatchLimits::new(64, 16, core::time::Duration::from_millis(5))?;
    let span_exporter = InMemorySpanExporterBuilder::new().build();
    let log_exporter = InMemoryLogExporter::default();
    let metric_exporter = InMemoryMetricExporter::default();
    let trace_provider = batch_provider(span_exporter.clone(), limits);
    let logger_provider = batch_logger_provider(log_exporter.clone(), limits);
    let meter_provider =
        periodic_meter_provider(metric_exporter.clone(), core::time::Duration::from_hours(1));
    let subscriber = dispatch(&trace_provider, &logger_provider, nudox_interest());

    let evidence = tracing::dispatcher::with_default(&subscriber, || {
        TracingProbe::with_meter(&meter_provider).run_scenario()
    })?;
    assert_eq!(
        evidence.workflow_phase,
        nudox_workflow::PhaseName::Published
    );
    trace_provider.force_flush()?;
    logger_provider.force_flush()?;
    meter_provider.force_flush()?;

    let span_ids = assert_exact_spans(&span_exporter.get_finished_spans()?)?;
    assert_exact_logs(&log_exporter.get_emitted_logs()?, span_ids)?;
    assert_exact_metrics(&metric_exporter.get_finished_metrics()?)?;

    trace_provider.shutdown()?;
    logger_provider.shutdown()?;
    meter_provider.shutdown()?;
    assert!(span_exporter.is_shutdown_called());
    assert!(log_exporter.is_shutdown_called());
    Ok(())
}

fn assert_exact_spans(spans: &[SpanData]) -> Result<ScenarioSpanIds, AdapterTestError> {
    assert_eq!(spans.len(), 3);
    let by_name: BTreeMap<_, _> = spans
        .iter()
        .map(|span| (span.name.as_ref(), span))
        .collect();
    let request = required_span(&by_name, "nudox.request")?;
    let root = required_span(&by_name, "nudox.root_selection")?;
    let workflow = required_span(&by_name, "nudox.workflow_stage")?;
    assert_eq!(request.parent_span_id, SpanId::INVALID);
    assert_eq!(root.parent_span_id, request.span_context.span_id());
    assert_eq!(workflow.parent_span_id, request.span_context.span_id());
    assert_eq!(
        root.span_context.trace_id(),
        request.span_context.trace_id()
    );
    assert_eq!(
        workflow.span_context.trace_id(),
        request.span_context.trace_id()
    );

    assert_trace_events(root, &EVENTS[0..1])?;
    assert_trace_events(request, &EVENTS[1..9])?;
    assert_trace_events(workflow, &EVENTS[9..])?;
    Ok(ScenarioSpanIds {
        trace: request.span_context.trace_id(),
        request: request.span_context.span_id(),
        root: root.span_context.span_id(),
        workflow: workflow.span_context.span_id(),
    })
}

fn required_span<'spans>(
    spans: &BTreeMap<&str, &'spans SpanData>,
    name: &'static str,
) -> Result<&'spans SpanData, AdapterTestError> {
    spans
        .get(name)
        .copied()
        .ok_or(AdapterTestError::MissingSpan { name })
}

fn assert_trace_events(
    span: &SpanData,
    expected: &[ExpectedEvent],
) -> Result<(), AdapterTestError> {
    assert_eq!(span.events.dropped_count, 0);
    assert_eq!(span.events.events.len(), expected.len());
    for (event, expected) in span.events.events.iter().zip(expected) {
        assert_eq!(event.name, expected.body);
        assert_eq!(event.dropped_attributes_count, 0);
        assert_eq!(
            event.attributes.len(),
            expected.fields.len() + TRACE_METADATA_FIELDS
        );
        assert_eq!(
            trace_attribute(event, "level")?,
            ObservedValue::text("INFO")
        );
        assert_eq!(
            trace_attribute(event, "target")?,
            ObservedValue::text(expected.target)
        );
        let _file = trace_attribute(event, "code.file.path")?;
        let _module = trace_attribute(event, "code.module.name")?;
        let _line = trace_attribute(event, "code.line.number")?;
        for &(key, value) in expected.fields {
            assert_eq!(trace_attribute(event, key)?, value.trace_value());
        }
    }
    Ok(())
}

fn trace_attribute(
    event: &opentelemetry::trace::Event,
    key: &'static str,
) -> Result<ObservedValue, AdapterTestError> {
    event
        .attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| match &attribute.value {
            Value::I64(value) if *value >= 0 => Ok(ObservedValue::Unsigned(value.cast_unsigned())),
            Value::String(value) => Ok(ObservedValue::text(value.as_ref())),
            _ => ObservedValue::unsupported(event.name.as_ref(), key),
        })
        .transpose()?
        .ok_or_else(|| AdapterTestError::MissingTraceAttribute {
            event: event.name.to_string(),
            key,
        })
}

fn assert_exact_logs(
    logs: &[LogDataWithResource],
    span_ids: ScenarioSpanIds,
) -> Result<(), AdapterTestError> {
    assert_eq!(logs.len(), EVENTS.len());
    for (index, (log, expected)) in logs.iter().zip(EVENTS).enumerate() {
        assert_eq!(log.record.event_name(), Some(expected.event_name));
        assert_eq!(
            log.record.target().map(AsRef::as_ref),
            Some(expected.target)
        );
        assert_eq!(log.record.severity_text(), Some("INFO"));
        assert_eq!(log.record.body(), Some(&AnyValue::from(expected.body)));
        let context = log
            .record
            .trace_context()
            .ok_or(AdapterTestError::MissingLogContext { index })?;
        assert_eq!(context.trace_id, span_ids.trace);
        assert_eq!(context.span_id, span_ids.for_event(expected.span));
        assert_eq!(log.record.attributes_iter().count(), expected.fields.len());
        for &(key, value) in expected.fields {
            let observed = log
                .record
                .attributes_iter()
                .find(|(observed, _)| observed.as_str() == key)
                .map(|(_, value)| log_value(index, key, value))
                .transpose()?
                .ok_or_else(|| AdapterTestError::UnsupportedLogValue {
                    index,
                    key: key.to_owned(),
                })?;
            assert_eq!(observed, value.log_value());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ScenarioSpanIds {
    trace: TraceId,
    request: SpanId,
    root: SpanId,
    workflow: SpanId,
}

impl ScenarioSpanIds {
    const fn for_event(self, owner: ScenarioSpan) -> SpanId {
        match owner {
            ScenarioSpan::Request => self.request,
            ScenarioSpan::Root => self.root,
            ScenarioSpan::Workflow => self.workflow,
        }
    }
}

fn log_value(index: usize, key: &str, value: &AnyValue) -> Result<ObservedValue, AdapterTestError> {
    match value {
        AnyValue::Int(value) if *value >= 0 => Ok(ObservedValue::Unsigned(value.cast_unsigned())),
        AnyValue::String(value) => Ok(ObservedValue::text(value.as_str())),
        _ => Err(AdapterTestError::UnsupportedLogValue {
            index,
            key: key.to_owned(),
        }),
    }
}

#[derive(Debug, Eq, PartialEq)]
enum ObservedValue {
    Text(String),
    Unsigned(u64),
}

impl ObservedValue {
    fn text(value: &str) -> Self {
        Self::Text(value.to_owned())
    }

    fn unsupported(event: &str, key: &'static str) -> Result<Self, AdapterTestError> {
        Err(AdapterTestError::UnsupportedTraceValue {
            event: event.to_owned(),
            key,
        })
    }
}

impl ExpectedValue {
    // tracing-opentelemetry writes event counters as canonical decimal strings; the log bridge
    // preserves them as OTLP integers. Both representations are part of this adapter contract.
    fn trace_value(self) -> ObservedValue {
        match self {
            Self::Text(value) => ObservedValue::text(value),
            Self::Count(value) => ObservedValue::text(&value.to_string()),
        }
    }

    fn log_value(self) -> ObservedValue {
        match self {
            Self::Text(value) => ObservedValue::text(value),
            Self::Count(value) => ObservedValue::Unsigned(value),
        }
    }
}

fn assert_exact_metrics(exports: &[ResourceMetrics]) -> Result<(), AdapterTestError> {
    assert_eq!(exports.len(), 1);
    let expected = [
        ("nudox.runtime.capacity", 1),
        ("nudox.runtime.active_capacity", 1),
        ("nudox.runtime.available", 1),
        ("nudox.runtime.checked_out", 0),
        ("nudox.runtime.reserved_bytes", 0),
        ("nudox.runtime.retired_work_slots", 0),
        ("nudox.runtime.terminal_occupied", 0),
    ];
    let exported_count = exports[0]
        .scope_metrics()
        .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
        .count();
    assert_eq!(exported_count, expected.len());
    for (name, expected_value) in expected {
        let metric = exports[0]
            .scope_metrics()
            .flat_map(opentelemetry_sdk::metrics::data::ScopeMetrics::metrics)
            .find(|metric| metric.name() == name)
            .ok_or(AdapterTestError::MissingMetric { name })?;
        let AggregatedMetrics::U64(MetricData::Gauge(gauge)) = metric.data() else {
            return Err(AdapterTestError::UnexpectedMetricShape {
                name: name.to_owned(),
            });
        };
        let mut points = gauge.data_points();
        let Some(point) = points.next() else {
            return Err(AdapterTestError::UnexpectedMetricShape {
                name: name.to_owned(),
            });
        };
        if points.next().is_some() || point.attributes().next().is_some() {
            return Err(AdapterTestError::UnexpectedMetricShape {
                name: name.to_owned(),
            });
        }
        assert_eq!(point.value(), expected_value);
    }
    Ok(())
}

#[test]
fn filtered_dispatch_does_not_build_root_probe_events() -> Result<(), AdapterTestError> {
    use core::cell::Cell;
    use nudox_observe::Probe;
    use nudox_root::{RootProbeEvent, SelectionWork};
    use tracing::level_filters::LevelFilter;

    let built = Cell::new(false);
    let mut probe = TracingProbe::new();
    let limits = BatchLimits::new(1, 1, core::time::Duration::from_secs(1))?;
    let trace_provider = batch_provider(InMemorySpanExporterBuilder::new().build(), limits);
    let logger_provider = batch_logger_provider(InMemoryLogExporter::default(), limits);
    let subscriber = dispatch(
        &trace_provider,
        &logger_provider,
        nudox_interest().with_target("nudox.root", LevelFilter::OFF),
    );
    tracing::dispatcher::with_default(&subscriber, || {
        Probe::<RootProbeEvent>::record_with(&mut probe, || {
            built.set(true);
            RootProbeEvent {
                selected_rows: 1,
                work: SelectionWork {
                    projected_rows: 1,
                    ancestor_edges: 0,
                },
            }
        });
    });
    assert!(!built.get());
    trace_provider.shutdown()?;
    logger_provider.shutdown()?;
    Ok(())
}
