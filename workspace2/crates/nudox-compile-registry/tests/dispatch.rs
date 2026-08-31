use nudox_compile_registry::{AdapterRoute, FullRegistry};
use nudox_compile_vocab::{FrontendError, Language, NativeTool, Stage};

#[test]
fn closed_lowering_rows_select_one_native_adapter_or_typed_terminal() {
    assert_eq!(
        FullRegistry.route(Language::Rust, Stage::LowerIr),
        Ok(AdapterRoute::Native {
            tool: NativeTool::Rustc,
        })
    );
    assert_eq!(
        FullRegistry.route(Language::Python, Stage::LowerIr),
        Ok(AdapterRoute::Native {
            tool: NativeTool::Python,
        })
    );
    assert_eq!(
        FullRegistry.route(Language::Clang, Stage::LowerIr),
        Ok(AdapterRoute::Native {
            tool: NativeTool::Clang,
        })
    );
    assert_eq!(
        FullRegistry.route(Language::TypeScript, Stage::LowerIr),
        Ok(AdapterRoute::ToolingUnavailable {
            tool: NativeTool::TypeScriptCompiler,
        })
    );
}

#[test]
fn every_language_parse_row_is_an_explicit_unsupported_terminal() {
    for language in Language::ALL {
        assert_eq!(
            FullRegistry.route(language, Stage::Parse),
            Err(FrontendError::UnsupportedStage {
                language,
                stage: Stage::Parse,
            })
        );
    }
}
