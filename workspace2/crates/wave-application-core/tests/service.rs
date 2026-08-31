use core::cell::Cell;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Wake, Waker},
};

use nudox_observe::{DropNewest, FlightRecorder, Probe};
use wave_application_core::{
    AdaptiveDisposition, ApplicationEvent, ApplicationInput, ApplicationService, BatteryState,
    ByteCount, Capability, CapabilityDomain, CapabilityHealth, CapabilityTransition, ContentId,
    CorrelationId, DiagnosticCode, ExecutionState, GenerationId, InconsistentRecovery,
    IndexSnapshotId, InputText, InputTextError, OperationBudget, OperationKey, Pin, Pressure,
    RecoveryCause, ReplyBody, ResourceBudget, RetryBudget, Terminal,
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

#[derive(Debug, Default)]
struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::Release);
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
fn accepted_registry_bytes_never_claim_compiler_output_and_rejections_keep_causes()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let accepted = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(11),
        language: text("rust")?,
        stage: text("lower-ir")?,
        package: text("demo")?,
        source: text("fn first() {}")?,
    });
    assert_eq!(
        accepted.terminal,
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::CompilerOutput,
        }
    );
    assert!(matches!(
        accepted.body,
        ReplyBody::DependencyUnavailable {
            capability: Capability::CompilerOutput,
        }
    ));
    assert_eq!(
        accepted.diagnostic,
        Some(wave_application_core::Diagnostic {
            code: DiagnosticCode::DependencyUnavailable,
            detail: wave_application_core::DiagnosticDetail::Capability(Capability::CompilerOutput,),
        })
    );

    let rejected = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(12),
        language: text("typescript")?,
        stage: text("lower-ir")?,
        package: text("demo")?,
        source: text("const second = true;")?,
    });
    assert!(matches!(rejected.body, ReplyBody::Rejected));
    assert_eq!(
        rejected.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::UnsupportedCompilerStage)
    );
    assert!(matches!(
        rejected.diagnostic.map(|diagnostic| diagnostic.detail),
        Some(wave_application_core::DiagnosticDetail::Frontend(
            nudox_compile_vocab::FrontendError::UnsupportedStage {
                language: nudox_compile_vocab::Language::TypeScriptSubset,
                stage: nudox_compile_vocab::Stage::LowerIr,
            }
        ))
    ));
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
        }) if retries_remaining == RetryBudget::from(0)
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
fn inconsistent_remote_pin_is_exposed_through_the_application_policy_seam()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    acquire_and_complete(&mut service)?;
    let observed = Pin {
        generation: GenerationId::from_canonical_bytes(b"different-generation"),
        snapshot: IndexSnapshotId::from_canonical_bytes(b"different-snapshot"),
    };

    let retry = service.execute(&ApplicationInput::RecoverInconsistent(
        InconsistentRecovery {
            correlation: CorrelationId(35),
            expected: pin(),
            observed,
            bundle: bundle(),
            budget: budget(1, 1, BatteryState::Normal),
        },
    ));

    assert!(matches!(
        retry.body,
        ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
            cause: RecoveryCause::Inconsistent { observed: returned },
            retries_remaining,
            ..
        }) if returned == observed && retries_remaining == RetryBudget::from(0)
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
fn admitted_execution_wakes_an_in_process_scheduler_without_poll_commands()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(48),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    let ReplyBody::ExecutionStarted { operation, .. } = admitted.body else {
        return Err(ServiceTestError::UnexpectedReply(
            "wake-driven acquisition admission",
        ));
    };

    let wake_flag = Arc::new(WakeFlag::default());
    let waker = Waker::from(Arc::clone(&wake_flag));
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        service.poll_admitted_execution(CorrelationId(48), operation, &mut context),
        Poll::Pending
    ));
    assert!(wake_flag.0.load(Ordering::Acquire));

    let Poll::Ready(completed) =
        service.poll_admitted_execution(CorrelationId(48), operation, &mut context)
    else {
        return Err(ServiceTestError::UnexpectedReply(
            "wake-driven acquisition completion",
        ));
    };
    assert!(matches!(
        completed.body,
        ReplyBody::Execution(ExecutionState::Completed { operation: observed, .. })
            if observed == operation
    ));
    assert_eq!(completed.terminal, Terminal::Complete { emitted: 1 });
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
