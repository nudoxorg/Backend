#![no_std]

use nudox_compile_vocab::{FrontendError, Language, Stage};

struct RustFrontend;
struct TypeScriptFrontend;

trait Frontend {
    fn parse(source: &[u8]) -> u8;
    fn lower(source: &[u8]) -> Result<u8, FrontendError>;
}

fn drive<ConcreteFrontend: Frontend>(stage: Stage, source: &[u8]) -> Result<u8, FrontendError> {
    match stage {
        Stage::Parse => Ok(ConcreteFrontend::parse(source)),
        Stage::LowerIr => ConcreteFrontend::lower(source),
    }
}

impl Frontend for RustFrontend {
    fn parse(_: &[u8]) -> u8 {
        1
    }
    fn lower(_: &[u8]) -> Result<u8, FrontendError> {
        Ok(1)
    }
}

impl Frontend for TypeScriptFrontend {
    fn parse(_: &[u8]) -> u8 {
        2
    }
    fn lower(_: &[u8]) -> Result<u8, FrontendError> {
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr,
        })
    }
}

pub struct FullRegistry;

impl FullRegistry {
    pub fn dispatch(language: Language, stage: Stage, source: &[u8]) -> Result<u8, FrontendError> {
        match (language, stage) {
            (Language::RustSubset, Stage::Parse) => drive::<RustFrontend>(stage, source),
            (Language::RustSubset, Stage::LowerIr) => drive::<RustFrontend>(stage, source),
            (Language::TypeScriptSubset, Stage::Parse) => {
                drive::<TypeScriptFrontend>(stage, source)
            }
            (Language::TypeScriptSubset, Stage::LowerIr) => {
                drive::<TypeScriptFrontend>(stage, source)
            }
        }
    }
}

/// ```compile_fail
/// use nudox_compile_registry::RustSubsetOnly;
/// RustSubsetOnly::parse_typescript(&[]);
/// ```
pub struct RustSubsetOnly;

impl RustSubsetOnly {
    pub fn parse(source: &[u8]) -> u8 {
        RustFrontend::parse(source)
    }
}
