use nudox_compile_vocab::{FrontendError, Language, Stage};
use wave_application_core::{
    ApplicationDisposition, ApplicationEvent, ApplicationInput, ApplicationObservation,
    ApplicationOutcome, ApplicationReply, ApplicationService, Capability, CapabilityHealth,
    CorrelationId, DiagnosticCode, DiagnosticDetail, InputText, InputTextError, ReplyBody,
};

fn text(value: &str) -> Result<InputText, InputTextError> {
    InputText::try_from_str(value)
}

#[test]
fn unavailable_compiler_specialization_is_never_generated_truth() -> Result<(), InputTextError> {
    let mut service = ApplicationService::new();

    let generated = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(1),
        language: text("rust")?,
        stage: text("lower-ir")?,
        source: text("pub fn alpha() -> u8 { 7 }")?,
    });

    assert_eq!(
        generated.outcome,
        ApplicationOutcome::Resolved(ReplyBody::DependencyUnavailable {
            capability: Capability::CompilerOutput,
        })
    );

    let health = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(2),
    });
    assert!(matches!(
        health.outcome,
        ApplicationOutcome::Resolved(ReplyBody::Health(facts))
            if ApplicationDisposition::from(&ReplyBody::Health(facts))
                == ApplicationDisposition::Partial {
                    emitted: 1,
                    unavailable: Capability::CompilerOutput,
                }
            && facts.contains(&CapabilityHealth::LocalReady(Capability::CompilerRegistry))
            && facts.contains(&CapabilityHealth::Unavailable(Capability::CompilerOutput))
    ));

    Ok(())
}

#[test]
fn compiler_rejection_preserves_the_exact_typed_cause() -> Result<(), InputTextError> {
    let mut service = ApplicationService::new();
    let rejected = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(3),
        language: text("rust")?,
        stage: text("parse")?,
        source: text("export const broken = 7;")?,
    });

    assert!(matches!(
        rejected.outcome,
        ApplicationOutcome::Failed {
            diagnostic: wave_application_core::Diagnostic {
                code: DiagnosticCode::UnsupportedCompilerStage,
                detail: DiagnosticDetail::Frontend(FrontendError::UnsupportedStage {
                    language: Language::Rust,
                    stage: Stage::Parse,
                }),
            },
        }
    ));

    Ok(())
}

#[test]
fn resolved_outcomes_derive_the_only_possible_observation() {
    let reply = ApplicationReply {
        correlation: CorrelationId(4),
        outcome: ApplicationOutcome::Resolved(ReplyBody::DependencyUnavailable {
            capability: Capability::Graph,
        }),
    };

    assert_eq!(
        ApplicationEvent::from(&reply).outcome,
        ApplicationObservation::Resolved(ApplicationDisposition::Degraded {
            emitted: 0,
            unavailable: Capability::Graph,
        })
    );
}
