use core::cell::Cell;
use std::{error::Error, fmt};

use nudox_observe::{DropNewest, FlightRecorder, Probe};
use wave_application_core::{
    AdaptiveDisposition, ApplicationEvent, ApplicationInput, ApplicationService, BatteryState,
    ByteCount, Capability, CapabilityDomain, CapabilityHealth, CapabilityTransition, ContentId,
    CorrelationId, DiagnosticCode, ExecutionState, GenerationId, IndexSnapshotId, InputText,
    InputTextError, OperationBudget, OperationKey, Pin, Pressure, ReplyBody, ResourceBudget,
    RetryBudget, Terminal,
};

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

fn pin() -> Pin {
    Pin {
        generation: GenerationId::from_canonical_bytes(b"application-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"application-snapshot"),
    }
}

fn bundle() -> ContentId<CapabilityDomain> {
    ContentId::from_canonical_bytes(b"verified-analyzer-bundle")
}

fn budget(operations: u8, retries: u8, battery: BatteryState) -> ResourceBudget {
    ResourceBudget {
        ram_free: ByteCount::from(4096),
        nvme_free: ByteCount::from(8192),
        operations: OperationBudget::from(operations),
        retries: RetryBudget::from(retries),
        memory_pressure: Pressure::Relaxed,
        storage_pressure: Pressure::Relaxed,
        battery,
    }
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

fn acquire_and_complete(
    service: &mut ApplicationService,
) -> Result<OperationKey, ServiceTestError> {
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(30),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    let ReplyBody::ExecutionStarted {
        operation,
        transition: CapabilityTransition::Acquire {
            bundle: selected, ..
        },
    } = admitted.body
    else {
        return Err(ServiceTestError::UnexpectedReply("acquisition admission"));
    };
    assert_eq!(selected, bundle());
    assert_eq!(admitted.terminal, Terminal::Accepted { operation });

    let pending = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(31),
        operation,
    });
    assert!(matches!(
        pending.body,
        ReplyBody::Execution(ExecutionState::Pending {
            transition: CapabilityTransition::Acquire { bundle: selected, .. },
            ..
        }) if selected == bundle()
    ));
    let completed = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(32),
        operation,
    });
    assert!(matches!(
        completed.body,
        ReplyBody::Execution(ExecutionState::Completed {
            transition: CapabilityTransition::Acquire { bundle: selected, .. },
            ..
        }) if selected == bundle()
    ));
    assert_eq!(completed.terminal, Terminal::Complete { emitted: 1 });
    Ok(operation)
}

#[test]
fn compiler_registry_passthrough_is_source_sensitive_but_never_claims_an_artifact()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let first_source = text("fn first() {}")?;
    let first = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(11),
        language: text("rust")?,
        stage: text("lower-ir")?,
        package: text("demo")?,
        source: first_source,
    });
    assert_eq!(
        first.terminal,
        Terminal::Partial {
            emitted: 1,
            unavailable: Capability::CompilerOutput,
        }
    );
    assert!(matches!(
        first.body,
        ReplyBody::CompilerPassthrough { source, .. } if source == first_source
    ));
    assert_eq!(
        first.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::DependencyUnavailable)
    );

    let second_source = text("fn second() {}")?;
    let second = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(12),
        language: text("rust")?,
        stage: text("parse")?,
        package: text("demo")?,
        source: second_source,
    });
    assert!(matches!(
        second.body,
        ReplyBody::CompilerPassthrough { source, .. } if source == second_source
    ));
    assert_ne!(first.body, second.body);
    Ok(())
}

#[test]
fn absent_index_plane_is_honestly_degraded() -> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let search = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(20),
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
    Ok(())
}

#[test]
fn semantic_limit_plus_one_is_retained_but_larger_transport_text_is_rejected()
-> Result<(), ServiceTestError> {
    let limit_plus_one = text("123456789012345678901234567890123")?;
    let mut service = ApplicationService::new();
    let rejected = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(25),
        language: text("rust")?,
        stage: text("parse")?,
        package: limit_plus_one,
        source: text("fn bounded() {}")?,
    });
    assert_eq!(rejected.terminal, Terminal::Failed);
    assert_eq!(
        rejected.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::SemanticTextTooLong)
    );
    let transport_error = InputText::try_from_str("1234567890123456789012345678901234");
    assert!(matches!(
        transport_error,
        Err(InputTextError {
            actual: 34,
            maximum: 33,
        })
    ));
    Ok(())
}

#[test]
fn outage_acquires_real_local_bundle_then_exposes_bounded_remote_retry()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let operation = acquire_and_complete(&mut service)?;

    let fused = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(33),
        operation,
    });
    assert!(matches!(
        fused.body,
        ReplyBody::Execution(ExecutionState::Completed { .. })
    ));
    let retry = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(34),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    assert!(matches!(
        retry.body,
        ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
            retries_remaining,
            ..
        }) if retries_remaining.get() == 0
    ));
    assert_eq!(
        retry.terminal,
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::Remote,
        }
    );
    Ok(())
}

#[test]
fn cancellation_fuses_without_acquiring_and_critical_contraction_releases_after_completion()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(40),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    let ReplyBody::ExecutionStarted { operation, .. } = admitted.body else {
        return Err(ServiceTestError::UnexpectedReply("cancellable admission"));
    };
    let cancelled = service.execute(&ApplicationInput::Cancel {
        correlation: CorrelationId(41),
        operation,
    });
    assert!(matches!(
        cancelled.body,
        ReplyBody::Execution(ExecutionState::Cancelled { .. })
    ));
    let fused = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(42),
        operation,
    });
    assert_eq!(cancelled.body, fused.body);
    let health = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(43),
    });
    let ReplyBody::Health(facts) = health.body else {
        return Err(ServiceTestError::UnexpectedReply(
            "health after cancellation",
        ));
    };
    assert!(facts.contains(&CapabilityHealth::Unavailable(Capability::LocalAnalyzer)));

    let _completed_operation = acquire_and_complete(&mut service)?;
    let release = service.execute(&ApplicationInput::ReleaseLocal {
        correlation: CorrelationId(47),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 0, BatteryState::Critical),
    });
    assert!(matches!(
        release.body,
        ReplyBody::ExecutionStarted {
            transition: CapabilityTransition::Release { .. },
            ..
        }
    ));
    Ok(())
}

#[test]
fn zero_action_credit_preserves_exact_adaptive_overload() {
    let mut service = ApplicationService::new();
    let reply = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(50),
        pin: pin(),
        bundle: bundle(),
        budget: budget(0, 1, BatteryState::Normal),
    });
    assert!(matches!(
        reply.body,
        ReplyBody::Adaptive(AdaptiveDisposition::Overloaded(overload))
            if matches!(overload.resource, wave_application_core::ResourceClass::Operations)
    ));
}

#[test]
fn disabled_probe_formats_nothing_and_bounded_probe_retains_exact_event() {
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
    let mut recorder: FlightRecorder<ApplicationEvent, DropNewest, 1> = FlightRecorder::new();
    let reply = service.execute_observed(
        &ApplicationInput::Health {
            correlation: CorrelationId(60),
        },
        &mut recorder,
    );
    let retained = recorder.events().next();
    assert_eq!(
        retained.map(|event| event.correlation),
        Some(reply.correlation)
    );
}
