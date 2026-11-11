//! Capture-time joins of native TypeScript parser spans to exact image entities.
//!
//! These rows are claims until the publication binds the image and source.
//! The caller owns the shared cancellation, deadline, work and byte budgets.

use backend_engine::application::DocumentationSession;
use backend_engine::builtin::NativeSourceEvidence;
use backend_frontend_typescript::TszExecutionCheckpoint;
use backend_semantic::ir::{
    DeclarationIdentity, DeclarationPublicationRole, FactAvailability, ImageProvenance,
    ItemKind, SemanticCoreReader as _, SemanticImageView,
};
use backend_semantic::vocabulary::NativeTool;
use std::collections::BTreeMap;

/// Names the native parser and selected-entity join recipe.
pub(super) fn producer_recipe() -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-ts-header-spans.v1\0");
    hash.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
    *hash.finalize().as_bytes()
}

/// One uniquely joined compiler entity and exact source byte ranges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeTsHeader {
    pub(super) entity_id: u32,
    pub(super) identity: DeclarationIdentity,
    pub(super) compiler_extent: (u32, u32),
    pub(super) header_extent: (u32, u32),
}

/// Revalidates every persisted claim against the exact reopened image. The
/// header producer's own source evidence must already be admitted by the
/// publication; these checks do not manufacture that authority.
pub(super) fn admit_image(
    claim: &NativeSourceEvidence,
    image: &SemanticImageView<'_>,
    encoded: &[u8],
) -> Result<(), String> {
    let Some(headers) = &claim.headers else { return Ok(()) };
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

/// The caller must charge the source bytes and this bounded temporary work
/// against the same capture budget as the rest of its source observation.
pub(super) fn capture(
    claim: &NativeSourceEvidence,
    image: &SemanticImageView<'_>,
    bytes: &[u8],
    arena: &backend_frontend_typescript::TszNodeArena,
    checkpoint: &dyn TszExecutionCheckpoint,
    remaining_bytes: &mut usize,
) -> Result<Vec<NativeTsHeader>, String> {
    const MAX_HEADERS: usize = 4096;
    const MAX_SOURCE: usize = 256 * 1024;
    checkpoint.checkpoint(0).map_err(|e| format!("native header capture stopped: {e:?}"))?;
    if bytes.len() > MAX_SOURCE { return Ok(Vec::new()); }
    // The compiler already owns the arena and source. Precharge compact
    // capture rows, result rows, and B-tree node/slack before allocation.
    let charge = MAX_HEADERS.checked_mul(512)
        .ok_or("native header capture byte charge overflow")?;
    let Some(after) = remaining_bytes.checked_sub(charge) else { return Ok(Vec::new()) };
    *remaining_bytes = after;
    let ImageProvenance::Captured { source, recipe, scope, .. } = image.image_facts().provenance else {
        return Ok(Vec::new());
    };
    if !claim.admits_image() || recipe.tool != NativeTool::TypeScriptCompiler
        || recipe.profile != claim.profile || image.atom(scope.path) != Some(claim.path.as_bytes())
        || source.byte_len != claim.byte_length
        || claim.verify_host(bytes, source.identity).is_err()
    { return Ok(Vec::new()); }
    let text = std::str::from_utf8(bytes).map_err(|_| "native header source is not UTF-8")?;
    let written = backend_frontend_typescript::native_project::function_headers_from_arena(
        arena, text, checkpoint,
    )?;
    let session = DocumentationSession::new(image);
    // Poison equal compiler extent/name slots; they cannot prove one owner.
    let mut slots = BTreeMap::<(u32, u32, &str), Option<(u32, DeclarationIdentity)>>::new();
    for item in session.publication_entities() {
        checkpoint.checkpoint(1).map_err(|e| format!("native header capture stopped: {e:?}"))?;
        let item = item.map_err(|e| format!("native header entity: {e}"))?;
        if item.entity.kind != ItemKind::Function
            || item.entity.authority.publication != DeclarationPublicationRole::PrimaryOwned
            || item.entity.authority.source != FactAvailability::Captured
            || item.entity.authority.source_file != FactAvailability::Captured
        { continue; }
        let Some(span) = item.entity.source.filter(|span| !span.is_empty()) else { continue };
        if image.atom(span.file()) != Some(claim.path.as_bytes()) { continue; }
        let Some(name) = item.name.named_bytes().and_then(|name| std::str::from_utf8(name).ok()) else { continue };
        if slots.len() >= MAX_HEADERS { return Ok(Vec::new()); }
        let identity = item.entity.version.identity();
        let entity_id = item.entity.id.raw;
        slots.entry((span.start(), span.end(), name))
            .and_modify(|slot| *slot = None).or_insert(Some((entity_id, identity)));
    }
    let mut result = Vec::new();
    for ((start, end, name), identity) in slots {
        checkpoint.checkpoint(1).map_err(|e| format!("native header capture stopped: {e:?}"))?;
        let Some((entity_id, identity)) = identity else { continue };
        let Some(header_extent) = backend_frontend_typescript::native_project::selected_function_header_range(
            &written, text, (start, end), name, checkpoint,
        )? else { continue };
        result.push(NativeTsHeader { entity_id, identity, compiler_extent: (start, end), header_extent });
    }
    result.sort_unstable_by_key(|entry| entry.entity_id);
    if result.windows(2).any(|pair| pair[0].entity_id == pair[1].entity_id) { return Ok(Vec::new()); }
    checkpoint.checkpoint(0).map_err(|e| format!("native header capture stopped: {e:?}"))?;
    Ok(result)
}
