//! Original-host Vue script syntax projection. This is deliberately separate
//! from compiler membership: the complete SFC is never passed off as TS input.
use super::*;

pub(super) fn scan(
    root: &Path, capability: &ProjectRoot, path: &Path, project: [u8; 32],
    policy: SourceAdmissionPolicy, cancelled: Option<&AtomicBool>,
) -> Result<(ScannedFile, bool), String> {
    check_scan_cancellation(cancelled)?;
    let relative = relative_coordinate(root, path)?;
    let bytes = match capability.read(Path::new(&relative), policy.limits().max_file_source_bytes.min(1024 * 1024)) {
        Ok(bytes) => bytes,
        Err(SourceFault::Unavailable(reason) | SourceFault::UnavailableRead(reason, _)) => return unavailable(path, project, relative, reason, 0),
        Err(SourceFault::Vanished) => return unavailable(path, project, relative, SourceUnavailableReason::Unreadable, 0),
        Err(SourceFault::Fatal(error)) => return Err(error),
    };
    policy.admit_actual_file(bytes.len()).map_err(|error| error.to_string())?;
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let source_identity = SourceFactIdentity::from_canonical_bytes(&bytes);
    let analysis = backend_frontend_vue::support_fingerprint();
    let mut declarations = Vec::new();
    let mut supported = false;
    if let Ok(container) = backend_frontend_vue::parse(&bytes) {
        let not_cancelled = AtomicBool::new(false);
        let budget = backend_frontend_vue::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            cancelled.unwrap_or(&not_cancelled), 5_000_000,
        );
        for block in 0..container.blocks().len() {
            check_scan_cancellation(cancelled)?;
            let parsed = match container.parse_script_with_checkpoint(block, &budget) {
                Ok(parsed) => parsed,
                Err(backend_frontend_vue::ScriptParseError::Stopped) => return Err("Vue script analysis stopped".into()),
                Err(_) => continue,
            };
            supported = true;
            for declaration in parsed.declarations {
                if declarations.len() >= 16_384 { return Err("Vue script declaration bound exceeded".into()) }
                let name = declaration.name.text();
                let owner = declaration.owner.text();
                let range = declaration.owner.byte_range();
                let name_range = declaration.name.byte_range();
                let line = bytes[..name_range.start].iter().filter(|byte| **byte == b'\n').count() + 1;
                let kind = match declaration.kind {
                    backend_frontend_vue::DeclarationKind::Variable | backend_frontend_vue::DeclarationKind::Binding => DeclarationKind::Variable,
                    backend_frontend_vue::DeclarationKind::Function => DeclarationKind::Function,
                    backend_frontend_vue::DeclarationKind::Class => DeclarationKind::Class,
                    backend_frontend_vue::DeclarationKind::Interface => DeclarationKind::Interface,
                    backend_frontend_vue::DeclarationKind::TypeAlias => DeclarationKind::Type,
                    backend_frontend_vue::DeclarationKind::Enum => DeclarationKind::Enum,
                    backend_frontend_vue::DeclarationKind::Import => DeclarationKind::Import,
                };
                let signature = owner.lines().next().unwrap_or(name);
                let signature = prefix(signature, backend_compile::SourceDeclaration::MAX_TEXT_BYTES);
                let binding = backend_compile::SourceBindingCapture::new(
                    u32::try_from(range.start).map_err(|_| "Vue extent overflow")?,
                    u32::try_from(range.end).map_err(|_| "Vue extent overflow")?,
                    u32::try_from(name_range.start).map_err(|_| "Vue name overflow")?,
                    u32::try_from(name_range.end).map_err(|_| "Vue name overflow")?,
                ).ok_or("Vue name is outside declaration extent")?;
                declarations.push(backend_compile::SourceDeclaration::at_path(
                    &relative, name, kind, u32::try_from(line).map_err(|_| "Vue line overflow")?, signature,
                    "Vue script syntax inventory; template, style, module resolution and complete framework semantics are unavailable.",
                )?.with_binding_identity(backend_compile::SourceBindingIdentity::Exact(binding))
                    .with_source_excerpt(SourceExcerpt::capture_bounded(owner)));
            }
        }
    }
    if !supported {
        return unavailable(path, project, relative, SourceUnavailableReason::Unparsed, bytes.len());
    }
    if let Some(name) = path.file_stem().and_then(|s| s.to_str()) {
        declarations.insert(0, backend_compile::SourceDeclaration::at_path(&relative, name,
            DeclarationKind::Module, 1, "Vue single-file component", 
            "Partial native script inventory; template, style and framework module semantics are unavailable.")?
            .with_source_excerpt(SourceExcerpt::capture_bounded(std::str::from_utf8(&bytes).map_err(|e| e.to_string())?)));
    }
    let declarations: Arc<[_]> = declarations.into();
    let facts = build_product_source_file_facts(project, relative.clone(), SourceLanguage::TypeScript,
        content, analysis, source_identity, &declarations)?;
    let record = ProductSourceRecord::profiled_file_within_row_capacity(project, relative.clone(),
        SourceLanguage::TypeScript, content, analysis, declarations, source_identity, None)?;
    let file = finish_scanned_file(relative.clone(), product_source_file_key(project, &relative),
        record, bytes.len(), bytes.len(), path, None, None, None, Some(facts))?;
    check_scan_cancellation(cancelled)?;
    Ok((file, supported))
}

fn prefix(text: &str, maximum: usize) -> &str {
    let mut end = text.len().min(maximum);
    while !text.is_char_boundary(end) { end -= 1; }
    &text[..end]
}

fn unavailable(path: &Path, project: [u8; 32], relative: String, reason: SourceUnavailableReason, read: usize) -> Result<(ScannedFile, bool), String> {
    let record = ProductSourceRecord::profiled_file_unavailable(project, relative.clone(), SourceLanguage::TypeScript,
        backend_frontend_vue::support_fingerprint(), reason, None)?;
    let file = finish_scanned_file(relative.clone(), product_source_file_key(project, &relative), record,
        read, read, path, None, None, None, None)?;
    Ok((file, false))
}
