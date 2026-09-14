//! Defines json wire compiler authority behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler authority invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use backend_semantic::vocabulary::CompileRecipeFact;
use backend_version::{
    ArtifactId, CompilePublicationDomain, CompilePublicationEncoding, CompileRecipeDomain,
    DependencySetDomain, IrFragmentDomain, IrFragmentEncoding, IrManifestDomain,
    IrManifestEncoding, IrSemanticImageDomain, IrSemanticImageEncoding, SourceFactDomain,
    ToolchainDomain,
};
use crate::interface::{
    DurableReceiptAuthority, GenerationAuthority, PublicationAuthority, SemanticImageAuthority,
    SourceAuthority,
};
use serde::Serialize;

use super::super::scalar::{
    LanguageProfileWire, NativeToolWire, StageWire, serialize_artifact, serialize_content,
};

/// Remote serde definition for the source authority retained by generated results and terminals.
#[derive(Serialize)]
#[serde(remote = "crate::interface::SourceAuthority")]
pub(crate) struct SourceAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    identity: backend_version::ContentId<SourceFactDomain>,
    byte_len: u32,
}

/// Remote serde definition for the canonical compile recipe.
#[derive(Serialize)]
#[serde(remote = "backend_semantic::vocabulary::CompileRecipeFact")]
pub(crate) struct CompileRecipeWire {
    #[serde(serialize_with = "serialize_content")]
    identity: backend_version::ContentId<CompileRecipeDomain>,
    #[serde(with = "LanguageProfileWire")]
    profile: backend_semantic::vocabulary::LanguageProfile,
    #[serde(with = "StageWire")]
    stage: backend_semantic::vocabulary::Stage,
    #[serde(with = "NativeToolWire")]
    tool: backend_semantic::vocabulary::NativeTool,
    #[serde(serialize_with = "serialize_content")]
    toolchain: backend_version::ContentId<ToolchainDomain>,
}

/// Remote serde definition for the complete verified publication authority.
#[derive(Serialize)]
#[serde(remote = "crate::interface::PublicationAuthority")]
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
#[serde(remote = "crate::interface::DurableReceiptAuthority")]
struct DurableReceiptAuthorityWire {
    sequence: u64,
    durable_end: u64,
    immutable_checksum: [u8; 16],
    head_checksum: [u8; 16],
}

/// Remote serde definition for the generation closure that was proved before publication.
#[derive(Serialize)]
#[serde(remote = "crate::interface::GenerationAuthority")]
pub(crate) struct GenerationAuthorityWire {
    #[serde(serialize_with = "serialize_content")]
    pinned_root: backend_version::GenerationId,
    #[serde(serialize_with = "serialize_content")]
    dep_set: backend_version::ContentId<DependencySetDomain>,
}

/// Remote serde definition for the compact attempt authority retained by compiler failures.
#[derive(Serialize)]
#[serde(remote = "crate::interface::CompilerAttempt")]
pub(crate) struct CompilerAttemptWire {
    #[serde(with = "SourceAuthorityWire")]
    source: SourceAuthority,
    #[serde(serialize_with = "serialize_content")]
    recipe: backend_version::ContentId<CompileRecipeDomain>,
}

/// Remote serde definition for one complete reopened semantic image.
#[derive(Serialize)]
#[serde(remote = "crate::interface::SemanticImageAuthority")]
pub(crate) struct SemanticImageAuthorityWire {
    #[serde(serialize_with = "serialize_artifact")]
    identity: ArtifactId<IrSemanticImageEncoding, IrSemanticImageDomain>,
    byte_len: u32,
}

/// Remote serde definition for the only successful compiler body.
#[derive(Serialize)]
#[serde(remote = "crate::interface::GeneratedArtifact")]
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
