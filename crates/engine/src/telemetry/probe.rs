//! Defines probe behavior for `heart-telemetry`, whose purpose is to batch typed product signals into tracing and OpenTelemetry backends.
//! This module owns the probe invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Static server-side span tree and portable probe event mapping.

use heart_hydration::HydrationProbeEvent;
use backend_store::memory::StoreProbeEvent;
use backend_version::observe::Probe;
use heart_root::RootProbeEvent;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use backend_runtime::server::RuntimeProbeEvent;
use server_workflow::WorkflowProbeEvent;
use tracing::{Level, Span};

use super::{
    MetricReportError,
    metrics::RuntimeMetricReporter,
    names::{
        event_name, hydration_fields, phase_name, runtime_fields, store_name, workflow_disposition,
    },
};

/// Server-side probe retaining static request/root/workflow span parents.
pub struct TracingProbe {
    request: Span,
    root: Span,
    workflow: Span,
    metrics: Option<RuntimeMetricReporter>,
}

impl TracingProbe {
    /// Creates one request span and its root-selection and workflow-stage children.
    #[must_use]
    pub fn new() -> Self {
        let request = tracing::info_span!(target: "heart", "heart.request");
        let root = tracing::info_span!(target: "heart", parent: &request, "server.root_selection");
        let workflow =
            tracing::info_span!(target: "heart", parent: &request, "heart.workflow_stage");
        Self {
            request,
            root,
            workflow,
            metrics: None,
        }
    }

    /// Creates a probe that can report aggregate runtime snapshots.
    #[must_use]
    pub fn with_meter(provider: &SdkMeterProvider) -> Self {
        let mut probe = Self::new();
        probe.metrics = Some(RuntimeMetricReporter::new(provider));
        probe
    }

    /// Runs one operation under this probe's request parent span.
    pub fn within_request<Output>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Output,
    ) -> Output {
        let request = self.request.clone();
        request.in_scope(|| operation(self))
    }

    /// Reports one aggregate runtime snapshot when this probe has a meter.
    ///
    /// # Errors
    ///
    /// Returns the exact integer conversion failure when a metric cannot fit its instrument.
    pub fn record_runtime_metrics(
        &self,
        snapshot: backend_runtime::server::RuntimeMetrics,
    ) -> Result<(), MetricReportError> {
        if let Some(metrics) = &self.metrics {
            metrics.record(snapshot)?;
        }
        Ok(())
    }
}

impl Default for TracingProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Probe<RootProbeEvent> for TracingProbe {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> RootProbeEvent,
    {
        if !tracing::enabled!(target: "server.root", Level::INFO) {
            return;
        }
        let event = build();
        self.root.in_scope(|| {
            tracing::event!(
                name: "server.root.selection",
                target: "server.root",
                Level::INFO,
                selected_rows = event.selected_rows,
                projected_rows = event.work.projected_rows,
                ancestor_edges = event.work.ancestor_edges,
                parent_search_comparisons = event.work.parent_search_comparisons,
                "root selection completed"
            );
        });
    }
}

impl Probe<HydrationProbeEvent> for TracingProbe {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> HydrationProbeEvent,
    {
        if !tracing::enabled!(target: "heart.hydration", Level::INFO) {
            return;
        }
        let event = build();
        let (outcome, required, present, promised, missing) = hydration_fields(event.outcome);
        self.request.in_scope(|| {
            tracing::event!(
                name: "heart.hydration.plan",
                target: "heart.hydration",
                Level::INFO,
                outcome,
                required,
                present,
                promised,
                missing,
                "hydration planning completed"
            );
        });
    }
}

impl Probe<StoreProbeEvent> for TracingProbe {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> StoreProbeEvent,
    {
        if !tracing::enabled!(target: "heart.store", Level::INFO) {
            return;
        }
        let admission = store_name(build().admission);
        self.request.in_scope(|| {
            tracing::event!(
                name: "heart.store.admission",
                target: "heart.store",
                Level::INFO,
                admission,
                "store admission completed"
            );
        });
    }
}

impl Probe<RuntimeProbeEvent> for TracingProbe {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> RuntimeProbeEvent,
    {
        if !tracing::enabled!(target: "heart.runtime", Level::INFO) {
            return;
        }
        let (operation, outcome) = runtime_fields(build());
        self.request.in_scope(|| {
            tracing::event!(
                name: "heart.runtime.transition",
                target: "heart.runtime",
                Level::INFO,
                operation,
                outcome,
                "runtime transition completed"
            );
        });
    }
}

impl Probe<WorkflowProbeEvent> for TracingProbe {
    fn record_with<Build>(&mut self, build: Build)
    where
        Build: FnOnce() -> WorkflowProbeEvent,
    {
        if !tracing::enabled!(target: "heart.workflow", Level::INFO) {
            return;
        }
        let event = build();
        let (outcome, to) = workflow_disposition(event.disposition);
        self.workflow.in_scope(|| {
            tracing::event!(
                name: "heart.workflow.transition",
                target: "heart.workflow",
                Level::INFO,
                from = phase_name(event.from),
                event = event_name(event.event),
                outcome,
                to,
                "workflow transition completed"
            );
        });
    }
}
