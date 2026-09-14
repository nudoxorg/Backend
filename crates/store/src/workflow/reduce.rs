//! Defines reduce behavior for the `workflow` module, whose purpose is to reduce durable workflow events into deterministic recovery state.
//! This module owns the reduce invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Pure, monotone durable workflow reduction.
#![allow(
    missing_docs,
    reason = "the closed transition and probe vocabularies document their compact type boundaries"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "each typed error names its exact reduction failure"
)]

use backend_version::observe::Probe;
use thiserror::Error;

use crate::workflow::{
    EventKind, EventName, FailureCode, StageKey, WorkflowEvent, WorkflowVersion, key::StageOutput,
};

/// Committed facts before a terminal pre-publication decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriorFacts {
    Requested,
    Admitted,
    Staged(StageOutput),
    Verified(StageOutput),
}
/// One keyed workflow phase. Every variant is keyed by the enclosing state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    Requested,
    Admitted,
    Staged(StageOutput),
    Verified(StageOutput),
    Publishing(StageOutput),
    PublicationUnknown {
        output: StageOutput,
        code: FailureCode,
    },
    Published(StageOutput),
    Failed {
        code: FailureCode,
        prior: PriorFacts,
    },
    Cancelled {
        prior: PriorFacts,
    },
}
impl Phase {
    const fn name(self) -> PhaseName {
        match self {
            Self::Requested => PhaseName::Requested,
            Self::Admitted => PhaseName::Admitted,
            Self::Staged(_) => PhaseName::Staged,
            Self::Verified(_) => PhaseName::Verified,
            Self::Publishing(_) => PhaseName::Publishing,
            Self::PublicationUnknown { .. } => PhaseName::PublicationUnknown,
            Self::Published(_) => PhaseName::Published,
            Self::Failed { .. } => PhaseName::Failed,
            Self::Cancelled { .. } => PhaseName::Cancelled,
        }
    }
}

/// Public durable phase name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PhaseName {
    New,
    Requested,
    Admitted,
    Staged,
    Verified,
    Publishing,
    PublicationUnknown,
    Published,
    Failed,
    Cancelled,
}

/// Structural workflow state: `New` cannot carry a key, while `Keyed` always does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowState {
    New,
    Keyed { key: StageKey, phase: Phase },
}
impl WorkflowState {
    #[must_use]
    pub const fn empty() -> Self {
        Self::New
    }
    #[must_use]
    pub const fn phase(&self) -> PhaseName {
        match self {
            Self::New => PhaseName::New,
            Self::Keyed { phase, .. } => phase.name(),
        }
    }
    #[must_use]
    pub const fn published_output(&self) -> Option<StageOutput> {
        match self {
            Self::Keyed {
                phase: Phase::Published(output),
                ..
            } => Some(*output),
            Self::New | Self::Keyed { .. } => None,
        }
    }
}
impl Default for WorkflowState {
    fn default() -> Self {
        Self::empty()
    }
}

/// Pure next command, with its common key stored exactly once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Effect {
    pub key: StageKey,
    pub action: EffectAction,
}

/// Closed action; only output-dependent commands retain an output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectAction {
    Admit,
    Stage,
    Verify { output: StageOutput },
    BeginPublication { output: StageOutput },
    Publish { output: StageOutput },
    ReconcilePublication { output: StageOutput },
}
/// Reducer output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Reduction {
    pub state: WorkflowState,
    pub effect: Option<Effect>,
}
/// Exact deterministic reduction rejection.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ReductionError {
    #[error("workflow version {observed:?} is unknown")]
    UnknownVersion { observed: WorkflowVersion },
    #[error("event stage key {observed:?} differs from committed key {expected:?}")]
    StageKeyMismatch {
        expected: StageKey,
        observed: StageKey,
    },
    #[error("{phase:?} output {observed:?} differs from committed output {expected:?}")]
    ConflictingOutput {
        phase: PhaseName,
        expected: StageOutput,
        observed: StageOutput,
    },
    #[error("{phase:?} failure {observed:?} differs from committed failure {expected:?}")]
    ConflictingFailure {
        phase: PhaseName,
        expected: FailureCode,
        observed: FailureCode,
    },
    #[error("event {event:?} is illegal in {phase:?}")]
    ImpossibleTransition { phase: PhaseName, event: EventName },
    #[error("cancellation after publication started cannot claim no visible publication")]
    CannotCancelPublishing,
}

