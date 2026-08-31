//! Deterministic headless projection tests.

use wave_application_core::{
    AdaptiveDisposition, ApplicationInput, ApplicationReply, ApplicationService, BatteryState,
    ByteCount, Capability, CapabilityDomain, CapabilityHealth, CorrelationId, DiagnosticCode,
    ExecutionState, GenerationId, IndexSnapshotId, InputText, InputTextError, OperationBudget, Pin,
    Pressure, ReplyBody, ResourceBudget, RetryBudget, Terminal,
};
use wave_application_gpui_shell::{
    AdaptiveProjection, ApplyError, BatchReceipt, ExecutionProjection, HealthProjection,
    MAX_BATCH_REPLIES, ProjectionState, SURFACE_COUNT, ShellState, Surface, SurfaceStatus,
};

#[derive(Debug)]
enum TestError {
    Input(InputTextError),
    Apply(ApplyError),
    Unexpected(&'static str),
}

impl std::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => write!(formatter, "input error: {error:?}"),
            Self::Apply(error) => write!(formatter, "projection error: {error:?}"),
            Self::Unexpected(message) => write!(formatter, "unexpected reply: {message}"),
        }
    }
}

impl std::error::Error for TestError {}

impl From<InputTextError> for TestError {
    fn from(error: InputTextError) -> Self {
        Self::Input(error)
    }
}

impl From<ApplyError> for TestError {
    fn from(error: ApplyError) -> Self {
        Self::Apply(error)
    }
}

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(b"gpui-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"gpui-snapshot"),
    }
}

fn bundle() -> wave_application_core::ContentId<CapabilityDomain> {
    wave_application_core::ContentId::from_canonical_bytes(b"gpui-analyzer-bundle")
}

fn budget(operations: u8, retries: u8) -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(4096),
        nvme_free: ByteCount::from(8192),
        operations: OperationBudget::from(operations),
        retries: RetryBudget::from(retries),
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery: BatteryState::Normal,
    }
}

fn reply(correlation: u64, body: ReplyBody, terminal: Terminal) -> ApplicationReply {
    ApplicationReply {
        correlation: CorrelationId(correlation),
        body,
        terminal,
        diagnostic: None,
    }
}

fn apply(state: &mut ShellState, replies: &[ApplicationReply]) -> Result<BatchReceipt, ApplyError> {
    state.apply_batch(replies)
}

#[test]
fn first_frame_is_stable_without_polling_or_allocated_rows() {
    let state = ShellState::default();
    let summaries = state.summaries();

    assert_eq!(summaries.len(), SURFACE_COUNT);
    assert_eq!(
        summaries.map(|summary| summary.surface),
        [
            Surface::Generation,
            Surface::Adaptive,
            Surface::Execution,
            Surface::Index,
            Surface::Graph,
            Surface::Vector,
            Surface::Health,
        ]
    );
    assert!(
        summaries
            .into_iter()
            .all(|summary| summary.state == ProjectionState::Checking)
    );
    assert_eq!(state.last_correlation(), None);
    assert_eq!(state.notification_epoch(), 0);
}

#[test]
fn compiler_passthrough_is_projected_as_degraded_artifact_output() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let source = text("fn gpui() {}")?;
    let reply = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(11),
        language: text("rust")?,
        stage: text("parse")?,
        package: text("demo")?,
        source,
    });
    let mut state = ShellState::default();
    apply(&mut state, &[reply])?;

    assert!(
        matches!(reply.body, ReplyBody::CompilerPassthrough { source: returned, .. } if returned == source)
    );
    assert_eq!(
        state.generation(),
        SurfaceStatus::Degraded {
            terminal: Terminal::Partial {
                emitted: 1,
                unavailable: Capability::CompilerOutput,
            },
            capability: Capability::CompilerOutput,
        }
    );
    assert_eq!(
        state.summaries()[0].state,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    Ok(())
}

#[test]
fn adaptive_decision_is_projected_without_replacing_its_typed_cause() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let reply = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(12),
        pin: pin(),
        bundle: bundle(),
        budget: budget(0, 1),
    });
    let mut state = ShellState::default();
    apply(&mut state, &[reply])?;

    assert!(matches!(
        state.adaptive(),
        AdaptiveProjection::Reported {
            disposition: AdaptiveDisposition::Overloaded(_),
            terminal: Terminal::Degraded {
                unavailable: Capability::LocalAnalyzer,
                ..
            },
        }
    ));
    assert_eq!(
        state.summaries()[1].state,
        ProjectionState::Degraded(Capability::LocalAnalyzer)
    );
    Ok(())
}

