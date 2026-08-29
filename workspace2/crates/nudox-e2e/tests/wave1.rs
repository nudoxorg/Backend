//! Public cross-crate Wave 1 semantics and typed diagnostic chronology.

use nudox_e2e::{ScenarioError, ScenarioEvidence, run_wave1_scenario};
use nudox_hydration::{AbsentCount, HydrationOutcome, HydrationProbeEvent, PlanCoverage};
use nudox_observe::{FlightRecorder, OverwriteOldest, Probe};
use nudox_root::{RootProbeEvent, SelectedCount, SelectionWork};
use nudox_runtime::{
    RejectionReason, RuntimeAdmission, RuntimeExecution, RuntimeProbeEvent, RuntimeTerminal,
    TerminalClass,
};
use nudox_store_memory::{StoreAdmission, StoreProbeEvent};
use nudox_workflow::{EventName, PhaseName, WorkflowDisposition, WorkflowProbeEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScenarioEvent {
    Root(RootProbeEvent),
    Hydration(HydrationProbeEvent),
    Store(StoreProbeEvent),
    Runtime(RuntimeProbeEvent),
    Workflow(WorkflowProbeEvent),
}

struct ScenarioFlight<const CAPACITY: usize> {
    events: FlightRecorder<ScenarioEvent, OverwriteOldest, CAPACITY>,
}

impl<const CAPACITY: usize> ScenarioFlight<CAPACITY> {
    const fn new() -> Self {
        Self {
            events: FlightRecorder::new(),
        }
    }
}

macro_rules! map_probe {
    ($event:ty, $variant:ident) => {
        impl<const CAPACITY: usize> Probe<$event> for ScenarioFlight<CAPACITY> {
            fn record_with<Build>(&mut self, build: Build)
            where
                Build: FnOnce() -> $event,
            {
                let _disposition = self.events.record(|| ScenarioEvent::$variant(build()));
            }
        }
    };
}

map_probe!(RootProbeEvent, Root);
map_probe!(HydrationProbeEvent, Hydration);
map_probe!(StoreProbeEvent, Store);
map_probe!(RuntimeProbeEvent, Runtime);
map_probe!(WorkflowProbeEvent, Workflow);

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the scenario test retains descriptor-rich typed errors directly without heap allocation"
)]
fn noop_and_flight_modes_preserve_exact_scenario_semantics() -> Result<(), ScenarioError> {
    let expected = run_wave1_scenario(&mut ())?;
    let mut flight = ScenarioFlight::<32>::new();
    let observed = run_wave1_scenario(&mut flight)?;
    assert_eq!(observed, expected);
    assert_eq!(observed, expected_evidence()?);
    assert_eq!(
        flight.events.events().copied().collect::<Vec<_>>(),
        expected_trace()
    );
    Ok(())
}

fn expected_evidence() -> Result<ScenarioEvidence, nudox_schema::LimitError> {
    Ok(ScenarioEvidence {
        promised_objects: AbsentCount::from(1),
        emitted_objects: nudox_schema::RowCount::try_from(1)?,
        workflow_phase: PhaseName::Published,
        runtime_terminal: TerminalClass::Completed,
        runtime: nudox_runtime::RuntimeMetrics {
            capacity: 1,
            active_capacity: 1,
            available: 1,
            checked_out: 0,
            reserved_bytes: 0,
            retired_work_slots: 0,
            terminal_occupied: 0,
        },
    })
}

fn expected_trace() -> Vec<ScenarioEvent> {
    let one_selected = SelectedCount::from(1);
    vec![
        ScenarioEvent::Root(RootProbeEvent {
            selected_rows: 1,
            work: SelectionWork {
                projected_rows: 1,
                ancestor_edges: 0,
            },
        }),
        ScenarioEvent::Hydration(HydrationProbeEvent {
            outcome: HydrationOutcome::Planned(PlanCoverage {
                required: one_selected,
                present: SelectedCount::ZERO,
                promised: AbsentCount::from(1),
                missing: AbsentCount::from(0),
            }),
        }),
        ScenarioEvent::Store(StoreProbeEvent {
            admission: StoreAdmission::Inserted,
        }),
        ScenarioEvent::Store(StoreProbeEvent {
            admission: StoreAdmission::AlreadyPresent,
        }),
        ScenarioEvent::Hydration(HydrationProbeEvent {
            outcome: HydrationOutcome::Planned(PlanCoverage {
                required: one_selected,
                present: one_selected,
                promised: AbsentCount::from(0),
                missing: AbsentCount::from(0),
            }),
        }),
        ScenarioEvent::Runtime(RuntimeProbeEvent::Admission(RuntimeAdmission::Admitted)),
        ScenarioEvent::Runtime(RuntimeProbeEvent::Admission(RuntimeAdmission::Rejected(
            RejectionReason::WorkSlots,
        ))),
        ScenarioEvent::Runtime(RuntimeProbeEvent::Execution(RuntimeExecution::Terminalized)),
        ScenarioEvent::Runtime(RuntimeProbeEvent::Terminal(RuntimeTerminal::Observed(
            TerminalClass::Completed,
        ))),
        accepted_workflow(PhaseName::New, EventName::Requested, PhaseName::Requested),
        accepted_workflow(
            PhaseName::Requested,
            EventName::Admitted,
            PhaseName::Admitted,
        ),
        accepted_workflow(PhaseName::Admitted, EventName::Staged, PhaseName::Staged),
        accepted_workflow(PhaseName::Staged, EventName::Verified, PhaseName::Verified),
        accepted_workflow(
            PhaseName::Verified,
            EventName::PublicationStarted,
            PhaseName::Publishing,
        ),
        accepted_workflow(
            PhaseName::Publishing,
            EventName::Published,
            PhaseName::Published,
        ),
    ]
}

const fn accepted_workflow(from: PhaseName, event: EventName, to: PhaseName) -> ScenarioEvent {
    ScenarioEvent::Workflow(WorkflowProbeEvent {
        from,
        event,
        disposition: WorkflowDisposition::Accepted { to },
    })
}
