//! Exhaustive contract tests for static compiler registry dispatch.
//! Every language must resolve to one concrete lowering tool without a dynamic fallback.
//! Unsupported parse requests retain both language and stage in their typed terminal.
use compiler_registry::{AdapterRoute, FullRegistry};
use backend_semantic::vocabulary::{FrontendError, Language, NativeTool, Stage};

#[test]
/// Proves the lowering registry covers the canonical language schedule exactly once.
fn closed_lowering_rows_select_one_native_adapter_or_typed_terminal() {
    let rows = [
        (
            Language::Rust,
            AdapterRoute::Native {
                tool: NativeTool::Rustc,
            },
        ),
        (
            Language::TypeScript,
            AdapterRoute::Native {
                tool: NativeTool::TypeScriptCompiler,
            },
        ),
        (
            Language::Python,
            AdapterRoute::Native {
                tool: NativeTool::Python,
            },
        ),
        (
            Language::Go,
            AdapterRoute::Native {
                tool: NativeTool::GoCompiler,
            },
        ),
        (
            Language::Java,
            AdapterRoute::Native {
                tool: NativeTool::JavaCompiler,
            },
        ),
        (
            Language::CSharp,
            AdapterRoute::Native {
                tool: NativeTool::CSharpCompiler,
            },
        ),
        (
            Language::Clang,
            AdapterRoute::Native {
                tool: NativeTool::Clang,
            },
        ),
    ];
    assert_eq!(rows.map(|(language, _route)| language), Language::ALL);
    for (language, route) in rows {
        assert_eq!(FullRegistry.route(language, Stage::LowerIr), Ok(route));
    }
}

#[test]
/// Proves parse requests fail explicitly for every known language rather than changing stages.
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
