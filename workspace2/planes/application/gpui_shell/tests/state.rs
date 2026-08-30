//! Deterministic headless projection tests.

use wave_application_core::{
    APPLICATION_OPERATION, ApplicationReply, Capability, CapabilityHealth, CorrelationId,
    DiagnosticCode, InputText, ProgressCursor, ProgressPage, ReplyBody, Terminal,
};
use wave_application_gpui_shell::{
    ApplyError, BatchReceipt, HealthProjection, MAX_BATCH_REPLIES, ProgressProjection,
    ProjectionState, SURFACE_COUNT, ShellState, Surface,
};

#[derive(Debug)]
enum TestError {
    Apply(ApplyError),
    Text(wave_application_core::InputTextError),
}

impl std::fmt::Display for TestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Apply(error) => write!(formatter, "apply error: {error:?}"),
            Self::Text(error) => write!(formatter, "fixture text error: {error:?}"),
        }
    }
}

impl std::error::Error for TestError {}

impl From<ApplyError> for TestError {
    fn from(error: ApplyError) -> Self {
        Self::Apply(error)
    }
}

impl From<wave_application_core::InputTextError> for TestError {
    fn from(error: wave_application_core::InputTextError) -> Self {
        Self::Text(error)
    }
}

fn apply(state: &mut ShellState, replies: &[ApplicationReply]) -> Result<BatchReceipt, ApplyError> {
    state.apply_batch(replies)
}

fn reply(correlation: u64, body: ReplyBody, terminal: Terminal) -> ApplicationReply {
    ApplicationReply {
        correlation: CorrelationId(correlation),
        body,
        terminal,
        diagnostic: None,
    }
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
            Surface::Index,
            Surface::Graph,
            Surface::Vector,
            Surface::Health,
            Surface::Progress,
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
fn core_health_is_projected_directly_and_marks_index_unavailable() -> Result<(), ApplyError> {
    let facts = [
        CapabilityHealth::LocalReady(Capability::Compiler),
        CapabilityHealth::Unavailable(Capability::Index),
        CapabilityHealth::Unavailable(Capability::Graph),
        CapabilityHealth::Unavailable(Capability::Vector),
    ];
    let incoming = reply(
        41,
        ReplyBody::Health(facts),
        Terminal::Complete { emitted: 4 },
    );
    let mut state = ShellState::default();

    let receipt = apply(&mut state, &[incoming])?;

    assert_eq!(receipt.applied_replies(), 1);
    assert_eq!(receipt.notifications(), 1);
    assert_eq!(
        state.health(),
        HealthProjection::Reported {
            facts,
            terminal: Terminal::Complete { emitted: 4 },
        }
    );
    assert_eq!(
        state.summaries()[1].state,
        ProjectionState::Degraded(Capability::Index)
    );
    assert_eq!(
        state.summaries()[4].state,
        ProjectionState::Degraded(Capability::Index)
    );
    assert_eq!(state.last_correlation(), Some(CorrelationId(41)));
    Ok(())
}

#[test]
fn core_progress_page_is_projected_without_rebuilding_cursor_state() -> Result<(), ApplyError> {
    let page = ProgressPage::Terminal {
        terminal: Terminal::Complete { emitted: 1 },
        next: ProgressCursor::Finished,
    };
    let mut state = ShellState::default();

    apply(
        &mut state,
        &[reply(
            42,
            ReplyBody::Progress(page),
            Terminal::Complete { emitted: 1 },
        )],
    )?;

    assert_eq!(state.progress(), ProgressProjection::Reported(page));
    assert_eq!(state.summaries()[5].state, ProjectionState::Ready);
    Ok(())
}

#[test]
fn core_progress_admission_and_pending_page_stay_typed_and_active() -> Result<(), ApplyError> {
    let admitted = reply(
        43,
        ReplyBody::ProgressStarted {
            operation: APPLICATION_OPERATION,
        },
        Terminal::Complete { emitted: 1 },
    );
    let pending_page = ProgressPage::Pending {
        cursor: ProgressCursor::Offset(1),
    };
    let pending = reply(
        44,
        ReplyBody::Progress(pending_page),
        Terminal::Complete { emitted: 1 },
    );
    let mut state = ShellState::default();

    apply(&mut state, &[admitted])?;
    assert_eq!(state.summaries()[5].state, ProjectionState::Active);
    apply(&mut state, &[pending])?;

    assert_eq!(state.progress(), ProgressProjection::Reported(pending_page));
    assert_eq!(state.summaries()[5].state, ProjectionState::Active);
    Ok(())
}

#[test]
fn one_batch_coalesces_multiple_core_replies_to_one_notification() -> Result<(), ApplyError> {
    let health = reply(
        50,
        ReplyBody::Health([
            CapabilityHealth::LocalReady(Capability::Compiler),
            CapabilityHealth::Unavailable(Capability::Index),
            CapabilityHealth::Unavailable(Capability::Graph),
            CapabilityHealth::Unavailable(Capability::Vector),
        ]),
        Terminal::Complete { emitted: 4 },
    );
    let progress = reply(
        51,
        ReplyBody::Progress(ProgressPage::Finished),
        Terminal::Complete { emitted: 1 },
    );
    let mut state = ShellState::default();

    let receipt = apply(&mut state, &[health, progress])?;

    assert_eq!(receipt.applied_replies(), 2);
    assert_eq!(receipt.notifications(), 1);
    assert_eq!(receipt.notification_epoch(), 1);
    assert_eq!(state.notification_epoch(), 1);
    assert_eq!(state.last_correlation(), Some(CorrelationId(51)));
    Ok(())
}

#[test]
fn dependency_unavailable_is_typed_and_bounded() -> Result<(), ApplyError> {
    let incoming = reply(
        60,
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
        state.summaries()[3].state,
        ProjectionState::Degraded(Capability::Vector)
    );
    Ok(())
}

#[test]
fn oversized_batch_is_rejected_before_state_mutation() {
    let incoming = reply(70, ReplyBody::Rejected, Terminal::Failed);
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
fn rejected_core_diagnostic_is_retained_without_a_shell_error_type() -> Result<(), TestError> {
    let detail = InputText::try_from_str("provider-unavailable").map_err(TestError::from)?;
    let incoming = ApplicationReply {
        correlation: CorrelationId(80),
        body: ReplyBody::Rejected,
        terminal: Terminal::Failed,
        diagnostic: Some(wave_application_core::Diagnostic {
            code: DiagnosticCode::DependencyUnavailable,
            detail: wave_application_core::DiagnosticDetail::Text(detail),
        }),
    };
    let mut state = ShellState::default();

    apply(&mut state, &[incoming]).map_err(TestError::from)?;

    assert_eq!(
        state.last_diagnostic_code(),
        Some(DiagnosticCode::DependencyUnavailable)
    );
    assert_eq!(state.last_terminal(), Some(Terminal::Failed));
    Ok(())
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