/// Stable low-cardinality rejection class for workflow diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowRejection {
    UnknownVersion,
    StageKeyMismatch,
    ConflictingOutput,
    ConflictingFailure,
    ImpossibleTransition,
    CannotCancelPublishing,
    LogFull,
}

impl From<ReductionError> for WorkflowRejection {
    fn from(error: ReductionError) -> Self {
        match error {
            ReductionError::UnknownVersion { .. } => Self::UnknownVersion,
            ReductionError::StageKeyMismatch { .. } => Self::StageKeyMismatch,
            ReductionError::ConflictingOutput { .. } => Self::ConflictingOutput,
            ReductionError::ConflictingFailure { .. } => Self::ConflictingFailure,
            ReductionError::ImpossibleTransition { .. } => Self::ImpossibleTransition,
            ReductionError::CannotCancelPublishing => Self::CannotCancelPublishing,
        }
    }
}

/// Aggregate reducer outcome without keys or content identifiers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowDisposition {
    Accepted { to: PhaseName },
    Rejected(WorkflowRejection),
}

/// One workflow transition observation with only closed state/event names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowProbeEvent {
    pub from: PhaseName,
    pub event: EventName,
    pub disposition: WorkflowDisposition,
}

/// Applies one event without I/O, time, retries, or mutation.
pub fn reduce(state: WorkflowState, event: WorkflowEvent) -> Result<Reduction, ReductionError> {
    if event.version != WorkflowVersion::WAVE1 {
        return Err(ReductionError::UnknownVersion {
            observed: event.version,
        });
    }
    match state {
        WorkflowState::New => reduce_new(event),
        WorkflowState::Keyed { key, phase } => {
            if key != event.key {
                return Err(ReductionError::StageKeyMismatch {
                    expected: key,
                    observed: event.key,
                });
            }
            reduce_keyed(key, phase, event.kind)
        }
    }
}

