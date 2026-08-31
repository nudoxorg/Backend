use nudox_compile_vocab::{FrontendError, Language, Stage};
use wave_application_core::{
    ApplicationInput, ApplicationService, Capability, CapabilityHealth, CorrelationId,
    DiagnosticCode, DiagnosticDetail, InputText, InputTextError, ReplyBody, Terminal,
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
        generated.terminal,
        Terminal::Degraded {
            emitted: 0,
            unavailable: Capability::CompilerOutput,
        }
    );
    assert!(matches!(
        generated.body,
        ReplyBody::DependencyUnavailable {
            capability: Capability::CompilerOutput,
        }
    ));

    let health = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(2),
    });
    assert!(matches!(
        health.body,
        ReplyBody::Health(facts)
            if facts.contains(&CapabilityHealth::LocalReady(Capability::CompilerRegistry))
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

    assert_eq!(rejected.terminal, Terminal::Failed);
    assert_eq!(
        rejected
            .diagnostic
            .as_ref()
            .map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::UnsupportedCompilerStage)
    );
    assert!(matches!(
        rejected
            .diagnostic
            .as_ref()
            .map(|diagnostic| &diagnostic.detail),
        Some(DiagnosticDetail::Frontend(
            FrontendError::UnsupportedStage {
                language: Language::Rust,
                stage: Stage::Parse,
            }
        ))
    ));

    Ok(())
}
