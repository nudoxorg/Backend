//! Static low-cardinality names for portable semantic events.

use nudox_hydration::{HydrationOutcome, PlanRejection};
use nudox_runtime::{
    RejectionReason, RuntimeAdmission, RuntimeContainment, RuntimeExecution, RuntimeProbeEvent,
    RuntimeTerminal, TerminalClass,
};
use nudox_store_memory::StoreAdmission;
use nudox_workflow::{EventName, PhaseName, WorkflowDisposition, WorkflowRejection};

pub(crate) fn hydration_fields(outcome: HydrationOutcome) -> (&'static str, u64, u64, u64, u64) {
    match outcome {
        HydrationOutcome::Planned(coverage) => (
            "planned",
            u64::from(u32::from(coverage.required)),
            u64::from(u32::from(coverage.present)),
            u64::from(u32::from(coverage.promised)),
            u64::from(u32::from(coverage.missing)),
        ),
        HydrationOutcome::Rejected(rejection) => (plan_rejection(rejection), 0, 0, 0, 0),
    }
}

const fn plan_rejection(rejection: PlanRejection) -> &'static str {
    match rejection {
        PlanRejection::DemandMismatch => "demand_mismatch",
        PlanRejection::ClosureScratchTooSmall => "closure_scratch_too_small",
        PlanRejection::PlanScratchTooSmall => "plan_scratch_too_small",
    }
}

pub(crate) const fn store_name(admission: StoreAdmission) -> &'static str {
    match admission {
        StoreAdmission::Inserted => "inserted",
        StoreAdmission::AlreadyPresent => "already_present",
        StoreAdmission::LengthMismatch => "length_mismatch",
        StoreAdmission::ContentMismatch => "content_mismatch",
        StoreAdmission::IntegrityConflict => "integrity_conflict",
        StoreAdmission::ByteCapacityExceeded => "byte_capacity_exceeded",
        StoreAdmission::SlotCapacityExceeded => "slot_capacity_exceeded",
    }
}

pub(crate) const fn runtime_fields(event: RuntimeProbeEvent) -> (&'static str, &'static str) {
    match event {
        RuntimeProbeEvent::Admission(admission) => ("admission", admission_name(admission)),
        RuntimeProbeEvent::Execution(execution) => ("execution", execution_name(execution)),
        RuntimeProbeEvent::Terminal(terminal) => ("terminal", terminal_name(terminal)),
    }
}

const fn admission_name(admission: RuntimeAdmission) -> &'static str {
    match admission {
        RuntimeAdmission::Admitted => "admitted",
        RuntimeAdmission::Rejected(RejectionReason::WorkSlots) => "work_slots_rejected",
        RuntimeAdmission::Rejected(RejectionReason::ByteBudget) => "byte_budget_rejected",
        RuntimeAdmission::SlotRetired => "slot_retired",
        RuntimeAdmission::WaiterCapacity => "waiter_capacity",
        RuntimeAdmission::WaiterRegistration => "waiter_registration",
    }
}

const fn containment_name(containment: RuntimeContainment) -> &'static str {
    match containment {
        RuntimeContainment::WorkReadyContainedTerminal => "work_ready_contained_terminal",
        RuntimeContainment::TerminalReadyContainedWork => "terminal_ready_contained_work",
    }
}

const fn execution_name(execution: RuntimeExecution) -> &'static str {
    match execution {
        RuntimeExecution::Idle => "idle",
        RuntimeExecution::Terminalized => "terminalized",
        RuntimeExecution::Contained(containment) => containment_name(containment),
    }
}

const fn terminal_name(terminal: RuntimeTerminal) -> &'static str {
    match terminal {
        RuntimeTerminal::Idle => "idle",
        RuntimeTerminal::Observed(class) => terminal_class_name(class),
        RuntimeTerminal::Contained(containment) => containment_name(containment),
    }
}

const fn terminal_class_name(class: TerminalClass) -> &'static str {
    match class {
        TerminalClass::Completed => "completed",
        TerminalClass::Failed => "failed",
        TerminalClass::Cancelled => "cancelled",
        TerminalClass::StaleGeneration => "stale_generation",
        TerminalClass::ExecutorUnwound => "executor_unwound",
    }
}

pub(crate) const fn workflow_disposition(
    disposition: WorkflowDisposition,
) -> (&'static str, &'static str) {
    match disposition {
        WorkflowDisposition::Accepted { to } => ("accepted", phase_name(to)),
        WorkflowDisposition::Rejected(rejection) => (workflow_rejection(rejection), "unchanged"),
    }
}

const fn workflow_rejection(rejection: WorkflowRejection) -> &'static str {
    match rejection {
        WorkflowRejection::UnknownVersion => "unknown_version",
        WorkflowRejection::StageKeyMismatch => "stage_key_mismatch",
        WorkflowRejection::ConflictingOutput => "conflicting_output",
        WorkflowRejection::ConflictingFailure => "conflicting_failure",
        WorkflowRejection::ImpossibleTransition => "impossible_transition",
        WorkflowRejection::CannotCancelPublishing => "cannot_cancel_publishing",
        WorkflowRejection::LogFull => "log_full",
    }
}

pub(crate) const fn phase_name(phase: PhaseName) -> &'static str {
    match phase {
        PhaseName::New => "new",
        PhaseName::Requested => "requested",
        PhaseName::Admitted => "admitted",
        PhaseName::Staged => "staged",
        PhaseName::Verified => "verified",
        PhaseName::Publishing => "publishing",
        PhaseName::PublicationUnknown => "publication_unknown",
        PhaseName::Published => "published",
        PhaseName::Failed => "failed",
        PhaseName::Cancelled => "cancelled",
    }
}

pub(crate) const fn event_name(event: EventName) -> &'static str {
    match event {
        EventName::Requested => "requested",
        EventName::Admitted => "admitted",
        EventName::Staged => "staged",
        EventName::Verified => "verified",
        EventName::PublicationStarted => "publication_started",
        EventName::Published => "published",
        EventName::Failed => "failed",
        EventName::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use nudox_hydration::{HydrationOutcome, PlanRejection};

    use super::hydration_fields;

    #[test]
    fn demand_mismatch_has_one_static_closed_name_and_zero_coverage() {
        assert_eq!(
            hydration_fields(HydrationOutcome::Rejected(PlanRejection::DemandMismatch,)),
            ("demand_mismatch", 0, 0, 0, 0),
        );
    }
}
