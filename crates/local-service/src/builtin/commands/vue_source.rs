//! Exact native Vue syntax extents joined to selected source facts.
use super::super::{BuiltinModelError, IndexedSources};
use backend_library::Row;

pub(super) fn extent(
    sources: &IndexedSources,
    row: &Row,
) -> Result<
    Option<(
        String,
        backend_version::ContentId<backend_version::SourceFactDomain>,
        (u32, u32),
    )>,
    BuiltinModelError,
> {
    let Some(location) = row.source.captured() else {
        return Ok(None);
    };
    if !std::path::Path::new(location.path())
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("vue"))
    {
        return Ok(None);
    }
    let Some(package) = row.package else {
        return Ok(None);
    };
    let Some(record) = sources.files.iter().find_map(|(_, record)| {
        record
            .file_fields()
            .filter(|file| file.project == package.to_bytes() && file.path == location.path())
            .map(|_| record)
    }) else {
        return Ok(None);
    };
    let Some(project) = sources.projects.get(&package.to_bytes()) else {
        return Ok(None);
    };
    let key = backend_engine::product_source_file_key(package.to_bytes(), location.path());
    if project.package != package || project.files.binary_search(&key).is_err() {
        return Ok(None);
    }
    let file = record
        .file_fields()
        .ok_or_else(|| BuiltinModelError("Vue source is not a file".to_owned()))?;
    if file.analysis_version != super::super::ingest::vue_analysis_version() {
        return Ok(None);
    }
    let Some(identity) = file.source_identity else {
        return Ok(None);
    };
    if row.kind == Some(backend_library::DeclarationKind::Module)
        && location.start_line() == 1
        && row.signature.as_deref() == Some("Vue single-file component")
    {
        let Some(project) = sources.projects.get(&package.to_bytes()) else {
            return Ok(None);
        };
        let root = std::path::Path::new(&project.label);
        if !root.is_absolute() {
            return Ok(None);
        }
        let bytes =
            super::super::ingest::read_stylo_selected_source(root, location.path(), 1024 * 1024)
                .map_err(|_| BuiltinModelError("Vue host source unavailable".into()))?;
        if backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes()
            != file.content_version
            || backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                &bytes,
            ) != identity
        {
            return Err(BuiltinModelError(
                "Vue host changed; reindex and retry".into(),
            ));
        }
        return Ok(Some((
            location.path().to_owned(),
            identity,
            (0, bytes.len() as u32),
        )));
    }
    Ok(super::structural_source::admit(sources, row)?
        .map(|source| (source.path, source.identity, source.binding.owner())))
}
