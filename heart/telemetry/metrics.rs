//! Defines metrics behavior for `heart-telemetry`, whose purpose is to batch typed product signals into tracing and OpenTelemetry backends.
//! This module owns the metrics invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Periodic metric export and aggregate runtime reporting.

use core::time::Duration;

use opentelemetry::metrics::{Gauge, MeterProvider as _};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider, exporter::PushMetricExporter};
use thiserror::Error;

/// Builds a background periodic metric reader; exporter work never runs on a probe hot path.
///
/// A zero interval retains the SDK's documented 60-second default. Exporter timeout and retry
/// behavior remain the concrete exporter's responsibility.
pub fn periodic_meter_provider<Exporter>(exporter: Exporter, interval: Duration) -> SdkMeterProvider
where
    Exporter: PushMetricExporter,
{
    let reader = PeriodicReader::builder(exporter)
        .with_interval(interval)
        .build();
    SdkMeterProvider::builder().with_reader(reader).build()
}

/// Exact failure while reporting one runtime metric snapshot.
#[derive(Debug, Error)]
pub enum MetricReportError {
    /// A process-native runtime metric did not fit the OTLP `u64` instrument value.
    #[error("runtime metric {metric} did not fit an OpenTelemetry u64 gauge")]
    MetricValue {
        /// Static instrument name whose value did not fit.
        metric: &'static str,
        /// Exact integer conversion source.
        #[source]
        source: core::num::TryFromIntError,
    },
}

pub(crate) struct RuntimeMetricReporter {
    capacity: Gauge<u64>,
    active_capacity: Gauge<u64>,
    available: Gauge<u64>,
    checked_out: Gauge<u64>,
    reserved_bytes: Gauge<u64>,
    retired_work_slots: Gauge<u64>,
    terminal_occupied: Gauge<u64>,
}

impl RuntimeMetricReporter {
    pub(crate) fn new(provider: &SdkMeterProvider) -> Self {
        let meter = provider.meter("server");
        Self {
            capacity: meter.u64_gauge("heart.runtime.capacity").build(),
            active_capacity: meter.u64_gauge("heart.runtime.active_capacity").build(),
            available: meter.u64_gauge("heart.runtime.available").build(),
            checked_out: meter.u64_gauge("heart.runtime.checked_out").build(),
            reserved_bytes: meter
                .u64_gauge("heart.runtime.reserved_bytes")
                .with_unit("By")
                .build(),
            retired_work_slots: meter.u64_gauge("heart.runtime.retired_work_slots").build(),
            terminal_occupied: meter.u64_gauge("heart.runtime.terminal_occupied").build(),
        }
    }

    pub(crate) fn record(
        &self,
        metrics: server_runtime::RuntimeMetrics,
    ) -> Result<(), MetricReportError> {
        record_gauge(&self.capacity, "heart.runtime.capacity", metrics.capacity)?;
        record_gauge(
            &self.active_capacity,
            "heart.runtime.active_capacity",
            metrics.active_capacity,
        )?;
        record_gauge(
            &self.available,
            "heart.runtime.available",
            metrics.available,
        )?;
        record_gauge(
            &self.checked_out,
            "heart.runtime.checked_out",
            metrics.checked_out,
        )?;
        record_gauge(
            &self.reserved_bytes,
            "heart.runtime.reserved_bytes",
            metrics.reserved_bytes,
        )?;
        record_gauge(
            &self.retired_work_slots,
            "heart.runtime.retired_work_slots",
            metrics.retired_work_slots,
        )?;
        record_gauge(
            &self.terminal_occupied,
            "heart.runtime.terminal_occupied",
            metrics.terminal_occupied,
        )?;
        Ok(())
    }
}

fn record_gauge(
    gauge: &Gauge<u64>,
    metric: &'static str,
    value: usize,
) -> Result<(), MetricReportError> {
    let value =
        u64::try_from(value).map_err(|source| MetricReportError::MetricValue { metric, source })?;
    gauge.record(value, &[]);
    Ok(())
}
