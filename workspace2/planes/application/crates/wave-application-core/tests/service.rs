use core::cell::Cell;
use std::{error::Error, fmt};

use nudox_observe::{DropNewest, FlightRecorder, Probe};
use wave_application_core::{
    APPLICATION_OPERATION, ApplicationEvent, ApplicationInput, ApplicationService, Capability,
    CorrelationId, DiagnosticCode, InputText, InputTextError, OperationKey, ProgressCursor,
    ProgressPage, ReplyBody, Terminal,
};

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

#[derive(Debug)]
enum ServiceTestError {
    Input(InputTextError),
    UnexpectedReply(&'static str),
}

impl fmt::Display for ServiceTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(
                formatter,
                "test input exceeded {} bytes: {}",
                error.maximum, error.actual
            ),
            Self::UnexpectedReply(law) => write!(formatter, "unexpected reply: {law}"),
        }
    }
}

impl Error for ServiceTestError {}

impl From<InputTextError> for ServiceTestError {
    fn from(error: InputTextError) -> Self {
        Self::Input(error)
    }
}

#[test]
fn real_compiler_request_is_source_sensitive() -> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let first_source = text("fn first() {}")?;
    let generated = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(11),
        language: text("rust")?,
        stage: text("lower-ir")?,
        package: text("demo")?,
        source: first_source,
    });
    assert_eq!(generated.correlation, CorrelationId(11));
    assert_eq!(generated.terminal, Terminal::Complete { emitted: 1 });
    assert!(matches!(
        generated.body,
        ReplyBody::Generated { output, .. } if output == first_source
    ));
    let second_source = text("fn second() {}")?;
    let generated_again = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(111),
        language: text("rust")?,
        stage: text("parse")?,
        package: text("demo")?,
        source: second_source,
    });
    assert!(matches!(
        generated_again.body,
        ReplyBody::Generated { output, .. } if output == second_source
    ));
    assert_ne!(generated.body, generated_again.body);

    let unsupported = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(112),
        language: text("typescript")?,
        stage: text("lower-ir")?,
        package: text("demo")?,
        source: text("const typed = 1;")?,
    });
    assert_eq!(unsupported.terminal, Terminal::Failed);
    assert_eq!(
        unsupported.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::UnsupportedCompilerStage)
    );

    Ok(())
}

#[test]
fn absent_index_plane_is_honestly_degraded() -> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let search = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(12),
        snapshot: text("any-unverified-snapshot")?,
        query: text("render")?,
        limit: 2,
    });
    assert_eq!(
        search.terminal,
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::Index,
        }
    );
    assert!(matches!(
        search.body,
        ReplyBody::DependencyUnavailable {
            capability: Capability::Index,
        }
    ));
    assert_eq!(
        search.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::DependencyUnavailable)
    );
    Ok(())
}

#[test]
fn malformed_semantic_input_and_limit_plus_one_preserve_exact_rejections_without_state_change()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let rejected_language = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(21),
        language: text("go")?,
        stage: text("parse")?,
        package: text("demo")?,
        source: text("package demo")?,
    });
    assert_eq!(rejected_language.terminal, Terminal::Failed);
    assert_eq!(
        rejected_language
            .diagnostic
            .map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::UnknownLanguage)
    );
    let rejected_limit = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(22),
        snapshot: text("primary")?,
        query: text("render")?,
        limit: 5,
    });
    assert_eq!(rejected_limit.terminal, Terminal::Failed);
    assert_eq!(
        rejected_limit.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::ResultLimitExceeded)
    );
    let healthy = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(23),
    });
    assert_eq!(healthy.terminal, Terminal::Complete { emitted: 4 });
    Ok(())
}

#[test]
fn independent_cursors_replay_one_service_owned_history_and_terminal_fuses()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::BeginProgress {
        correlation: CorrelationId(30),
    });
    assert!(matches!(
        admitted.body,
        ReplyBody::ProgressStarted {
            operation: APPLICATION_OPERATION,
        }
    ));
    let first = service.execute(&ApplicationInput::Progress {
        correlation: CorrelationId(31),
        cursor: ProgressCursor::Start,
    });
    let second = service.execute(&ApplicationInput::Progress {
        correlation: CorrelationId(32),
        cursor: ProgressCursor::Start,
    });
    let ReplyBody::Progress(ProgressPage::Events {
        events: first_events,
        next: first_next,
    }) = first.body
    else {
        return Err(ServiceTestError::UnexpectedReply("first progress page"));
    };
    let ReplyBody::Progress(ProgressPage::Events {
        events: second_events,
        next: second_next,
    }) = second.body
    else {
        return Err(ServiceTestError::UnexpectedReply("replayed progress page"));
    };
    assert_eq!(first_events, second_events);
    assert_eq!(first_next, second_next);
    assert_eq!(first_events.len(), 1);
    let pending = service.execute(&ApplicationInput::Progress {
        correlation: CorrelationId(31),
        cursor: first_next,
    });
    assert!(matches!(
        pending.body,
        ReplyBody::Progress(ProgressPage::Pending {
            cursor: ProgressCursor::Offset(1),
        })
    ));
    let fused = service.execute(&ApplicationInput::Progress {
        correlation: CorrelationId(31),
        cursor: ProgressCursor::Finished,
    });
    assert!(matches!(
        fused.body,
        ReplyBody::Progress(ProgressPage::Finished)
    ));
    Ok(())
}

#[test]
fn cancellation_is_the_named_winner_and_cannot_later_turn_into_completion() {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::BeginProgress {
        correlation: CorrelationId(40),
    });
    assert_eq!(admitted.terminal, Terminal::Complete { emitted: 1 });
    let cancellation = service.execute(&ApplicationInput::Cancel {
        correlation: CorrelationId(41),
        operation: APPLICATION_OPERATION,
    });
    assert_eq!(cancellation.terminal, Terminal::Cancelled { emitted: 0 });
    let progress = service.execute(&ApplicationInput::Progress {
        correlation: CorrelationId(42),
        cursor: ProgressCursor::Start,
    });
    assert!(matches!(
        progress.body,
        ReplyBody::Progress(ProgressPage::Terminal {
            terminal: Terminal::Cancelled { emitted: 0 },
            next: ProgressCursor::Finished,
        })
    ));
    let unknown = service.execute(&ApplicationInput::Cancel {
        correlation: CorrelationId(43),
        operation: OperationKey(99),
    });
    assert_eq!(unknown.terminal, Terminal::Failed);
    assert_eq!(
        unknown.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::OperationUnavailable)
    );
}

#[test]
fn disabled_probe_keeps_typed_event_builder_lazy_and_a_bounded_live_probe_keeps_exact_event() {
    let constructed = Cell::new(0_u8);
    let mut disabled = ();
    disabled.record_with(|| {
        constructed.set(constructed.get() + 1);
        ApplicationEvent {
            correlation: CorrelationId(0),
            terminal: Terminal::Complete { emitted: 0 },
        }
    });
    assert_eq!(constructed.get(), 0);

    let mut service = ApplicationService::new();
    let mut recorder = FlightRecorder::<ApplicationEvent, DropNewest, 1>::new();
    let reply = service.execute_observed(
        &ApplicationInput::Health {
            correlation: CorrelationId(51),
        },
        &mut recorder,
    );
    assert_eq!(recorder.len(), 1);
    assert_eq!(
        recorder.events().next(),
        Some(&ApplicationEvent {
            correlation: CorrelationId(51),
            terminal: reply.terminal,
        })
    );
}
