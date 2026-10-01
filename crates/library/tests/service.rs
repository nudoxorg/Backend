//! Exercises the `backend-library` tests service contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
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

use backend_semantic::vocabulary::{Language, LanguageProfile, RustEdition, Stage};
use backend_version::observe::{DropNewest, FlightRecorder, Probe};
use backend_library::interface::{
    AdaptiveDisposition, ApplicationDisposition, ApplicationEvent, ApplicationInput,
    ApplicationObservation, ApplicationOutcome, ApplicationService, BatteryState, ByteCount,
    Capability, CapabilityDomain, CapabilityHealth, CapabilityTransition, ContentId, CorrelationId,
    DiagnosticCode, ExecutionReply, GenerateRequest, GenerateTarget, GenerationId,
    InconsistentRecovery, IndexSnapshotId, InputText, InputTextError, OperationBudget,
    OperationKey, Pin, Pressure, RecoveryCause, RejectedSourceText, ReplyBody, ResourceBudget,
    RetryBudget, SourceText,
};

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

fn generate(
    correlation: u64,
    profile: LanguageProfile,
    stage: Stage,
    source: &str,
) -> Result<ApplicationInput, RejectedSourceText> {
    Ok(ApplicationInput::Generate(GenerateRequest {
        target: GenerateTarget {
            correlation: CorrelationId(correlation),
            profile,
            stage,
        },
        source: SourceText::try_from(source.to_owned())?,
    }))
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
    Source(RejectedSourceText),
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
            Self::Source(error) => write!(
                formatter,
                "test source exceeded {} bytes: {}",
                error.error.limit.bytes, error.error.observed
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

impl From<RejectedSourceText> for ServiceTestError {
    fn from(error: RejectedSourceText) -> Self {
        Self::Source(error)
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
    let ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted {
        operation,
        transition: CapabilityTransition::Acquire {
            bundle: selected, ..
        },
    }) = admitted.outcome
    else {
        return Err(ServiceTestError::UnexpectedReply("acquisition admission"));
    };
    assert_eq!(selected, bundle());
    assert_eq!(
        ApplicationDisposition::from(&ReplyBody::ExecutionStarted {
            operation,
            transition: CapabilityTransition::Acquire {
                capability: backend_execution::adaptive::CapabilityKind::Analyzer,
                bundle: selected,
            },
        }),
        ApplicationDisposition::Accepted { operation }
    );

    let pending = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(31),
        operation,
    });
    assert!(matches!(
        pending.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Pending {
                transition: CapabilityTransition::Acquire { bundle: selected, .. },
                ..
            })) if selected == bundle()
    ));
    let completed = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(32),
        operation,
    });
    assert!(matches!(
        completed.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Completed {
                transition: CapabilityTransition::Acquire { bundle: selected, .. },
                ..
            })) if selected == bundle()
    ));
    Ok(operation)
}

#[test]
fn unavailable_compiler_specialization_never_claims_generated_output_and_keeps_rejections()
-> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let accepted_input = generate(
        11,
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        "fn first() {}",
    )?;
    let accepted = service.execute(&accepted_input);
    assert_eq!(
        accepted.outcome,
        ApplicationOutcome::Resolved(ReplyBody::DependencyUnavailable {
            capability: Capability::CompilerOutput,
        })
    );

    let rejected_input = generate(
        12,
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::Parse,
        "const second = true;",
    )?;
    let rejected = service.execute(&rejected_input);
    assert!(matches!(
        rejected.outcome,
        ApplicationOutcome::Failed {
            diagnostic: backend_library::interface::Diagnostic {
                code: DiagnosticCode::UnsupportedCompilerStage,
                detail: backend_library::interface::DiagnosticDetail::Frontend(
                    backend_semantic::vocabulary::FrontendError::UnsupportedStage {
                        language: Language::Rust,
                        stage: Stage::Parse,
                    }
                ),
            },
        }
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
        search.outcome,
        ApplicationOutcome::Resolved(ReplyBody::DependencyUnavailable {
            capability: Capability::Index,
        })
    );
    Ok(())
}

#[test]
fn semantic_text_limit_plus_one_is_retained_but_larger_transport_text_is_rejected()
-> Result<(), ServiceTestError> {
    let limit_plus_one = text("123456789012345678901234567890123")?;
    let mut service = ApplicationService::new();
    let rejected = service.execute(&ApplicationInput::Search {
        correlation: CorrelationId(25),
        snapshot: limit_plus_one,
        query: text("render")?,
        limit: 1,
    });
    assert!(matches!(
        rejected.outcome,
        ApplicationOutcome::Failed {
            diagnostic: backend_library::interface::Diagnostic {
                code: DiagnosticCode::SemanticTextTooLong,
                ..
            },
        }
    ));
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
        fused.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Completed { .. }))
    ));
    let retry = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(34),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    assert!(matches!(
        retry.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
                retries_remaining,
                ..
            })) if retries_remaining == RetryBudget::from(0)
    ));
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
        retry.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Adaptive(AdaptiveDisposition::RetryRemote {
                cause: RecoveryCause::Inconsistent { observed: returned },
                retries_remaining,
                ..
            })) if returned == observed && retries_remaining == RetryBudget::from(0)
    ));
    Ok(())
}

#[test]
fn cancellation_fuses_without_acquiring() -> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let admitted = service.execute(&ApplicationInput::RecoverLocal {
        correlation: CorrelationId(40),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 1, BatteryState::Normal),
    });
    let ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted { operation, .. }) =
        admitted.outcome
    else {
        return Err(ServiceTestError::UnexpectedReply("cancellable admission"));
    };
    let cancelled = service.execute(&ApplicationInput::Cancel {
        correlation: CorrelationId(41),
        operation,
    });
    assert!(matches!(
        cancelled.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Cancelled { .. }))
    ));
    let fused = service.execute(&ApplicationInput::PollExecution {
        correlation: CorrelationId(42),
        operation,
    });
    assert_eq!(cancelled.outcome, fused.outcome);
    let health = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(43),
    });
    let ApplicationOutcome::Resolved(ReplyBody::Health(facts)) = health.outcome else {
        return Err(ServiceTestError::UnexpectedReply(
            "health after cancellation",
        ));
    };
    assert!(facts.contains(&CapabilityHealth::Unavailable(Capability::LocalAnalyzer)));
    Ok(())
}

#[test]
fn critical_contraction_releases_after_completed_acquisition() -> Result<(), ServiceTestError> {
    let mut service = ApplicationService::new();
    let _completed_operation = acquire_and_complete(&mut service)?;
    let release = service.execute(&ApplicationInput::ReleaseLocal {
        correlation: CorrelationId(47),
        pin: pin(),
        bundle: bundle(),
        budget: budget(1, 0, BatteryState::Critical),
    });
    assert!(matches!(
        release.outcome,
        ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted {
            transition: CapabilityTransition::Release { .. },
            ..
        })
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
    let ApplicationOutcome::Resolved(ReplyBody::ExecutionStarted { operation, .. }) =
        admitted.outcome
    else {
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
        completed.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Execution(ExecutionReply::Completed {
            operation: observed, ..
        })) if observed == operation
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
        reply.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Adaptive(AdaptiveDisposition::Overloaded(overload)))
            if matches!(overload.resource, backend_library::interface::ResourceClass::Operations)
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
            outcome: ApplicationObservation::Resolved(ApplicationDisposition::Complete {
                emitted: 0,
            }),
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
