//! Capture-time joins of native TypeScript parser spans to exact image entities.
//!
//! These rows are claims until the publication binds the image and source.
//! The caller owns the shared cancellation, deadline, work and byte budgets.

use backend_engine::application::DocumentationSession;
use backend_engine::builtin::NativeSourceEvidence;
use backend_semantic::ir::{
    DeclarationPublicationRole, FactAvailability, ItemKind, SemanticCoreReader as _, SemanticImageView,
};

/// Names the native parser and selected-entity join recipe.
pub(super) fn producer_recipe() -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-ts-header-spans.v1\0");
    hash.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
    *hash.finalize().as_bytes()
}

/// Revalidates every persisted claim against the exact reopened image. The
/// header producer's own source evidence must already be admitted by the
/// publication; these checks do not manufacture that authority.
pub(super) fn admit_image(
    claim: &NativeSourceEvidence,
    headers: &backend_engine::builtin::NativeHeaderEvidence,
    image: &SemanticImageView<'_>,
    encoded: &[u8],
) -> Result<(), String> {
    let identity = *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(encoded).as_ref();
    if headers.image_identity != identity || headers.image_byte_len != encoded.len() as u64 {
        return Err("native header image differs".into());
    }
    let session = DocumentationSession::new(image);
    for entry in &headers.entries {
        let entity = session.entity(backend_semantic::ir::EntityId::new(entry.entity_id))
            .map_err(|error| format!("native header entity: {error}"))?;
        let Some(span) = entity.entity.source else { return Err("native header source absent".into()) };
        if entity.entity.version.identity() != entry.identity
            || entity.entity.kind != ItemKind::Function
            || entity.entity.authority.publication != DeclarationPublicationRole::PrimaryOwned
            || entity.entity.authority.source != FactAvailability::Captured
            || entity.entity.authority.source_file != FactAvailability::Captured
            || (span.start(), span.end()) != entry.compiler_extent
            || image.atom(span.file()) != Some(claim.path.as_bytes())
        { return Err("native header entity differs".into()); }
    }
    Ok(())
}
