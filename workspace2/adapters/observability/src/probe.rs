//! Static server-side span tree and portable probe event mapping.

use nudox_hydration::HydrationProbeEvent;
use nudox_observe::Probe;
use nudox_root::RootProbeEvent;
use nudox_runtime::RuntimeProbeEvent;
use nudox_store_memory::StoreProbeEvent;
use nudox_workflow::WorkflowProbeEvent;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use tracing::{Level, Span};

use crate::{
    AdapterRunError,
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
        let request = tracing::info_span!(target: "nudox", "nudox.request", scenario = "wave1");
        let root = tracing::info_span!(target: "nudox", parent: &request, "nudox.root_selection");
        let workflow =
            tracing::info_span!(target: "nudox", parent: &request, "nudox.workflow_stage");
        Self {
            request,
            root,
            workflow,
            metrics: None,
        }
    }

    /// Creates a probe that reports one aggregate runtime snapshot after the scenario completes.
    #[must_use]
    pub fn with_meter(provider: &SdkMeterProvider) -> Self {
        let mut probe = Self::new();
        probe.metrics = Some(RuntimeMetricReporter::new(provider));
        probe
    }

    /// Runs the shared scenario under the request parent span.
    ///
    /// # Errors
    ///
    /// Returns the unchanged typed scenario failure or exact metric conversion failure.
    #[allow(
        clippy::result_large_err,
        reason = "the adapter preserves the scenario's descriptor-rich typed source without allocation"
    )]
    pub fn run_scenario(&mut self) -> Result<nudox_e2e::ScenarioEvidence, AdapterRunError> {
        let request = self.request.clone();
        let evidence = request.in_scope(|| nudox_e2e::run_wave1_scenario(self))?;
        if let Some(metrics) = &self.metrics {
            metrics.record(evidence.runtime)?;
        }
        Ok(evidence)
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
        if !tracing::enabled!(target: "nudox.root", Level::INFO) {
            return;
        }
        let event = build();
        self.root.in_scope(|| {
            tracing::event!(
                name: "nudox.root.selection",
                target: "nudox.root",
                Level::INFO,
                selected_rows = event.selected_rows,
                projected_rows = event.work.projected_rows,
                ancestor_edges = event.work.ancestor_edges,
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
        if !tracing::enabled!(target: "nudox.hydration", Level::INFO) {
            return;
        }
        let event = build();
        let (outcome, required, present, promised, missing) = hydration_fields(event.outcome);
        self.request.in_scope(|| {
            tracing::event!(
                name: "nudox.hydration.plan",
                target: "nudox.hydration",
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
        if !tracing::enabled!(target: "nudox.store", Level::INFO) {
            return;
        }
        let admission = store_name(build().admission);
        self.request.in_scope(|| {
            tracing::event!(
                name: "nudox.store.admission",
                target: "nudox.store",
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
        if !tracing::enabled!(target: "nudox.runtime", Level::INFO) {
            return;
        }
        let (operation, outcome) = runtime_fields(build());
        self.request.in_scope(|| {
            tracing::event!(
                name: "nudox.runtime.transition",
                target: "nudox.runtime",
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
        if !tracing::enabled!(target: "nudox.workflow", Level::INFO) {
            return;
        }
        let event = build();
        let (outcome, to) = workflow_disposition(event.disposition);
        self.workflow.in_scope(|| {
            tracing::event!(
                name: "nudox.workflow.transition",
                target: "nudox.workflow",
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
