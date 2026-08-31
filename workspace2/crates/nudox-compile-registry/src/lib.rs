#![no_std]

use nudox_compile_vocab::{FrontendError, Language, NativeTool, Stage};

/// Closed native-adapter disposition for one language and semantic stage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdapterRoute {
    /// The caller must invoke this concrete, already selected native adapter.
    Native { tool: NativeTool },
    /// The selected adapter is intentionally unavailable in this build or host recipe.
    ToolingUnavailable { tool: NativeTool },
}

/// The single static language/stage dispatch boundary for compilation.
pub struct FullRegistry;

impl FullRegistry {
    /// Resolves one closed language and stage row without parsing or lending synthetic source.
    pub const fn route(
        self,
        language: Language,
        stage: Stage,
    ) -> Result<AdapterRoute, FrontendError> {
        match stage {
            Stage::Parse => Err(FrontendError::UnsupportedStage { language, stage }),
            Stage::LowerIr => Ok(match language {
                Language::Rust => AdapterRoute::Native {
                    tool: NativeTool::Rustc,
                },
                Language::Python => AdapterRoute::Native {
                    tool: NativeTool::Python,
                },
                Language::Clang => AdapterRoute::Native {
                    tool: NativeTool::Clang,
                },
                Language::TypeScript => AdapterRoute::Native {
                    tool: NativeTool::TypeScriptCompiler,
                },
                Language::Go => AdapterRoute::Native {
                    tool: NativeTool::GoCompiler,
                },
                Language::Java => AdapterRoute::Native {
                    tool: NativeTool::JavaCompiler,
                },
                Language::CSharp => AdapterRoute::Native {
                    tool: NativeTool::CSharpCompiler,
                },
            }),
        }
    }
}
