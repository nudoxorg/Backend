use serde::Deserialize;
use wave_application_core::{
    CompilerAttempt, GeneratedArtifact, GenerationAuthority, PublicationAuthority, SourceAuthority,
};

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenGeneratedArtifact {
    pub source: GoldenSourceAuthority,
    pub recipe: GoldenCompileRecipe,
    pub fragment: String,
    pub publication: GoldenPublicationAuthority,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenSourceAuthority {
    pub identity: String,
    pub byte_len: u32,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenCompileRecipe {
    pub identity: String,
    pub language: GoldenLanguage,
    pub stage: GoldenStage,
    pub tool: GoldenNativeTool,
    pub toolchain: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenPublicationAuthority {
    pub generation: GoldenGenerationAuthority,
    pub manifest: String,
    pub binding: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub struct GoldenGenerationAuthority {
    pub pinned_root: String,
    pub dep_set: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenLanguage {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub enum GoldenStage {
    #[serde(rename = "parse")]
    Parse,
    #[serde(rename = "lower-ir")]
    LowerIr,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoldenNativeTool {
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

impl From<nudox_compile_vocab::CompileRecipeFact> for GoldenCompileRecipe {
    fn from(recipe: nudox_compile_vocab::CompileRecipeFact) -> Self {
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

impl From<nudox_compile_vocab::Language> for GoldenLanguage {
    fn from(language: nudox_compile_vocab::Language) -> Self {
        match language {
            nudox_compile_vocab::Language::Rust => Self::Rust,
            nudox_compile_vocab::Language::TypeScript => Self::TypeScript,
            nudox_compile_vocab::Language::Python => Self::Python,
            nudox_compile_vocab::Language::Go => Self::Go,
            nudox_compile_vocab::Language::Java => Self::Java,
            nudox_compile_vocab::Language::CSharp => Self::CSharp,
            nudox_compile_vocab::Language::Clang => Self::Clang,
        }
    }
}

impl From<nudox_compile_vocab::Stage> for GoldenStage {
    fn from(stage: nudox_compile_vocab::Stage) -> Self {
        match stage {
            nudox_compile_vocab::Stage::Parse => Self::Parse,
            nudox_compile_vocab::Stage::LowerIr => Self::LowerIr,
        }
    }
}

impl From<nudox_compile_vocab::NativeTool> for GoldenNativeTool {
    fn from(tool: nudox_compile_vocab::NativeTool) -> Self {
        match tool {
            nudox_compile_vocab::NativeTool::Rustc => Self::Rustc,
            nudox_compile_vocab::NativeTool::Clang => Self::Clang,
            nudox_compile_vocab::NativeTool::Python => Self::Python,
            nudox_compile_vocab::NativeTool::TypeScriptCompiler => Self::TypeScriptCompiler,
            nudox_compile_vocab::NativeTool::GoCompiler => Self::GoCompiler,
            nudox_compile_vocab::NativeTool::JavaCompiler => Self::JavaCompiler,
            nudox_compile_vocab::NativeTool::CSharpCompiler => Self::CSharpCompiler,
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
