//! Exercises the `interface-protocol` tests support compiler authority contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use interface_core::{
    CompilerAttempt, GeneratedArtifact, GenerationAuthority, PublicationAuthority,
    SemanticImageAuthority, SourceAuthority,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenGeneratedArtifact {
    pub source: GoldenSourceAuthority,
    pub recipe: GoldenCompileRecipe,
    pub fragment: String,
    pub semantic_image: GoldenSemanticImageAuthority,
    pub publication: GoldenPublicationAuthority,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenSemanticImageAuthority {
    pub identity: String,
    pub byte_len: u32,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenSourceAuthority {
    pub identity: String,
    pub byte_len: u32,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenCompileRecipe {
    pub identity: String,
    pub profile: GoldenLanguageProfile,
    pub stage: GoldenStage,
    pub tool: GoldenNativeTool,
    pub toolchain: String,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenLanguageProfile {
    Rust(GoldenRustEdition),
    TypeScript(GoldenTypeScriptSource),
    Python(GoldenPythonVersion),
    Go(GoldenGoVersion),
    Java(GoldenJavaRelease),
    CSharp(GoldenCSharpVersion),
    C(GoldenCStandard),
    Cxx(GoldenCxxStandard),
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenRustEdition {
    Rust2015,
    Rust2018,
    Rust2021,
    Rust2024,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenTypeScriptSource {
    TypeScript,
    Tsx,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPythonVersion {
    Python310,
    Python311,
    Python312,
    Python313,
    Python314,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenGoVersion {
    Go122,
    Go123,
    Go124,
    Go125,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenJavaRelease {
    Java8,
    Java11,
    Java17,
    Java21,
    Java25,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCSharpVersion {
    CSharp10,
    CSharp11,
    CSharp12,
    CSharp13,
    CSharp14,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCStandard {
    C11,
    C17,
    C23,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenCxxStandard {
    Cxx17,
    Cxx20,
    Cxx23,
    Cxx26,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenPublicationAuthority {
    pub generation: GoldenGenerationAuthority,
    pub manifest: String,
    pub binding: String,
    pub receipt: GoldenDurableReceiptAuthority,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
pub(crate) struct GoldenDurableReceiptAuthority {
    pub sequence: u64,
    pub durable_end: u64,
    pub immutable_checksum: [u8; 16],
    pub head_checksum: [u8; 16],
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
            semantic_image: artifact.semantic_image.into(),
            publication: artifact.publication.into(),
        }
    }
}

impl From<SemanticImageAuthority> for GoldenSemanticImageAuthority {
    fn from(image: SemanticImageAuthority) -> Self {
        Self {
            identity: image.identity.to_string(),
            byte_len: image.byte_len,
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
            profile: recipe.profile.into(),
            stage: recipe.stage.into(),
            tool: recipe.tool.into(),
            toolchain: recipe.toolchain.to_string(),
        }
    }
}

impl From<compiler_vocabulary::LanguageProfile> for GoldenLanguageProfile {
    fn from(profile: compiler_vocabulary::LanguageProfile) -> Self {
        use compiler_vocabulary::LanguageProfile;
        match profile {
            LanguageProfile::Rust(value) => Self::Rust(match value {
                compiler_vocabulary::RustEdition::Rust2015 => GoldenRustEdition::Rust2015,
                compiler_vocabulary::RustEdition::Rust2018 => GoldenRustEdition::Rust2018,
                compiler_vocabulary::RustEdition::Rust2021 => GoldenRustEdition::Rust2021,
                compiler_vocabulary::RustEdition::Rust2024 => GoldenRustEdition::Rust2024,
            }),
            LanguageProfile::TypeScript(value) => Self::TypeScript(match value {
                compiler_vocabulary::TypeScriptSource::TypeScript => {
                    GoldenTypeScriptSource::TypeScript
                }
                compiler_vocabulary::TypeScriptSource::Tsx => GoldenTypeScriptSource::Tsx,
            }),
            LanguageProfile::Python(value) => Self::Python(match value {
                compiler_vocabulary::PythonVersion::Python310 => GoldenPythonVersion::Python310,
                compiler_vocabulary::PythonVersion::Python311 => GoldenPythonVersion::Python311,
                compiler_vocabulary::PythonVersion::Python312 => GoldenPythonVersion::Python312,
                compiler_vocabulary::PythonVersion::Python313 => GoldenPythonVersion::Python313,
                compiler_vocabulary::PythonVersion::Python314 => GoldenPythonVersion::Python314,
            }),
            LanguageProfile::Go(value) => Self::Go(match value {
                compiler_vocabulary::GoVersion::Go122 => GoldenGoVersion::Go122,
                compiler_vocabulary::GoVersion::Go123 => GoldenGoVersion::Go123,
                compiler_vocabulary::GoVersion::Go124 => GoldenGoVersion::Go124,
                compiler_vocabulary::GoVersion::Go125 => GoldenGoVersion::Go125,
            }),
            LanguageProfile::Java(value) => Self::Java(match value {
                compiler_vocabulary::JavaRelease::Java8 => GoldenJavaRelease::Java8,
                compiler_vocabulary::JavaRelease::Java11 => GoldenJavaRelease::Java11,
                compiler_vocabulary::JavaRelease::Java17 => GoldenJavaRelease::Java17,
                compiler_vocabulary::JavaRelease::Java21 => GoldenJavaRelease::Java21,
                compiler_vocabulary::JavaRelease::Java25 => GoldenJavaRelease::Java25,
            }),
            LanguageProfile::CSharp(value) => Self::CSharp(match value {
                compiler_vocabulary::CSharpVersion::CSharp10 => GoldenCSharpVersion::CSharp10,
                compiler_vocabulary::CSharpVersion::CSharp11 => GoldenCSharpVersion::CSharp11,
                compiler_vocabulary::CSharpVersion::CSharp12 => GoldenCSharpVersion::CSharp12,
                compiler_vocabulary::CSharpVersion::CSharp13 => GoldenCSharpVersion::CSharp13,
                compiler_vocabulary::CSharpVersion::CSharp14 => GoldenCSharpVersion::CSharp14,
            }),
            LanguageProfile::C(value) => Self::C(match value {
                compiler_vocabulary::CStandard::C11 => GoldenCStandard::C11,
                compiler_vocabulary::CStandard::C17 => GoldenCStandard::C17,
                compiler_vocabulary::CStandard::C23 => GoldenCStandard::C23,
            }),
            LanguageProfile::Cxx(value) => Self::Cxx(match value {
                compiler_vocabulary::CxxStandard::Cxx17 => GoldenCxxStandard::Cxx17,
                compiler_vocabulary::CxxStandard::Cxx20 => GoldenCxxStandard::Cxx20,
                compiler_vocabulary::CxxStandard::Cxx23 => GoldenCxxStandard::Cxx23,
                compiler_vocabulary::CxxStandard::Cxx26 => GoldenCxxStandard::Cxx26,
            }),
        }
    }
}

impl From<PublicationAuthority> for GoldenPublicationAuthority {
    fn from(publication: PublicationAuthority) -> Self {
        Self {
            generation: publication.generation.into(),
            manifest: publication.manifest.to_string(),
            binding: publication.binding.to_string(),
            receipt: GoldenDurableReceiptAuthority {
                sequence: publication.receipt.sequence,
                durable_end: publication.receipt.durable_end,
                immutable_checksum: publication.receipt.immutable_checksum,
                head_checksum: publication.receipt.head_checksum,
            },
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
