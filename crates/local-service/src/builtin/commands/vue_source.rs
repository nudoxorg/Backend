//! Exact native Vue syntax extents joined to selected source facts.
use super::super::{BuiltinModelError, IndexedSources, view_build};
use backend_library::Row;

pub(super) fn extent(sources: &IndexedSources, row: &Row) -> Result<Option<(String, backend_version::ContentId<backend_version::SourceFactDomain>, (u32, u32))>, BuiltinModelError> {
    let Some(location) = row.source.captured() else { return Ok(None) };
    if !std::path::Path::new(location.path()).extension().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case("vue")) { return Ok(None) }
    let Some(package) = row.package else { return Ok(None) };
    let Some(record) = sources.files.iter().find_map(|(_, record)| record.file_fields().filter(|file| file.project == package.to_bytes() && file.path == location.path()).map(|_| record)) else { return Ok(None) };
    let file = record.file_fields().ok_or_else(|| BuiltinModelError("Vue source is not a file".to_owned()))?;
    if file.analysis_version != backend_frontend_vue::support_fingerprint() { return Ok(None) }
    let Some(identity) = file.source_identity else { return Ok(None) };
    let mut found = None;
    for declaration in view_build::admitted_complete_declarations_for_file(sources, record)? {
        if declaration.line() != location.start_line() || row.kind != Some(declaration.kind())
            || row.signature.as_deref() != Some(declaration.signature())
            || row.label.rsplit("::").next() != Some(declaration.name()) { continue }
        let backend_compile::SourceBindingIdentity::Exact(binding) = declaration.binding_identity() else { return Ok(None) };
        if found.is_some() { return Ok(None) }
        found = Some((location.path().to_owned(), identity, binding.owner()));
    }
    Ok(found)
}
