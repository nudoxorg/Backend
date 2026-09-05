//! Defines json wire compiler authority behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler authority invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_vocabulary::CompileRecipeFact;
use heart_identity::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, CompileRecipeDomain,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, IrSemanticImageDomain, IrSemanticImageEncoding, SourceFactDomain,
    ToolchainDomain,
};
use interface_core::{
    DurableReceiptAuthority, GenerationAuthority, PublicationAuthority, SemanticImageAuthority,
    SourceAuthority,
};
use serde::Serialize;

use super::super::scalar::{
    LanguageProfileWire, NativeToolWire, StageWire, serialize_artifact, serialize_content,
};

/// Remote serde definition for the source authority retained by generated results and terminals.
#[derive(Serialize)]
#[serde(remote = "interface_core::SourceAuthority")]
pub(crate) struct SourceAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    identity: heart_identity::ContentId<SourceFactDomain>,
    byte_len: u32,
}

/// Remote serde definition for the canonical compile recipe.
#[derive(Serialize)]
#[serde(remote = "compiler_vocabulary::CompileRecipeFact")]
pub(crate) struct CompileRecipeWire {
    #[serde(serialize_with = "serialize_content")]
    identity: heart_identity::ContentId<CompileRecipeDomain>,
    #[serde(with = "LanguageProfileWire")]
    profile: compiler_vocabulary::LanguageProfile,
    #[serde(with = "StageWire")]
    stage: compiler_vocabulary::Stage,
    #[serde(with = "NativeToolWire")]
    tool: compiler_vocabulary::NativeTool,
    #[serde(serialize_with = "serialize_content")]
    toolchain: heart_identity::ContentId<ToolchainDomain>,
}

/// Remote serde definition for the complete verified publication authority.
#[derive(Serialize)]
#[serde(remote = "interface_core::PublicationAuthority")]
pub(crate) struct PublicationAuthorityWire {
    #[serde(with = "GenerationAuthorityWire")]
    generation: GenerationAuthority,
    #[serde(serialize_with = "serialize_artifact")]
    manifest: ArtifactId<IrManifestEncoding, IrManifestDomain>,
    #[serde(serialize_with = "serialize_artifact")]
    binding: ArtifactId<CompilePublicationEncoding, CompilePublicationDomain>,
    #[serde(with = "DurableReceiptAuthorityWire")]
    receipt: DurableReceiptAuthority,
}

/// Remote serde definition for exact stable journal and publication-artifact facts.
#[derive(Serialize)]
#[serde(remote = "interface_core::DurableReceiptAuthority")]
struct DurableReceiptAuthorityWire {
    sequence: u64,
    durable_end: u64,
    immutable_checksum: [u8; 16],
    head_checksum: [u8; 16],
}

/// Remote serde definition for the generation closure that was proved before publication.
#[derive(Serialize)]
#[serde(remote = "interface_core::GenerationAuthority")]
pub(crate) struct GenerationAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    pinned_root: heart_identity::GenerationId,
    #[serde(serialize_with = "serialize_content")]
    dep_set: heart_identity::ContentId<DependencySetDomain>,
}

/// Remote serde definition for the compact attempt authority retained by compiler failures.
#[derive(Serialize)]
#[serde(remote = "interface_core::CompilerAttempt")]
pub(crate) struct CompilerAttemptWire {
    #[serde(with = "SourceAuthorityWire")]
    source: SourceAuthority,
    #[serde(serialize_with = "serialize_content")]
    recipe: heart_identity::ContentId<CompileRecipeDomain>,
}

/// Remote serde definition for one complete reopened semantic image.
#[derive(Serialize)]
#[serde(remote = "interface_core::SemanticImageAuthority")]
pub(crate) struct SemanticImageAuthorityWire {
    #[serde(serialize_with = "serialize_artifact")]
    identity: ArtifactId<IrSemanticImageEncoding, IrSemanticImageDomain>,
    byte_len: u32,
}

/// Remote serde definition for the only successful compiler body.
#[derive(Serialize)]
#[serde(remote = "interface_core::GeneratedArtifact")]
pub(crate) struct GeneratedArtifactWire {
    #[serde(with = "SourceAuthorityWire")]
    source: SourceAuthority,
    #[serde(with = "CompileRecipeWire")]
    recipe: CompileRecipeFact,
    #[serde(serialize_with = "serialize_artifact")]
    fragment: ArtifactId<IrFragmentEncoding, IrFragmentDomain>,
    #[serde(with = "SemanticImageAuthorityWire")]
    semantic_image: SemanticImageAuthority,
    #[serde(with = "PublicationAuthorityWire")]
    publication: PublicationAuthority,
}
