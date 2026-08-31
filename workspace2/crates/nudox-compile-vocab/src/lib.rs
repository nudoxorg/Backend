#![no_std]

use nudox_id::{
    CompileRecipeDomain, ContentId, SourceFactDomain, ToolchainDomain,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

impl From<Language> for u8 {
    fn from(value: Language) -> Self {
        match value {
            Language::Rust => 0,
            Language::TypeScript => 1,
            Language::Python => 2,
            Language::Go => 3,
            Language::Java => 4,
            Language::CSharp => 5,
            Language::Clang => 6,
        }
    }
}

impl TryFrom<u8> for Language {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rust),
            1 => Ok(Self::TypeScript),
            2 => Ok(Self::Python),
            3 => Ok(Self::Go),
            4 => Ok(Self::Java),
            5 => Ok(Self::CSharp),
            6 => Ok(Self::Clang),
            value => Err(value),
        }
    }
}

impl Language {
    /// Canonical language-family order used by compiler schedules and reports.
    pub const ALL: [Self; 7] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::Clang,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    Parse,
    LowerIr,
}

impl From<Stage> for u8 {
    fn from(value: Stage) -> Self {
        match value {
            Stage::Parse => 0,
            Stage::LowerIr => 1,
        }
    }
}

impl TryFrom<u8> for Stage {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Parse),
            1 => Ok(Self::LowerIr),
            value => Err(value),
        }
    }
}

/// Concrete native tool family selected only by the closed compiler registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeTool {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
    CSharpCompiler,
}

impl From<NativeTool> for u8 {
    fn from(value: NativeTool) -> Self {
        match value {
            NativeTool::Rustc => 0,
            NativeTool::Clang => 1,
            NativeTool::Python => 2,
            NativeTool::TypeScriptCompiler => 3,
            NativeTool::GoCompiler => 4,
            NativeTool::JavaCompiler => 5,
            NativeTool::CSharpCompiler => 6,
        }
    }
}

impl TryFrom<u8> for NativeTool {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Rustc),
            1 => Ok(Self::Clang),
            2 => Ok(Self::Python),
            3 => Ok(Self::TypeScriptCompiler),
            4 => Ok(Self::GoCompiler),
            5 => Ok(Self::JavaCompiler),
            6 => Ok(Self::CSharpCompiler),
            value => Err(value),
        }
    }
}

/// Copyable canonical recipe facts retained by compact compiler artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileRecipeFact {
    /// Central typed identity over language, stage, source fact, and resolved toolchain fact.
    pub identity: ContentId<CompileRecipeDomain>,
    /// Closed language family bound into `identity`.
    pub language: Language,
    /// Closed semantic stage bound into `identity`.
    pub stage: Stage,
    /// Concrete tool family bound into `identity`.
    pub tool: NativeTool,
    /// Central resolved-toolchain identity bound into `identity`.
    pub toolchain: ContentId<ToolchainDomain>,
}

impl CompileRecipeFact {
    /// Derives the only canonical recipe identity from all semantic recipe authorities.
    #[must_use]
    pub fn derive(
        language: Language,
        stage: Stage,
        tool: NativeTool,
        source: ContentId<SourceFactDomain>,
        toolchain: ContentId<ToolchainDomain>,
    ) -> Self {
        let mut canonical = [0; 67];
        canonical[0] = u8::from(language);
        canonical[1] = u8::from(stage);
        canonical[2] = u8::from(tool);
        canonical[3..35].copy_from_slice(source.as_ref());
        canonical[35..67].copy_from_slice(toolchain.as_ref());
        Self {
            identity: ContentId::<CompileRecipeDomain>::from_canonical_bytes(&canonical),
            language,
            stage,
            tool,
            toolchain,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendError {
    UnsupportedStage { language: Language, stage: Stage },
}
