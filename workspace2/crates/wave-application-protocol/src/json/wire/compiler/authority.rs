use nudox_compile_vocab::CompileRecipeFact;
use nudox_id::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, CompileRecipeDomain,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, SourceFactDomain, ToolchainDomain,
};
use serde::Serialize;
use wave_application_core::{GenerationAuthority, PublicationAuthority, SourceAuthority};

use super::super::scalar::{
    LanguageWire, NativeToolWire, StageWire, serialize_artifact, serialize_content,
};

/// Remote serde definition for the source authority retained by generated results and terminals.
#[derive(Serialize)]
#[serde(remote = "wave_application_core::SourceAuthority")]
pub(crate) struct SourceAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    identity: nudox_id::ContentId<SourceFactDomain>,
    byte_len: u32,
}

/// Remote serde definition for the canonical compile recipe.
#[derive(Serialize)]
#[serde(remote = "nudox_compile_vocab::CompileRecipeFact")]
pub(crate) struct CompileRecipeWire {
    #[serde(serialize_with = "serialize_content")]
    identity: nudox_id::ContentId<CompileRecipeDomain>,
    #[serde(with = "LanguageWire")]
    language: nudox_compile_vocab::Language,
    #[serde(with = "StageWire")]
    stage: nudox_compile_vocab::Stage,
    #[serde(with = "NativeToolWire")]
    tool: nudox_compile_vocab::NativeTool,
    #[serde(serialize_with = "serialize_content")]
    toolchain: nudox_id::ContentId<ToolchainDomain>,
}

/// Remote serde definition for the complete verified publication authority.
#[derive(Serialize)]
#[serde(remote = "wave_application_core::PublicationAuthority")]
pub(crate) struct PublicationAuthorityWire {
    #[serde(with = "GenerationAuthorityWire")]
    generation: GenerationAuthority,
    #[serde(serialize_with = "serialize_artifact")]
    manifest: ArtifactId<IrManifestEncoding, IrManifestDomain>,
    #[serde(serialize_with = "serialize_artifact")]
    binding: ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>,
}

/// Remote serde definition for the generation closure that was proved before publication.
#[derive(Serialize)]
#[serde(remote = "wave_application_core::GenerationAuthority")]
pub(crate) struct GenerationAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    pinned_root: nudox_id::GenerationId,
    #[serde(serialize_with = "serialize_content")]
    dep_set: nudox_id::ContentId<DependencySetDomain>,
}

/// Remote serde definition for the compact attempt authority retained by compiler failures.
#[derive(Serialize)]
#[serde(remote = "wave_application_core::CompilerAttempt")]
pub(crate) struct CompilerAttemptWire {
    #[serde(with = "SourceAuthorityWire")]
    source: SourceAuthority,
    #[serde(serialize_with = "serialize_content")]
    recipe: nudox_id::ContentId<CompileRecipeDomain>,
}

/// Remote serde definition for the only successful compiler body.
#[derive(Serialize)]
#[serde(remote = "wave_application_core::GeneratedArtifact")]
pub(crate) struct GeneratedArtifactWire {
    #[serde(with = "SourceAuthorityWire")]
    source: SourceAuthority,
    #[serde(with = "CompileRecipeWire")]
    recipe: CompileRecipeFact,
    #[serde(serialize_with = "serialize_artifact")]
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    #[serde(with = "PublicationAuthorityWire")]
    publication: PublicationAuthority,
}
