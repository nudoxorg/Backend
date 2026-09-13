//! Exercises the `heart-telemetry` tests adapter signal-fixture contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Typed input fixture for the adapter's complete signal vocabulary.

use heart_hydration::{AbsentCount, HydrationOutcome, HydrationProbeEvent, PlanCoverage};
use backend_store::memory::{StoreAdmission, StoreProbeEvent};
use backend_version::observe::Probe;
use heart_root::{RootProbeEvent, SelectedCount, SelectionWork};
use heart_telemetry::TracingProbe;
use backend_runtime::server::{
    RejectionReason, RuntimeAdmission, RuntimeExecution, RuntimeMetrics, RuntimeProbeEvent,
    RuntimeTerminal, TerminalClass,
};
use server_workflow::{EventName, PhaseName, WorkflowDisposition, WorkflowProbeEvent};

/// Emits one instance of every portable event mapped by the adapter.
pub(super) fn emit_signal_fixture(probe: &mut TracingProbe) -> RuntimeMetrics {
    emit_root(probe);
    let selected = SelectedCount::from(1);
    emit_hydration(
        probe,
        PlanCoverage {
            required: selected,
            present: SelectedCount::ZERO,
            promised: AbsentCount::from(1),
            missing: AbsentCount::from(0),
        },
    );
    emit_store(probe);
    emit_hydration(
        probe,
        PlanCoverage {
            required: selected,
            present: selected,
            promised: AbsentCount::from(0),
            missing: AbsentCount::from(0),
        },
    );
    emit_runtime(probe);
    emit_workflow(probe);
    RUNTIME_METRICS
}

fn emit_root(probe: &mut TracingProbe) {
    record(
        probe,
        RootProbeEvent {
            selected_rows: 1,
            work: SelectionWork {
                projected_rows: 1,
                ancestor_edges: 0,
                parent_search_comparisons: 0,
            },
        },
    );
}

fn emit_hydration(probe: &mut TracingProbe, coverage: PlanCoverage) {
    record(
        probe,
        HydrationProbeEvent {
            outcome: HydrationOutcome::Planned(coverage),
        },
    );
}

fn emit_store(probe: &mut TracingProbe) {
    record(
        probe,
        StoreProbeEvent {
            admission: StoreAdmission::Inserted,
        },
    );
    record(
        probe,
        StoreProbeEvent {
            admission: StoreAdmission::AlreadyPresent,
        },
    );
}

fn emit_runtime(probe: &mut TracingProbe) {
    record(
        probe,
        RuntimeProbeEvent::Admission(RuntimeAdmission::Admitted),
    );
    record(
        probe,
        RuntimeProbeEvent::Admission(RuntimeAdmission::Rejected(RejectionReason::WorkSlots)),
    );
    record(
        probe,
        RuntimeProbeEvent::Execution(RuntimeExecution::Terminalized),
    );
    record(
        probe,
        RuntimeProbeEvent::Terminal(RuntimeTerminal::Observed(TerminalClass::Completed)),
    );
}

fn emit_workflow(probe: &mut TracingProbe) {
    for (from, event, to) in WORKFLOW {
        record(
            probe,
            WorkflowProbeEvent {
                from,
                event,
                disposition: WorkflowDisposition::Accepted { to },
            },
        );
    }
}

fn record<Event>(probe: &mut TracingProbe, event: Event)
where
    TracingProbe: Probe<Event>,
{
    probe.record_with(|| event);
}

const WORKFLOW: [(PhaseName, EventName, PhaseName); 6] = [
    (PhaseName::New, EventName::Requested, PhaseName::Requested),
    (
        PhaseName::Requested,
        EventName::Admitted,
        PhaseName::Admitted,
    ),
    (PhaseName::Admitted, EventName::Staged, PhaseName::Staged),
    (PhaseName::Staged, EventName::Verified, PhaseName::Verified),
    (
        PhaseName::Verified,
        EventName::PublicationStarted,
        PhaseName::Publishing,
    ),
    (
        PhaseName::Publishing,
        EventName::Published,
        PhaseName::Published,
    ),
];

const RUNTIME_METRICS: RuntimeMetrics = RuntimeMetrics {
    capacity: 1,
    active_capacity: 1,
    available: 1,
    checked_out: 0,
    reserved_bytes: 0,
    retired_work_slots: 0,
    terminal_occupied: 0,
};
