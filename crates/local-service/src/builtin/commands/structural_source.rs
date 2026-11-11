//! Source extents from authenticated complete structural declaration facts.
//! This names a syntax-owned source range and grants no compiler authority.
use super::super::{BuiltinModelError, IndexedSources, view_build};

pub(super) struct StructuralSource {
    pub(super) path: String,
    pub(super) identity: backend_version::ContentId<backend_version::SourceFactDomain>,
    pub(super) binding: backend_compile::SourceBindingCapture,
    pub(super) name: String,
}

/// Join a displayed declaration to a unique exact structural capture in the
/// same package, source file, line, kind and signature. Complete-facts admission
/// authenticates the stored capture; the caller must revalidate current bytes.
/// A semantic row can use this range only with an explicit structural scope.
pub(super) fn admit(
    sources: &IndexedSources,
    row: &backend_library::Row,
) -> Result<Option<StructuralSource>, BuiltinModelError> {
    let Some(location) = row.source.captured() else {
        return Ok(None);
    };
    let Some(package) = row.package else {
        return Ok(None);
    };
    let Some(project) = sources.projects.get(&package.to_bytes()) else {
        return Ok(None);
    };
    let key = backend_engine::product_source_file_key(package.to_bytes(), location.path());
    if project.package != package || project.files.binary_search(&key).is_err() {
        return Ok(None);
    }
    let Some(record) = sources.files.get(&key) else {
        return Ok(None);
    };
    let file = record
        .file_fields()
        .ok_or_else(|| BuiltinModelError("structural source record is not a file".to_owned()))?;
    if file.project != package.to_bytes() || file.path != location.path() {
        return Ok(None);
    }
    let Some(identity) = file.source_identity else {
        return Ok(None);
    };
    let mut found = None;
    for declaration in view_build::admitted_complete_declarations_for_file(sources, record)? {
        if declaration.location().path() != file.path
            || declaration.line() != location.start_line()
            || row.kind != Some(declaration.kind())
            || row.signature.as_deref() != Some(declaration.signature())
            || row.label.rsplit("::").next() != Some(declaration.name())
        {
            continue;
        }
        let backend_compile::SourceBindingIdentity::Exact(binding) = declaration.binding_identity()
        else {
            continue;
        };
        if found.is_some() {
            return Ok(None);
        }
        found = Some(StructuralSource {
            path: file.path.to_owned(),
            identity,
            binding,
            name: declaration.name().to_owned(),
        });
    }
    Ok(found)
}
