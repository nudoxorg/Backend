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
use std::path::Path;

/// One uniquely joined compiler entity and exact source byte ranges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NativeTsHeader {
    pub(super) entity_id: u32,
    pub(super) identity: DeclarationIdentity,
    pub(super) compiler_extent: (u32, u32),
    pub(super) header_extent: (u32, u32),
}

/// The caller must charge the source bytes and this bounded temporary work
/// against the same capture budget as the rest of its source observation.
pub(super) fn capture(
    claim: &NativeSourceEvidence,
    image: &SemanticImageView<'_>,
    bytes: &[u8],
    checkpoint: &dyn TszExecutionCheckpoint,
    remaining_bytes: &mut usize,
) -> Result<Vec<NativeTsHeader>, String> {
    const MAX_HEADERS: usize = 4096;
    const MAX_SOURCE: usize = 256 * 1024;
    if bytes.len() > MAX_SOURCE { return Ok(Vec::new()); }
    // Precharge parser source/arena, row collections and B-tree slack before
    // any allocations. The capture shares this budget across all files.
    let charge = bytes.len().checked_mul(128)
        .and_then(|value| value.checked_add(MAX_HEADERS * 512))
        .ok_or("native header capture byte charge overflow")?;
    *remaining_bytes = remaining_bytes.checked_sub(charge).ok_or("native header capture byte budget exceeded")?;
    checkpoint.checkpoint(0).map_err(|e| format!("native header capture stopped: {e:?}"))?;
    let ImageProvenance::Captured { source, recipe, scope, .. } = image.image_facts().provenance else {
        return Ok(Vec::new());
    };
    if !claim.admits_image() || recipe.tool != NativeTool::TypeScriptCompiler
        || recipe.profile != claim.profile || image.atom(scope.path) != Some(claim.path.as_bytes())
        || source.byte_len != claim.byte_length
        || claim.verify_host(bytes, source.identity).is_err()
    { return Ok(Vec::new()); }
    let text = std::str::from_utf8(bytes).map_err(|_| "native header source is not UTF-8")?;
    let written = backend_frontend_typescript::native_project::function_headers(
        Path::new(&claim.path), text, checkpoint,
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
