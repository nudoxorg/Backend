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

/// Resolves a closure-bound file without materializing the aggregate.
pub(super) fn witnessed_file_borrowed<'a>(
    claim: &NativeSourceEvidence,
    aggregate: &backend_semantic::native_ts_headers::NativeTsHeaderAggregateView<'a>,
    digest: [u8; 32], checkpoint: &mut impl FnMut(usize) -> Result<(), String>,
) -> Result<backend_semantic::native_ts_headers::NativeTsHeaderFileView<'a>, String> {
    let reference = claim.headers.as_ref().ok_or("native header reference absent")?;
    if reference.aggregate_digest != digest { return Err("native header closure differs".into()); }
    let file = aggregate.find_file_checkpointed(&claim.path, checkpoint)?
        .ok_or("native header file absent from compiler closure")?;
    validate_witnessed_file(claim, digest, file)
}

/// Resolve an already-indexed selected header aggregate without rescanning it.
pub(super) fn witnessed_indexed_file_borrowed<'a>(
    claim: &NativeSourceEvidence,
    index: &backend_semantic::native_ts_headers::NativeTsHeaderFileIndex<'a, '_>,
    digest: [u8; 32], checkpoint: &mut impl FnMut(usize) -> Result<(), String>,
) -> Result<backend_semantic::native_ts_headers::NativeTsHeaderFileView<'a>, String> {
    let reference = claim.headers.as_ref().ok_or("native header reference absent")?;
    if reference.aggregate_digest != digest { return Err("native header closure differs".into()); }
    let file = index.find_file_checkpointed(&claim.path, checkpoint)?
        .ok_or("native header file absent from compiler closure")?;
    validate_witnessed_file(claim, digest, file)
}

fn validate_witnessed_file<'a>(
    claim: &NativeSourceEvidence, digest: [u8; 32],
    file: backend_semantic::native_ts_headers::NativeTsHeaderFileView<'a>,
) -> Result<backend_semantic::native_ts_headers::NativeTsHeaderFileView<'a>, String> {
    let reference = claim.headers.as_ref().ok_or("native header reference absent")?;
    if reference.aggregate_digest != digest { return Err("native header closure differs".into()); }
    if file.source.byte_len != claim.byte_length || file.source.identity.as_ref() != claim.source_digest.as_slice()
        || file.image_identity != reference.image_identity || file.image_byte_len != reference.image_byte_len
        || file.producer_recipe != reference.producer_recipe
    { return Err("native header file differs from source evidence".into()); }
    Ok(file)
}

/// Checks every borrowed claim against the exact selected compiler image.
pub(super) fn admit_image_borrowed(
    claim: &NativeSourceEvidence, headers: &backend_engine::builtin::NativeHeaderEvidence,
    file: backend_semantic::native_ts_headers::NativeTsHeaderFileView<'_>, image: &SemanticImageView<'_>,
    encoded: &[u8], checkpoint: &mut impl FnMut(usize) -> Result<(), String>,
) -> Result<(), String> {
    let mut hasher = backend_version::ArtifactHasher::<backend_version::IrSemanticImageEncoding, backend_version::IrSemanticImageDomain>::new();
    for chunk in encoded.chunks(64 * 1024) { checkpoint(chunk.len())?; hasher.write_chunk(chunk); }
    let identity: [u8; 32] = *hasher.finalize().as_ref();
    if headers.image_identity != identity || headers.image_byte_len != encoded.len() as u64
        || file.path != claim.path || file.source.byte_len != claim.byte_length
        || file.source.identity.as_ref() != claim.source_digest.as_slice()
        || file.image_identity != identity || file.image_byte_len != encoded.len() as u64
        || file.producer_recipe != headers.producer_recipe
    { return Err("native header image or source differs".into()); }
    let session = DocumentationSession::new(image);
    for entry in file.entries() {
        checkpoint(52)?;
        let entity = session.entity(backend_semantic::ir::EntityId::new(entry.entity_id))
            .map_err(|error| format!("native header entity: {error}"))?;
        let Some(span) = entity.entity.source else { return Err("native header source absent".into()) };
        if entity.entity.version.identity() != entry.identity || entity.entity.kind != ItemKind::Function
            || entity.entity.authority.publication != DeclarationPublicationRole::PrimaryOwned
            || entity.entity.authority.source != FactAvailability::Captured
            || entity.entity.authority.source_file != FactAvailability::Captured
            || (span.start(), span.end()) != entry.compiler_extent
            || image.atom(span.file()) != Some(claim.path.as_bytes())
        { return Err("native header entity differs".into()); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::builtin::{NativeHeaderEvidence, NativeSourceEvidenceKind};
    use backend_semantic::ir::{DeclarationFamilyId, DeclarationIdentity, SourceIdentity, VariantFingerprint};
    use backend_semantic::native_ts_headers::{NativeTsHeaderAggregate, NativeTsHeaderEntry, NativeTsHeaderFile};
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    #[test]
    fn closure_pointer_rejects_changed_header_and_missing_file() {
        let source = SourceIdentity::from_bytes(b"export function run() { return 1; }").unwrap();
        let entry = NativeTsHeaderEntry {
            entity_id: 3,
            identity: DeclarationIdentity { family: DeclarationFamilyId::from_raw([1; 16]), variant: VariantFingerprint::from_raw([2; 16]) },
            compiler_extent: (7, source.byte_len), header_extent: (0, 21),
        };
        let aggregate = NativeTsHeaderAggregate { files: vec![NativeTsHeaderFile {
            path: "src/run.ts".into(), source, image_identity: [3; 32], image_byte_len: 100,
            producer_recipe: [4; 32], entries: vec![entry],
        }] };
        let object_for = |value: &NativeTsHeaderAggregate| {
            let bytes = value.encode_checked().unwrap();
            let mut budget = 1024 * 1024;
            backend_extension_turso::compiler_native_headers_object(&bytes, &mut budget).unwrap()
        };
        let object = object_for(&aggregate);
        let row = NativeSourceEvidence {
            project: [5; 32], profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            path: "src/run.ts".into(), input_digest: [6; 32], source_digest: *source.identity.as_ref(),
            byte_length: source.byte_len, kind: NativeSourceEvidenceKind::Image, line_starts: None,
            headers: Some(NativeHeaderEvidence { image_identity: [3; 32], image_byte_len: 100,
                producer_recipe: [4; 32], aggregate_digest: *object.id().as_bytes() }),
        };
        assert!(witnessed_file(&row, &aggregate, *object.id().as_bytes()).is_ok());
        let mut body_as_header = aggregate.clone();
        body_as_header.files[0].entries[0].header_extent.1 = source.byte_len;
        let forged = object_for(&body_as_header);
        assert!(witnessed_file(&row, &body_as_header, *forged.id().as_bytes()).is_err());
        let mut wrapper_changed = aggregate.clone();
        wrapper_changed.files[0].entries[0].header_extent.0 = 1;
        let forged = object_for(&wrapper_changed);
        assert!(witnessed_file(&row, &wrapper_changed, *forged.id().as_bytes()).is_err());
        let missing = NativeTsHeaderAggregate { files: Vec::new() };
        assert!(witnessed_file(&row, &missing, *object.id().as_bytes()).is_err());
        let mut foreign = aggregate.clone();
        foreign.files[0].image_identity[0] ^= 1;
        assert!(witnessed_file(&row, &foreign, *object.id().as_bytes()).is_err());
    }
}
