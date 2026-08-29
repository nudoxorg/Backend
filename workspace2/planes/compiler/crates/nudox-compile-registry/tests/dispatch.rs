use nudox_compile_registry::FullRegistry;
use nudox_compile_vocab::{FrontendError, Language, Stage};

#[test]
fn full_rows_have_distinct_results() {
    assert_eq!(
        FullRegistry::dispatch(Language::RustSubset, Stage::Parse, b"fn main()"),
        Ok(1)
    );
    assert_eq!(
        FullRegistry::dispatch(Language::RustSubset, Stage::LowerIr, b"fn main()"),
        Ok(1)
    );
    assert_eq!(
        FullRegistry::dispatch(Language::TypeScriptSubset, Stage::Parse, b"let x = 1"),
        Ok(2)
    );
    assert_eq!(
        FullRegistry::dispatch(Language::TypeScriptSubset, Stage::LowerIr, b"let x = 1"),
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr
        })
    );
}
