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

/// Joins a selected row's pointer to a compiler-closure aggregate file.
pub(super) fn witnessed_file<'a>(
    claim: &NativeSourceEvidence,
    aggregate: &'a backend_semantic::native_ts_headers::NativeTsHeaderAggregate,
    digest: [u8; 32],
) -> Result<&'a backend_semantic::native_ts_headers::NativeTsHeaderFile, String> {
    let reference = claim.headers.as_ref().ok_or("native header reference absent")?;
    if reference.aggregate_digest != digest { return Err("native header closure differs".into()); }
    let index = aggregate.files.binary_search_by(|file| file.path.as_str().cmp(&claim.path))
        .map_err(|_| "native header file absent from compiler closure")?;
    let file = &aggregate.files[index];
    if file.source.byte_len != claim.byte_length || file.source.identity.as_ref() != claim.source_digest.as_slice()
        || file.image_identity != reference.image_identity || file.image_byte_len != reference.image_byte_len
        || file.producer_recipe != reference.producer_recipe
    { return Err("native header file differs from source evidence".into()); }
    Ok(file)
}

/// Revalidates every persisted claim against the exact reopened image. The
/// header producer's own source evidence must already be admitted by the
/// publication; these checks do not manufacture that authority.
pub(super) fn admit_image(
    claim: &NativeSourceEvidence,
    headers: &backend_engine::builtin::NativeHeaderEvidence,
    file: &backend_semantic::native_ts_headers::NativeTsHeaderFile,
    image: &SemanticImageView<'_>,
    encoded: &[u8],
) -> Result<(), String> {
    let identity = *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(encoded).as_ref();
    if headers.image_identity != identity || headers.image_byte_len != encoded.len() as u64
        || file.path != claim.path || file.source.byte_len != claim.byte_length
        || file.source.identity.as_ref() != claim.source_digest.as_slice()
        || file.image_identity != identity || file.image_byte_len != encoded.len() as u64
        || file.producer_recipe != headers.producer_recipe
    { return Err("native header image or source differs".into()); }
    let session = DocumentationSession::new(image);
    for entry in &file.entries {
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
