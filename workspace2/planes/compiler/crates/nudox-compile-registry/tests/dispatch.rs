use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{FrontendError, Language, Stage};

#[test]
fn full_rows_lend_the_source_and_retain_exact_rejection() -> Result<(), FrontendError> {
    let rust_source: &[u8] = b"fn main()";
    let typescript_source: &[u8] = b"let x = 1";

    let rust_parse = FullRegistry.dispatch(Language::RustSubset, Stage::Parse, rust_source)?;
    let rust_lower = FullRegistry.dispatch(Language::RustSubset, Stage::LowerIr, rust_source)?;
    let typescript_parse =
        FullRegistry.dispatch(Language::TypeScriptSubset, Stage::Parse, typescript_source)?;

    assert!(core::ptr::eq(rust_parse, rust_source));
    assert!(core::ptr::eq(rust_lower, rust_source));
    assert!(core::ptr::eq(typescript_parse, typescript_source));
    assert_eq!(
        FullRegistry.dispatch(
            Language::TypeScriptSubset,
            Stage::LowerIr,
            typescript_source
        ),
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr
        })
    );

    Ok(())
}
