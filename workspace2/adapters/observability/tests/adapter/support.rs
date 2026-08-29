use thiserror::Error;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::Targets;

use nudox_observability_adapter::{AdapterRunError, BatchLimitsError};

#[derive(Debug, Error)]
pub(super) enum AdapterTestError {
    #[error("invalid batch test configuration")]
    Limits(#[from] BatchLimitsError),
    #[error("adapter scenario failed")]
    Run(#[from] AdapterRunError),
    #[error("OpenTelemetry SDK operation failed")]
    Sdk(#[from] opentelemetry_sdk::error::OTelSdkError),
    #[error("export omitted span {name}")]
    MissingSpan { name: &'static str },
    #[error("export repeated span {name}")]
    DuplicateSpan { name: &'static str },
    #[error("exported span {index} had an unexpected name")]
    UnexpectedSpan { index: usize },
    #[error("trace event {event} omitted attribute {key}")]
    MissingTraceAttribute { event: String, key: &'static str },
    #[error("trace event {event} carried unsupported value in {key}")]
    UnsupportedTraceValue { event: String, key: &'static str },
    #[error("log {index} omitted its active trace context")]
    MissingLogContext { index: usize },
    #[error("log {index} omitted field {key}")]
    MissingLogField { index: usize, key: &'static str },
    #[error("log {index} carried unsupported value in {key}")]
    UnsupportedLogValue { index: usize, key: &'static str },
    #[error("metric export omitted {name}")]
    MissingMetric { name: &'static str },
    #[error("metric {name} was not one exact u64 gauge point")]
    UnexpectedMetricShape { name: String },
    #[error("blocking exporter test gate was poisoned")]
    GatePoisoned,
    #[error("blocking exporter test gate exceeded its bounded wait")]
    GateTimedOut,
    #[error("producer did not complete before the bounded coordination timeout")]
    ProducerTimedOut,
    #[error("producer completion channel disconnected")]
    ProducerDisconnected,
    #[error("producer thread panicked")]
    ProducerPanicked,
}

impl AdapterTestError {
    pub(super) fn into_sdk_error(self) -> opentelemetry_sdk::error::OTelSdkError {
        opentelemetry_sdk::error::OTelSdkError::InternalFailure(self.to_string())
    }
}

pub(super) fn nudox_interest() -> Targets {
    Targets::new().with_target("nudox", LevelFilter::INFO)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ExpectedEvent {
    pub(super) event_name: &'static str,
    pub(super) target: &'static str,
    pub(super) body: &'static str,
    pub(super) fields: &'static [(&'static str, ExpectedValue)],
    pub(super) span: ScenarioSpan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExpectedValue {
    Text(&'static str),
    Count(u64),
}

pub(super) const fn text(value: &'static str) -> ExpectedValue {
    ExpectedValue::Text(value)
}

pub(super) const fn count(value: u64) -> ExpectedValue {
    ExpectedValue::Count(value)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ScenarioSpan {
    Request,
    Root,
    Workflow,
}

const fn event(
    event_name: &'static str,
    target: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, ExpectedValue)],
    span: ScenarioSpan,
) -> ExpectedEvent {
    ExpectedEvent {
        event_name,
        target,
        body,
        fields,
        span,
    }
}

pub(super) const EVENTS: [ExpectedEvent; 15] = [
    event(
        "nudox.root.selection",
        "nudox.root",
        "root selection completed",
        &[
            ("selected_rows", count(1)),
            ("projected_rows", count(1)),
            ("ancestor_edges", count(0)),
        ],
        ScenarioSpan::Root,
    ),
    event(
        "nudox.hydration.plan",
        "nudox.hydration",
        "hydration planning completed",
        &[
            ("outcome", text("planned")),
            ("required", count(1)),
            ("present", count(0)),
            ("promised", count(1)),
            ("missing", count(0)),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.store.admission",
        "nudox.store",
        "store admission completed",
        &[("admission", text("inserted"))],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.store.admission",
        "nudox.store",
        "store admission completed",
        &[("admission", text("already_present"))],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.hydration.plan",
        "nudox.hydration",
        "hydration planning completed",
        &[
            ("outcome", text("planned")),
            ("required", count(1)),
            ("present", count(1)),
            ("promised", count(0)),
            ("missing", count(0)),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.runtime.transition",
        "nudox.runtime",
        "runtime transition completed",
        &[
            ("operation", text("admission")),
            ("outcome", text("admitted")),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.runtime.transition",
        "nudox.runtime",
        "runtime transition completed",
        &[
            ("operation", text("admission")),
            ("outcome", text("work_slots_rejected")),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.runtime.transition",
        "nudox.runtime",
        "runtime transition completed",
        &[
            ("operation", text("execution")),
            ("outcome", text("terminalized")),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.runtime.transition",
        "nudox.runtime",
        "runtime transition completed",
        &[
            ("operation", text("terminal")),
            ("outcome", text("completed")),
        ],
        ScenarioSpan::Request,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("new")),
            ("event", text("requested")),
            ("outcome", text("accepted")),
            ("to", text("requested")),
        ],
        ScenarioSpan::Workflow,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("requested")),
            ("event", text("admitted")),
            ("outcome", text("accepted")),
            ("to", text("admitted")),
        ],
        ScenarioSpan::Workflow,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("admitted")),
            ("event", text("staged")),
            ("outcome", text("accepted")),
            ("to", text("staged")),
        ],
        ScenarioSpan::Workflow,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("staged")),
            ("event", text("verified")),
            ("outcome", text("accepted")),
            ("to", text("verified")),
        ],
        ScenarioSpan::Workflow,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("verified")),
            ("event", text("publication_started")),
            ("outcome", text("accepted")),
            ("to", text("publishing")),
        ],
        ScenarioSpan::Workflow,
    ),
    event(
        "nudox.workflow.transition",
        "nudox.workflow",
        "workflow transition completed",
        &[
            ("from", text("publishing")),
            ("event", text("published")),
            ("outcome", text("accepted")),
            ("to", text("published")),
        ],
        ScenarioSpan::Workflow,
    ),
];
