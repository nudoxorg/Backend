//! Exercises the `backend-library` tests unified-terminal contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::vocabulary::{FrontendError, Language, LanguageProfile, RustEdition, Stage};
use backend_library::interface::{
    ApplicationDisposition, ApplicationEvent, ApplicationInput, ApplicationObservation,
    ApplicationOutcome, ApplicationReply, ApplicationService, Capability, CapabilityHealth,
    CorrelationId, DiagnosticCode, DiagnosticDetail, GenerateRequest, GenerateTarget,
    RejectedSourceText, ReplyBody, SourceText,
};

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

#[test]
fn unavailable_compiler_specialization_is_never_generated_truth() -> Result<(), RejectedSourceText>
{
    let mut service = ApplicationService::new();

    let input = generate(
        1,
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        "pub fn alpha() -> u8 { 7 }",
    )?;
    let generated = service.execute(&input);

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
fn compiler_rejection_preserves_the_exact_typed_cause() -> Result<(), RejectedSourceText> {
    let mut service = ApplicationService::new();
    let input = generate(
        3,
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::Parse,
        "export const broken = 7;",
    )?;
    let rejected = service.execute(&input);

    assert!(matches!(
        rejected.outcome,
        ApplicationOutcome::Failed {
            diagnostic: backend_library::interface::Diagnostic {
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
