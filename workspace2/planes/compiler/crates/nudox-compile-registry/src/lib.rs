#![no_std]

use nudox_compile_vocab::{FrontendError, Language, Stage};

struct RustFrontend;
struct TypeScriptFrontend;

trait Frontend {
    fn parse(source: &[u8]) -> &[u8];
    fn lower(source: &[u8]) -> Result<&[u8], FrontendError>;
}

fn drive<ConcreteFrontend: Frontend>(stage: Stage, source: &[u8]) -> Result<&[u8], FrontendError> {
    match stage {
        Stage::Parse => Ok(ConcreteFrontend::parse(source)),
        Stage::LowerIr => ConcreteFrontend::lower(source),
    }
}

impl Frontend for RustFrontend {
    fn parse(source: &[u8]) -> &[u8] {
        source
    }
    fn lower(source: &[u8]) -> Result<&[u8], FrontendError> {
        Ok(source)
    }
}

impl Frontend for TypeScriptFrontend {
    fn parse(source: &[u8]) -> &[u8] {
        source
    }
    fn lower(_: &[u8]) -> Result<&[u8], FrontendError> {
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr,
        })
    }
}

pub struct FullRegistry;

impl FullRegistry {
    pub fn dispatch(
        self,
        language: Language,
        stage: Stage,
        source: &[u8],
    ) -> Result<&[u8], FrontendError> {
        match language {
            Language::RustSubset => drive::<RustFrontend>(stage, source),
            Language::TypeScriptSubset => drive::<TypeScriptFrontend>(stage, source),
        }
    }
}

/// ```compile_fail
/// use nudox_compile_registry::RustSubsetOnly;
/// RustSubsetOnly.parse_typescript(&[]);
/// ```
pub struct RustSubsetOnly;

impl RustSubsetOnly {
    pub fn parse(self, source: &[u8]) -> &[u8] {
        RustFrontend::parse(source)
    }
}
