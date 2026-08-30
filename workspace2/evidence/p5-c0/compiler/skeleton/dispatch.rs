use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{FrontendError, Language, Stage};

fn assert_identity(output: &[u8], source: &[u8]) {
    assert!(core::ptr::eq(output, source));
    assert_eq!(output.len(), source.len());
}

#[test]
fn rust_parse_forwards_its_own_pointer_and_length() -> Result<(), FrontendError> {
    let source = b"fn parse() {}";
    let output = FullRegistry.dispatch(Language::RustSubset, Stage::Parse, source)?;

    assert_identity(output, source);
    Ok(())
}

#[test]
fn rust_lower_forwards_its_own_pointer_and_length() -> Result<(), FrontendError> {
    let source = b"fn lower() {}";
    let output = FullRegistry.dispatch(Language::RustSubset, Stage::LowerIr, source)?;

    assert_identity(output, source);
    Ok(())
}

#[test]
fn typescript_parse_forwards_its_own_pointer_and_length() -> Result<(), FrontendError> {
    let source = b"const parsed = 1;";
    let output = FullRegistry.dispatch(Language::TypeScriptSubset, Stage::Parse, source)?;

    assert_identity(output, source);
    Ok(())
}

#[test]
fn typescript_lower_has_exact_typed_operands() {
    let source = b"const rejected = 1;";
    let result = FullRegistry.dispatch(Language::TypeScriptSubset, Stage::LowerIr, source);

    assert_eq!(
        result,
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr,
        })
    );
}
