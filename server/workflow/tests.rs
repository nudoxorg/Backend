//! Defines tests behavior for `server-workflow`, whose purpose is to reduce durable workflow events into deterministic recovery state.
//! This module owns the tests invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::size_of;

use heart_identity::{ContentId, GenerationId, ObjectDomain};
use heart_schema::OperationId;
use thiserror::Error;

use crate::{
    CapabilityDomain, CommitError, ConfigurationDomain, Effect, EffectAction, EventKind, EventName,
    FailureCode, LogConfigError, MemoryWorkflowLog, Phase, PhaseName, PriorFacts, Reduction,
    ReductionError, StageId, StageInput, StageKey, WorkflowEvent, WorkflowState, WorkflowVersion,
    reduce,
};

#[derive(Debug, Error)]
enum TestError {
    #[error("workflow log configuration failed")]
    Config(#[from] LogConfigError),
    #[error("workflow commit failed")]
    Commit(#[from] CommitError),
    #[error("workflow replay failed")]
    Replay(#[from] ReductionError),
}

pub(crate) fn key() -> StageKey {
    StageKey::derive(
        OperationId::PinnedObject,
        StageId::HydrateObject,
        &StageInput {
            generation: GenerationId::from_digest([1; 32]),
            content: ContentId::<ObjectDomain>::from_digest([2; 32]),
        },
        ContentId::<CapabilityDomain>::from_digest([3; 32]),
        ContentId::<ConfigurationDomain>::from_digest([4; 32]),
    )
}

fn event(key: StageKey, kind: EventKind) -> WorkflowEvent {
    WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key,
        kind,
    }
}

#[test]
fn compact_key_and_event_do_not_repeat_verbose_stage_inputs() {
    assert_eq!(size_of::<StageKey>(), 32);
    assert!(size_of::<WorkflowEvent>() <= 96);
    let base = key();
    let changed = StageKey::derive(
        OperationId::PinnedObject,
        StageId::HydrateObject,
        &StageInput {
            generation: GenerationId::from_digest([1; 32]),
            content: ContentId::<ObjectDomain>::from_digest([9; 32]),
        },
        ContentId::<CapabilityDomain>::from_digest([3; 32]),
        ContentId::<ConfigurationDomain>::from_digest([4; 32]),
    );
    assert_ne!(base, changed);
}

#[test]
fn every_durable_crash_prefix_derives_only_its_pending_effect() -> Result<(), TestError> {
    let key = key();
    let output = ContentId::<ObjectDomain>::from_digest([9; 32]);
    let events = [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output },
        EventKind::Verified { output },
        EventKind::PublicationStarted { output },
        EventKind::Published { output },
    ];
    let expected = [
        Some(Effect {
            key,
            action: EffectAction::Admit,
        }),
        Some(Effect {
            key,
            action: EffectAction::Stage,
        }),
        Some(Effect {
            key,
            action: EffectAction::Verify { output },
        }),
        Some(Effect {
            key,
            action: EffectAction::BeginPublication { output },
        }),
        Some(Effect {
            key,
            action: EffectAction::Publish { output },
        }),
        None,
    ];
    for (length, expected_effect) in (1..=events.len()).zip(expected) {
        let mut log = MemoryWorkflowLog::new(events.len())?;
        for kind in events.iter().copied().take(length) {
            let _reduction = log.append_then_reduce(event(key, kind))?;
        }
        assert_eq!(log.replay()?.pending_effect, expected_effect);
    }
    Ok(())
}

#[test]
fn conflicts_and_cancellation_after_publication_start_fail_closed() -> Result<(), TestError> {
    let key = key();
    let first = ContentId::<ObjectDomain>::from_digest([9; 32]);
    let second = ContentId::<ObjectDomain>::from_digest([8; 32]);
    let mut log = MemoryWorkflowLog::new(6)?;
    for kind in [
        EventKind::Requested,
        EventKind::Admitted,
        EventKind::Staged { output: first },
    ] {
        let _reduction = log.append_then_reduce(event(key, kind))?;
    }
    assert_eq!(
        log.append_then_reduce(event(key, EventKind::Verified { output: second })),
        Err(CommitError::Reduction(ReductionError::ConflictingOutput {
            phase: PhaseName::Staged,
            expected: first,
            observed: second,
        }))
    );
    let _verified = log.append_then_reduce(event(key, EventKind::Verified { output: first }))?;
    let _publication =
        log.append_then_reduce(event(key, EventKind::PublicationStarted { output: first }))?;
    assert_eq!(
        log.append_then_reduce(event(key, EventKind::Cancelled)),
        Err(CommitError::Reduction(
            ReductionError::CannotCancelPublishing
        ))
    );
    Ok(())
}

type ReductionCase = (WorkflowState, EventKind, Result<Reduction, ReductionError>);

#[test]
fn forward_transitions_return_exact_state_and_next_effect() {
    let key = key();
    let output = ContentId::<ObjectDomain>::from_digest([7; 32]);
    let cases = [
        (
            WorkflowState::New,
            EventKind::Requested,
            Ok(expected(key, Phase::Requested, EffectAction::Admit)),
        ),
        (
            keyed(key, Phase::Requested),
            EventKind::Admitted,
            Ok(expected(key, Phase::Admitted, EffectAction::Stage)),
        ),
        (
            keyed(key, Phase::Admitted),
            EventKind::Staged { output },
            Ok(expected(
                key,
                Phase::Staged(output),
                EffectAction::Verify { output },
            )),
        ),
        (
            keyed(key, Phase::Staged(output)),
            EventKind::Verified { output },
            Ok(expected(
                key,
                Phase::Verified(output),
                EffectAction::BeginPublication { output },
            )),
        ),
        (
            keyed(key, Phase::Verified(output)),
            EventKind::PublicationStarted { output },
            Ok(expected(
                key,
                Phase::Publishing(output),
                EffectAction::Publish { output },
            )),
        ),
        (
            keyed(key, Phase::Publishing(output)),
            EventKind::Published { output },
            Ok(Reduction {
                state: keyed(key, Phase::Published(output)),
                effect: None,
            }),
        ),
    ];
    assert_cases(key, cases);
}

#[test]
fn duplicate_and_output_conflict_are_exact() {
    let key = key();
    let output = ContentId::<ObjectDomain>::from_digest([7; 32]);
    let different_output = ContentId::<ObjectDomain>::from_digest([8; 32]);
    let cases = [
        (
            keyed(key, Phase::Staged(output)),
            EventKind::Staged { output },
            Ok(Reduction {
                state: keyed(key, Phase::Staged(output)),
                effect: None,
            }),
        ),
        (
            keyed(key, Phase::Staged(output)),
            EventKind::Verified {
                output: different_output,
            },
            Err(ReductionError::ConflictingOutput {
                phase: PhaseName::Staged,
                expected: output,
                observed: different_output,
            }),
        ),
    ];
    assert_cases(key, cases);
}

#[test]
fn failure_cancel_and_impossible_classes_are_exact() {
    let key = key();
    let output = ContentId::<ObjectDomain>::from_digest([7; 32]);
    let cases = [
        (
            keyed(
                key,
                Phase::Failed {
                    code: FailureCode::Runtime,
                    prior: PriorFacts::Requested,
                },
            ),
            EventKind::Failed {
                code: FailureCode::Verification,
            },
            Err(ReductionError::ConflictingFailure {
                phase: PhaseName::Failed,
                expected: FailureCode::Runtime,
                observed: FailureCode::Verification,
            }),
        ),
        (
            keyed(key, Phase::Publishing(output)),
            EventKind::Cancelled,
            Err(ReductionError::CannotCancelPublishing),
        ),
        (
            WorkflowState::New,
            EventKind::Admitted,
            Err(ReductionError::ImpossibleTransition {
                phase: PhaseName::New,
                event: EventName::Admitted,
            }),
        ),
        (
            keyed(key, Phase::Publishing(output)),
            EventKind::Failed {
                code: FailureCode::Runtime,
            },
            Ok(expected(
                key,
                Phase::PublicationUnknown {
                    output,
                    code: FailureCode::Runtime,
                },
                EffectAction::ReconcilePublication { output },
            )),
        ),
    ];
    assert_cases(key, cases);
}

#[test]
fn foreign_key_and_version_are_rejected_exactly() {
    let key = key();
    let foreign_key = StageKey::from([0x55; 32]);
    assert_eq!(
        reduce(
            keyed(key, Phase::Requested),
            event(foreign_key, EventKind::Admitted),
        ),
        Err(ReductionError::StageKeyMismatch {
            expected: key,
            observed: foreign_key,
        })
    );
    assert_eq!(
        reduce(
            WorkflowState::New,
            WorkflowEvent {
                version: WorkflowVersion::from(2),
                key,
                kind: EventKind::Requested,
            },
        ),
        Err(ReductionError::UnknownVersion {
            observed: WorkflowVersion::from(2),
        })
    );
}

fn assert_cases(key: StageKey, cases: impl IntoIterator<Item = ReductionCase>) {
    for (state, kind, expected) in cases {
        assert_eq!(reduce(state, event(key, kind)), expected);
    }
}

const fn keyed(key: StageKey, phase: Phase) -> WorkflowState {
    WorkflowState::Keyed { key, phase }
}

const fn expected(key: StageKey, phase: Phase, action: EffectAction) -> Reduction {
    Reduction {
        state: WorkflowState::Keyed { key, phase },
        effect: Some(Effect { key, action }),
    }
}
