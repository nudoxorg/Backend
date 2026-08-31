//! Exercises the `interface-protocol` tests support compiler authority contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use interface_core::{
    CompilerAttempt, GeneratedArtifact, GenerationAuthority, PublicationAuthority, SourceAuthority,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenGeneratedArtifact {
    pub source: GoldenSourceAuthority,
    pub recipe: GoldenCompileRecipe,
    pub fragment: String,
    pub publication: GoldenPublicationAuthority,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenSourceAuthority {
    pub identity: String,
    pub byte_len: u32,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenCompileRecipe {
    pub identity: String,
    pub language: GoldenLanguage,
    pub stage: GoldenStage,
    pub tool: GoldenNativeTool,
    pub toolchain: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenPublicationAuthority {
    pub generation: GoldenGenerationAuthority,
    pub manifest: String,
    pub binding: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenGenerationAuthority {
    pub pinned_root: String,
    pub dep_set: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLanguage {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum GoldenStage {
    #[serde(rename = "parse")]
    Parse,
    #[serde(rename = "lower-ir")]
    LowerIr,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenNativeTool {
    Rustc,
    Clang,
    Python,
    TypeScriptCompiler,
    GoCompiler,
    JavaCompiler,
    CSharpCompiler,
}

impl From<GeneratedArtifact> for GoldenGeneratedArtifact {
    fn from(artifact: GeneratedArtifact) -> Self {
        Self {
            source: artifact.source.into(),
            recipe: artifact.recipe.into(),
            fragment: artifact.fragment.to_string(),
            publication: artifact.publication.into(),
        }
    }
}

impl From<SourceAuthority> for GoldenSourceAuthority {
    fn from(source: SourceAuthority) -> Self {
        Self {
            identity: source.identity.to_string(),
            byte_len: source.byte_len,
        }
    }
}

impl From<compiler_vocabulary::CompileRecipeFact> for GoldenCompileRecipe {
    fn from(recipe: compiler_vocabulary::CompileRecipeFact) -> Self {
        Self {
            identity: recipe.identity.to_string(),
            language: recipe.language.into(),
            stage: recipe.stage.into(),
            tool: recipe.tool.into(),
            toolchain: recipe.toolchain.to_string(),
        }
    }
}

impl From<PublicationAuthority> for GoldenPublicationAuthority {
    fn from(publication: PublicationAuthority) -> Self {
        Self {
            generation: publication.generation.into(),
            manifest: publication.manifest.to_string(),
            binding: publication.binding.to_string(),
        }
    }
}

impl From<GenerationAuthority> for GoldenGenerationAuthority {
    fn from(generation: GenerationAuthority) -> Self {
        Self {
            pinned_root: generation.pinned_root.to_string(),
            dep_set: generation.dep_set.to_string(),
        }
    }
}

impl From<compiler_vocabulary::Language> for GoldenLanguage {
    fn from(language: compiler_vocabulary::Language) -> Self {
        match language {
            compiler_vocabulary::Language::Rust => Self::Rust,
            compiler_vocabulary::Language::TypeScript => Self::TypeScript,
            compiler_vocabulary::Language::Python => Self::Python,
            compiler_vocabulary::Language::Go => Self::Go,
            compiler_vocabulary::Language::Java => Self::Java,
            compiler_vocabulary::Language::CSharp => Self::CSharp,
            compiler_vocabulary::Language::Clang => Self::Clang,
        }
    }
}

impl From<compiler_vocabulary::Stage> for GoldenStage {
    fn from(stage: compiler_vocabulary::Stage) -> Self {
        match stage {
            compiler_vocabulary::Stage::Parse => Self::Parse,
            compiler_vocabulary::Stage::LowerIr => Self::LowerIr,
        }
    }
}

impl From<compiler_vocabulary::NativeTool> for GoldenNativeTool {
    fn from(tool: compiler_vocabulary::NativeTool) -> Self {
        match tool {
            compiler_vocabulary::NativeTool::Rustc => Self::Rustc,
            compiler_vocabulary::NativeTool::Clang => Self::Clang,
            compiler_vocabulary::NativeTool::Python => Self::Python,
            compiler_vocabulary::NativeTool::TypeScriptCompiler => Self::TypeScriptCompiler,
            compiler_vocabulary::NativeTool::GoCompiler => Self::GoCompiler,
            compiler_vocabulary::NativeTool::JavaCompiler => Self::JavaCompiler,
            compiler_vocabulary::NativeTool::CSharpCompiler => Self::CSharpCompiler,
        }
    }
}

impl From<CompilerAttempt> for super::terminal::GoldenCompilerAttempt {
    fn from(attempted: CompilerAttempt) -> Self {
        Self {
            source: attempted.source.into(),
            recipe: attempted.recipe.to_string(),
        }
    }
}
