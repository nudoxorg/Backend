//! Locally bound direct calls inside one original Vue script host.
use super::super::{BuiltinModelError, IndexedSources, ingest, view_build};
use backend_library::{Row, RowId, SurfaceReply, ProductText, ReferenceCoverage, ReferenceRecord, SemanticLinkTarget, SemanticLinkKind, SemanticLinkEvidence, SemanticConfidence, SemanticSourceSpan};

pub(super) fn references(sources: &IndexedSources, view: &backend_engine::ViewRoot, row: &Row, target: &ProductText) -> Result<Option<SurfaceReply>, BuiltinModelError> {
    let Some((path, identity, _)) = super::vue_source::extent(sources, row)? else { return Ok(None) };
    let Some(package) = row.package else { return Ok(None) };
    let Some(project) = sources.projects.get(&package.to_bytes()) else { return Ok(None) };
    let root = std::path::Path::new(&project.label);
    if !root.is_absolute() { return Ok(None) }
    let record = sources.files.iter().find_map(|(_, record)| record.file_fields().filter(|file| file.project == package.to_bytes() && file.path == path).map(|_| record)).ok_or_else(|| BuiltinModelError("Vue source missing".into()))?;
    let file = record.file_fields().ok_or_else(|| BuiltinModelError("Vue source missing".into()))?;
    let bytes = ingest::read_stylo_selected_source(root, &path, 1024 * 1024).map_err(|_| BuiltinModelError("Vue source unavailable; reindex and retry".into()))?;
    if backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes() != file.content_version
        || backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(&bytes) != identity {
        return Err(BuiltinModelError("Vue source changed; reindex and retry".into()));
    }
    let declarations = view_build::admitted_complete_declarations_for_file(sources, record)?;
    let mut selected = declarations.iter().filter(|declaration| row.source.captured().is_some_and(|location| location.start_line() == declaration.line()) && row.kind == Some(declaration.kind()) && row.signature.as_deref() == Some(declaration.signature()) && row.label.rsplit("::").next() == Some(declaration.name()));
    let Some(selected_declaration) = selected.next() else { return Ok(None) };
    if selected.next().is_some() { return Ok(None) }
    let backend_compile::SourceBindingIdentity::Exact(binding) = selected_declaration.binding_identity() else { return Ok(None) };
    let name_range = binding.name();
    let container = backend_frontend_vue::parse(&bytes).map_err(|_| BuiltinModelError("Vue host no longer parses".into()))?;
    let RowId::Symbol(symbol) = row.id else { return Ok(None) };
    let mut references = Vec::new();
    for block in 0..container.blocks().len() {
        let parsed = match container.parse_script(block) { Ok(parsed) => parsed, Err(_) => continue };
        for call in parsed.calls {
            let target_range = call.target.byte_range();
            if (target_range.start as u32, target_range.end as u32) != name_range { continue }
            let span = call.site.byte_range();
            let line = bytes[..span.start].iter().filter(|byte| **byte == b'\n').count() + 1;
            let owner = parsed.declarations.iter().filter(|declaration| {
                let owner = declaration.owner.byte_range(); owner.start <= span.start && span.end <= owner.end
            }).min_by_key(|declaration| declaration.owner.byte_range().len());
            let site = owner.and_then(|declaration| {
                let owner_line = bytes[..declaration.name.byte_range().start].iter().filter(|byte| **byte == b'\n').count() + 1;
                let coordinate = format!("{}::{path}:{owner_line}::{}", project.label, declaration.name.text());
                view.row_by_label(&coordinate).map(|r| r.label.clone())
            }).unwrap_or_else(|| format!("{}::{path}:1::{}", project.label, std::path::Path::new(&path).file_stem().and_then(|s| s.to_str()).unwrap_or("component")));
            references.push(ReferenceRecord {
                site_symbol: None, source_policy: backend_library::SourcePolicyQualification::Unknown,
                site: ProductText::new(site).map_err(|e| BuiltinModelError(e.to_string()))?,
                target: SemanticLinkTarget::Local { declaration: view_build::structural_symbol_identity(symbol) },
                relation: SemanticLinkKind::Calls,
                evidence: SemanticLinkEvidence { confidence: SemanticConfidence::Syntactic,
                    source: Some(SemanticSourceSpan { file: ProductText::new(&path).map_err(|e| BuiltinModelError(e.to_string()))?, start: span.start as u32, end: span.end as u32 }) },
            });
            let _ = line;
            if references.len() >= backend_library::MAX_PRODUCT_ROWS { return Err(BuiltinModelError("Vue references exceed the bounded result contract".into())) }
        }
    }
    // Reopen through the confined reader to exclude a source replacement while parsing.
    let after = ingest::read_stylo_selected_source(root, &path, 1024 * 1024).map_err(|_| BuiltinModelError("Vue source changed during reference read".into()))?;
    if after != bytes { return Err(BuiltinModelError("Vue source changed during reference read".into())) }
    let reason = ProductText::from_static("Partial Vue script coverage: only native locally bound direct calls in this host; template uses, module imports, cross-block scope and complete framework semantics are unavailable.");
    let coverage = if references.is_empty() { ReferenceCoverage::Unknown { reason } } else { ReferenceCoverage::Partial { reason } };
    Ok(Some(SurfaceReply::References { target: target.clone(), references: references.into_boxed_slice(), coverage }))
}