#[test]
fn execution_started_and_terminal_are_projected_as_distinct_states() -> Result<(), TestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(13),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1),
    });
    let ReplyBody::ExecutionStarted {
        operation,
        transition,
    } = admitted.body
    else {
        return Err(TestError::Unexpected(
            "recover should start a local operation",
        ));
    };
    let mut state = ShellState::default();
    apply(&mut state, &[admitted])?;
    assert_eq!(
        state.execution(),
        ExecutionProjection::Started {
            operation,
            transition,
            terminal: Terminal::Accepted { operation },
        }
    );
    assert_eq!(state.summaries()[2].state, ProjectionState::Accepted);

    let pending = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(14),
        operation,
    });
    apply(&mut state, &[pending])?;
    assert!(matches!(
        state.execution(),
        ExecutionProjection::Reported {
            state: ExecutionState::Pending { operation: observed, .. },
            terminal: Terminal::Accepted { operation: accepted },
        } if observed == operation && accepted == operation
    ));
    assert_eq!(state.summaries()[2].state, ProjectionState::Active);

    let completed = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(15),
        operation,
    });
    apply(&mut state, &[completed])?;
    assert!(matches!(
        state.execution(),
        ExecutionProjection::Reported {
            state: ExecutionState::Completed { operation: observed, .. },
            terminal: Terminal::Complete { emitted: 1 },
        } if observed == operation
    ));
    assert_eq!(state.summaries()[2].state, ProjectionState::Ready);
    Ok(())
}

#[test]
fn health_preserves_all_six_capability_facts() -> Result<(), TestError> {
    let facts = [
        CapabilityHealth::LocalReady(Capability::CompilerRegistry),
        CapabilityHealth::Unavailable(Capability::CompilerOutput),
        CapabilityHealth::Unavailable(Capability::Index),
        CapabilityHealth::Unavailable(Capability::Graph),
        CapabilityHealth::Unavailable(Capability::Vector),
        CapabilityHealth::Unavailable(Capability::LocalAnalyzer),
    ];
    let incoming = reply(
        20,
        ReplyBody::Health(facts),
        Terminal::Partial {
            emitted: 2,
            unavailable: Capability::CompilerOutput,
        },
    );
    let mut state = ShellState::default();
    apply(&mut state, &[incoming])?;

    assert_eq!(
        state.health(),
        HealthProjection::Reported {
            facts,
            terminal: incoming.terminal,
        }
    );
    assert_eq!(state.health().facts(), Some(facts));
    assert_eq!(
        state.summaries()[6].state,
        ProjectionState::Degraded(Capability::CompilerOutput)
    );
    assert_eq!(state.last_correlation(), Some(CorrelationId(20)));
    Ok(())
}

#[test]
fn unavailable_lower_plane_is_typed_and_bounded() -> Result<(), TestError> {
    let incoming = reply(
        30,
        ReplyBody::DependencyUnavailable {
            capability: Capability::Vector,
        },
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::Vector,
        },
    );
    let mut state = ShellState::default();
    apply(&mut state, &[incoming])?;
    assert_eq!(
        state.summaries()[5].state,
        ProjectionState::Degraded(Capability::Vector)
    );
    Ok(())
}

#[test]
fn rejected_diagnostic_is_retained_without_erasing_its_code() -> Result<(), TestError> {
    let incoming = ApplicationReply {
        correlation: CorrelationId(40),
        body: ReplyBody::Rejected,
        terminal: Terminal::Failed,
        diagnostic: Some(wave_application_core::Diagnostic {
            code: DiagnosticCode::DependencyUnavailable,
            detail: wave_application_core::DiagnosticDetail::Capability(Capability::Index),
        }),
    };
    let mut state = ShellState::default();
    apply(&mut state, &[incoming])?;
    assert_eq!(
        state.last_diagnostic_code(),
        Some(DiagnosticCode::DependencyUnavailable)
    );
    assert_eq!(state.last_terminal(), Some(Terminal::Failed));
    Ok(())
}

#[test]
fn oversized_batch_is_rejected_before_state_mutation() {
    let incoming = reply(50, ReplyBody::Rejected, Terminal::Failed);
    let replies = [incoming; MAX_BATCH_REPLIES + 1];
    let mut state = ShellState::default();
    let result = state.apply_batch(&replies);

    assert_eq!(
        result,
        Err(ApplyError::BatchTooLarge {
            limit: MAX_BATCH_REPLIES,
            actual: MAX_BATCH_REPLIES + 1,
        })
    );
    assert_eq!(state.last_correlation(), None);
    assert_eq!(state.notification_epoch(), 0);
}

#[test]
fn empty_boundary_is_fused_without_notification() -> Result<(), ApplyError> {
    let mut state = ShellState::default();
    let receipt = apply(&mut state, &[])?;
    assert_eq!(receipt.applied_replies(), 0);
    assert_eq!(receipt.notifications(), 0);
    assert_eq!(receipt.notification_epoch(), 0);
    Ok(())
}