/// Applies the publication chaining rule: a `Requested` event on a still-keyed
/// state opens the next generation chain.
pub fn reduce_chained(
    state: WorkflowState,
    event: WorkflowEvent,
) -> Result<Reduction, ReductionError> {
    match reduce(state, event) {
        Ok(reduction) => Ok(reduction),
        Err(error)
            if matches!(event.kind, EventKind::Requested)
                && matches!(state, WorkflowState::Keyed { .. }) =>
        {
            match reduce(WorkflowState::New, event) {
                Ok(reduction) => Ok(reduction),
                Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Applies one event and lazily records its low-cardinality transition classification.
pub fn reduce_with_probe<Observation>(
    state: WorkflowState,
    event: WorkflowEvent,
    probe: &mut Observation,
) -> Result<Reduction, ReductionError>
where
    Observation: Probe<WorkflowProbeEvent>,
{
    let result = reduce(state, event);
    let disposition = match result {
        Ok(reduction) => WorkflowDisposition::Accepted {
            to: reduction.state.phase(),
        },
        Err(error) => WorkflowDisposition::Rejected(error.into()),
    };
    probe.record_with(|| WorkflowProbeEvent {
        from: state.phase(),
        event: event.kind.name(),
        disposition,
    });
    result
}

const fn reduce_new(event: WorkflowEvent) -> Result<Reduction, ReductionError> {
    match event.kind {
        EventKind::Requested => Ok(next(event.key, Phase::Requested)),
        kind => Err(impossible(PhaseName::New, kind)),
    }
}
fn reduce_keyed(key: StageKey, phase: Phase, kind: EventKind) -> Result<Reduction, ReductionError> {
    match historical(phase, kind) {
        Historical::Same => return Ok(idempotent(key, phase)),
        Historical::OutputConflict { expected, observed } => {
            return Err(output_conflict(phase.name(), expected, observed));
        }
        Historical::FailureConflict { expected, observed } => {
            return Err(ReductionError::ConflictingFailure {
                phase: phase.name(),
                expected,
                observed,
            });
        }
        Historical::Absent => {}
    }
    match phase {
        Phase::Requested => reduce_requested(key, kind),
        Phase::Admitted => reduce_admitted(key, kind),
        Phase::Staged(output) => reduce_staged(key, output, kind),
        Phase::Verified(output) => reduce_verified(key, output, kind),
        Phase::Publishing(output) => reduce_publishing(key, output, kind),
        Phase::PublicationUnknown { output, code } => {
            reduce_publication_unknown(key, output, code, kind)
        }
        Phase::Published(_) | Phase::Failed { .. } | Phase::Cancelled { .. } => {
            Err(impossible(phase.name(), kind))
        }
    }
}

const fn prepublication_terminal(
    key: StageKey,
    prior: PriorFacts,
    kind: EventKind,
) -> Option<Reduction> {
    match kind {
        EventKind::Failed { code } => Some(next(key, Phase::Failed { code, prior })),
        EventKind::Cancelled => Some(next(key, Phase::Cancelled { prior })),
        _ => None,
    }
}

const fn reduce_requested(key: StageKey, kind: EventKind) -> Result<Reduction, ReductionError> {
    if let Some(terminal) = prepublication_terminal(key, PriorFacts::Requested, kind) {
        return Ok(terminal);
    }
    match kind {
        EventKind::Admitted => Ok(next(key, Phase::Admitted)),
        other => Err(impossible(PhaseName::Requested, other)),
    }
}
const fn reduce_admitted(key: StageKey, kind: EventKind) -> Result<Reduction, ReductionError> {
    if let Some(terminal) = prepublication_terminal(key, PriorFacts::Admitted, kind) {
        return Ok(terminal);
    }
    match kind {
        EventKind::Staged { output } => Ok(next(key, Phase::Staged(output))),
        other => Err(impossible(PhaseName::Admitted, other)),
    }
}
fn reduce_staged(
    key: StageKey,
    output: StageOutput,
    kind: EventKind,
) -> Result<Reduction, ReductionError> {
    if let Some(terminal) = prepublication_terminal(key, PriorFacts::Staged(output), kind) {
        return Ok(terminal);
    }
    match kind {
        EventKind::Verified { output: observed } if observed == output => {
            Ok(next(key, Phase::Verified(output)))
        }
        EventKind::Verified { output: observed } => {
            Err(output_conflict(PhaseName::Staged, output, observed))
        }
        other => Err(impossible(PhaseName::Staged, other)),
    }
}
fn reduce_verified(
    key: StageKey,
    output: StageOutput,
    kind: EventKind,
) -> Result<Reduction, ReductionError> {
    if let Some(terminal) = prepublication_terminal(key, PriorFacts::Verified(output), kind) {
        return Ok(terminal);
    }
    match kind {
        EventKind::PublicationStarted { output: observed } if observed == output => {
            Ok(next(key, Phase::Publishing(output)))
        }
        EventKind::PublicationStarted { output: observed } => {
            Err(output_conflict(PhaseName::Verified, output, observed))
        }
        other => Err(impossible(PhaseName::Verified, other)),
    }
}
fn reduce_publishing(
    key: StageKey,
    output: StageOutput,
    kind: EventKind,
) -> Result<Reduction, ReductionError> {
    match kind {
        EventKind::Published { output: observed } if observed == output => {
            Ok(next(key, Phase::Published(output)))
        }
        EventKind::Published { output: observed } => {
            Err(output_conflict(PhaseName::Publishing, output, observed))
        }
        EventKind::Failed { code } => Ok(next(key, Phase::PublicationUnknown { output, code })),
        EventKind::Cancelled => Err(ReductionError::CannotCancelPublishing),
        other => Err(impossible(PhaseName::Publishing, other)),
    }
}
fn reduce_publication_unknown(
    key: StageKey,
    output: StageOutput,
    _code: FailureCode,
    kind: EventKind,
) -> Result<Reduction, ReductionError> {
    match kind {
        EventKind::Published { output: observed } if observed == output => {
            Ok(next(key, Phase::Published(output)))
        }
        EventKind::Published { output: observed } => Err(output_conflict(
            PhaseName::PublicationUnknown,
            output,
            observed,
        )),
        EventKind::Cancelled => Err(ReductionError::CannotCancelPublishing),
        other => Err(impossible(PhaseName::PublicationUnknown, other)),
    }
}
enum Historical {
    Same,
    OutputConflict {
        expected: StageOutput,
        observed: StageOutput,
    },
    FailureConflict {
        expected: FailureCode,
        observed: FailureCode,
    },
    Absent,
}
fn historical(phase: Phase, kind: EventKind) -> Historical {
    match kind {
        EventKind::Requested => Historical::Same,
        EventKind::Admitted if phase_implies_admitted(phase) => Historical::Same,
        EventKind::Staged { output } => historical_output(phase, OutputFact::Staged, output),
        EventKind::Verified { output } => historical_output(phase, OutputFact::Verified, output),
        EventKind::PublicationStarted { output } => {
            historical_output(phase, OutputFact::PublicationStarted, output)
        }
        EventKind::Published { output } => historical_output(phase, OutputFact::Published, output),
        EventKind::Failed { code: observed } => match phase {
            Phase::Failed { code: expected, .. }
            | Phase::PublicationUnknown { code: expected, .. }
                if expected == observed =>
            {
                Historical::Same
            }
            Phase::Failed { code: expected, .. }
            | Phase::PublicationUnknown { code: expected, .. } => {
                Historical::FailureConflict { expected, observed }
            }
            _ => Historical::Absent,
        },
        EventKind::Cancelled if matches!(phase, Phase::Cancelled { .. }) => Historical::Same,
        EventKind::Admitted | EventKind::Cancelled => Historical::Absent,
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum OutputFact {
    Staged,
    Verified,
    PublicationStarted,
    Published,
}

const fn phase_implies_admitted(phase: Phase) -> bool {
    match phase {
        Phase::Requested
        | Phase::Failed {
            prior: PriorFacts::Requested,
            ..
        }
        | Phase::Cancelled {
            prior: PriorFacts::Requested,
        } => false,
        Phase::Admitted
        | Phase::Staged(_)
        | Phase::Verified(_)
        | Phase::Publishing(_)
        | Phase::PublicationUnknown { .. }
        | Phase::Published(_)
        | Phase::Failed { .. }
        | Phase::Cancelled { .. } => true,
    }
}

fn historical_output(phase: Phase, fact: OutputFact, observed: StageOutput) -> Historical {
    let expected = match (phase, fact) {
        (Phase::Staged(output), OutputFact::Staged)
        | (Phase::Verified(output), OutputFact::Staged | OutputFact::Verified)
        | (
            Phase::Publishing(output) | Phase::PublicationUnknown { output, .. },
            OutputFact::Staged | OutputFact::Verified | OutputFact::PublicationStarted,
        )
        | (Phase::Published(output), _) => output,
        (Phase::Failed { prior, .. } | Phase::Cancelled { prior }, fact) => {
            return historical_prior_output(prior, fact, observed);
        }
        _ => return Historical::Absent,
    };
    if expected == observed {
        Historical::Same
    } else {
        Historical::OutputConflict { expected, observed }
    }
}

fn historical_prior_output(
    prior: PriorFacts,
    fact: OutputFact,
    observed: StageOutput,
) -> Historical {
    let ((PriorFacts::Staged(expected), OutputFact::Staged)
    | (PriorFacts::Verified(expected), OutputFact::Staged | OutputFact::Verified)) = (prior, fact)
    else {
        return Historical::Absent;
    };
    if expected == observed {
        Historical::Same
    } else {
        Historical::OutputConflict { expected, observed }
    }
}
const fn next(key: StageKey, phase: Phase) -> Reduction {
    Reduction {
        state: WorkflowState::Keyed { key, phase },
        effect: pending_effect(key, phase),
    }
}
const fn idempotent(key: StageKey, phase: Phase) -> Reduction {
    Reduction {
        state: WorkflowState::Keyed { key, phase },
        effect: None,
    }
}

pub(crate) const fn pending_effect(key: StageKey, phase: Phase) -> Option<Effect> {
    let action = match phase {
        Phase::Requested => EffectAction::Admit,
        Phase::Admitted => EffectAction::Stage,
        Phase::Staged(output) => EffectAction::Verify { output },
        Phase::Verified(output) => EffectAction::BeginPublication { output },
        Phase::Publishing(output) => EffectAction::Publish { output },
        Phase::PublicationUnknown { output, .. } => EffectAction::ReconcilePublication { output },
        Phase::Published(_) | Phase::Failed { .. } | Phase::Cancelled { .. } => return None,
    };
    Some(Effect { key, action })
}
const fn output_conflict(
    phase: PhaseName,
    expected: StageOutput,
    observed: StageOutput,
) -> ReductionError {
    ReductionError::ConflictingOutput {
        phase,
        expected,
        observed,
    }
}
const fn impossible(phase: PhaseName, event: EventKind) -> ReductionError {
    ReductionError::ImpossibleTransition {
        phase,
        event: event.name(),
    }
}
