//! Canonical compile recipe facts and frontend routing errors.

use backend_version::{CompileRecipeDomain, ContentId, SourceFactDomain, ToolchainDomain};

use super::authority::{Language, NativeTool, Stage};
use super::profile::LanguageProfile;

/// Copyable canonical recipe facts retained by compact compiler artifacts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompileRecipeFact {
    /// Central typed identity over language, stage, source fact, and resolved toolchain fact.
    pub identity: ContentId<CompileRecipeDomain>,
    /// Closed source-language profile bound into `identity`.
    pub profile: LanguageProfile,
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
        profile: LanguageProfile,
        stage: Stage,
        tool: NativeTool,
        source: ContentId<SourceFactDomain>,
        toolchain: ContentId<ToolchainDomain>,
    ) -> Self {
        let mut canonical = [0; 68];
        canonical[..2].copy_from_slice(&<[u8; 2]>::from(profile));
        canonical[2] = u8::from(stage);
        canonical[3] = u8::from(tool);
        canonical[4..36].copy_from_slice(source.as_ref());
        canonical[36..68].copy_from_slice(toolchain.as_ref());
        Self {
            identity: ContentId::<CompileRecipeDomain>::from_canonical_bytes(&canonical),
            profile,
            stage,
            tool,
            toolchain,
        }
    }
}

/// Registry rejection for a language and stage pair with no valid frontend route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontendError {
    /// The language is known, but the requested semantic stage is not implemented for it.
    UnsupportedStage {
        /// Language whose registry row was requested.
        language: Language,
        /// Unsupported stage requested for that language.
        stage: Stage,
    },
}
