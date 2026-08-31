use std::error::Error;

use wave_application_core::{
    ApplicationInput, ApplicationService, Capability, CapabilityHealth, CorrelationId, InputText,
    ReplyBody, Terminal,
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
fn generation_completes_real_output_and_opens_the_local_index() -> Result<(), Box<dyn Error>> {
    let mut service = ApplicationService::new();

    let generated = service.execute(&ApplicationInput::Generate {
        correlation: CorrelationId(1),
        language: text("rust")?,
        stage: text("lower-ir")?,
        package: text("alpha")?,
        source: text("pub fn alpha() -> u8 { 7 }")?,
    });

    assert_eq!(generated.terminal, Terminal::Complete { emitted: 1 });
    assert_eq!(generated.diagnostic, None);

    let health = service.execute(&ApplicationInput::Health {
        correlation: CorrelationId(2),
    });
    let ReplyBody::Health(facts) = health.body else {
        return Err("health request did not return typed capability facts".into());
    };
    assert!(facts.contains(&CapabilityHealth::LocalReady(
        Capability::CompilerOutput,
    )));
    assert!(facts.contains(&CapabilityHealth::LocalReady(Capability::Index)));

    Ok(())
}
