#![no_std]

use nudox_compile_vocab::{FrontendError, Language, Stage};

struct RustFrontend;
struct TypeScriptFrontend;
struct RustCapabilities;
struct TypeScriptCapabilities;

trait CapabilityRow {
    fn lower(source: &[u8]) -> Result<&[u8], FrontendError>;
}

trait Frontend {
    type Capabilities: CapabilityRow;

    fn parse(source: &[u8]) -> &[u8];
}

impl CapabilityRow for RustCapabilities {
    fn lower(source: &[u8]) -> Result<&[u8], FrontendError> {
        Ok(source)
    }
}

impl CapabilityRow for TypeScriptCapabilities {
    fn lower(_: &[u8]) -> Result<&[u8], FrontendError> {
        Err(FrontendError::UnsupportedStage {
            language: Language::TypeScriptSubset,
            stage: Stage::LowerIr,
        })
    }
}

impl Frontend for RustFrontend {
    type Capabilities = RustCapabilities;

    fn parse(source: &[u8]) -> &[u8] {
        source
    }
}

impl Frontend for TypeScriptFrontend {
    type Capabilities = TypeScriptCapabilities;

    fn parse(source: &[u8]) -> &[u8] {
        source
    }
}

fn drive<ConcreteFrontend: Frontend>(stage: Stage, source: &[u8]) -> Result<&[u8], FrontendError> {
    match stage {
        Stage::Parse => Ok(ConcreteFrontend::parse(source)),
        Stage::LowerIr => <ConcreteFrontend::Capabilities as CapabilityRow>::lower(source),
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

pub struct TypeScriptSubset;

impl TypeScriptSubset {
    pub fn parse(self, source: &[u8]) -> &[u8] {
        TypeScriptFrontend::parse(source)
    }
}
