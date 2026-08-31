use std::error::Error;

use nudox_compile_vocab::{FrontendError, Language, Stage};
use wave_application_core::{
    ApplicationInput, ApplicationService, Capability, CapabilityHealth, CorrelationId,
    DiagnosticCode, DiagnosticDetail, InputText, ReplyBody, Terminal,
};

fn text(value: &str) -> Result<InputText, Box<dyn Error>> {
    InputText::try_from_str(value).map_err(|error| {
        format!(
            "test input has {} bytes but the transport accepts {}",
            error.actual, error.maximum
        )
        .into()
    })
}

#[test]
fn registry_echo_is_not_generated_compiler_truth() -> Result<(), Box<dyn Error>> {
    let mut service = ApplicationService::new();

    let generated = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(1),
        language: text("rust")?,
        stage: text("lower-ir")?,
        package: text("alpha")?,
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
    let ReplyBody::Health(facts) = health.body else {
        return Err("health request did not return typed capability facts".into());
    };
    assert!(facts.contains(&CapabilityHealth::LocalReady(Capability::CompilerRegistry,)));
    assert!(facts.contains(&CapabilityHealth::Unavailable(Capability::CompilerOutput,)));

    Ok(())
}

#[test]
fn compiler_rejection_preserves_the_exact_typed_cause() -> Result<(), Box<dyn Error>> {
    let mut service = ApplicationService::new();
    let rejected = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(3),
        language: text("typescript")?,
        stage: text("lower-ir")?,
        package: text("broken")?,
        source: text("export const broken = 7;")?,
    });

    assert_eq!(rejected.terminal, Terminal::Failed);
    assert_eq!(
        rejected.diagnostic.map(|diagnostic| diagnostic.code),
        Some(DiagnosticCode::UnsupportedCompilerStage)
    );
    assert!(matches!(
        rejected.diagnostic.map(|diagnostic| diagnostic.detail),
        Some(DiagnosticDetail::Frontend(
            FrontendError::UnsupportedStage {
                language: Language::TypeScriptSubset,
                stage: Stage::LowerIr,
            }
        ))
    ));

    Ok(())
}
