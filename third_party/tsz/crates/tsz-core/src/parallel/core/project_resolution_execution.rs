// Shared resolution admission with caller-selected polling. Legacy admission
// uses the original validator and a no-op poll; bounded project execution uses
// the same outcome builder with a borrowed stop control and metered path walk.

impl From<ProjectModuleResolutionError> for ProjectProgramCheckError {
    fn from(error: ProjectModuleResolutionError) -> Self {
        Self::ModuleResolution(error)
    }
}

fn validate_project_virtual_path_with_poll<E: From<ProjectModuleResolutionError>>(
    path: &str,
    poll: &mut (impl FnMut(u64) -> std::result::Result<(), E> + ?Sized),
) -> std::result::Result<(), E> {
    poll(0)?;
    let invalid = || ProjectModuleResolutionError::InvalidVirtualPath {
        path: path.to_owned(),
    };
    if path.is_empty() || path.starts_with('/') {
        return Err(invalid().into());
    }
    let mut component_start = 0;
    for (chunk_index, chunk) in path.as_bytes().chunks(64).enumerate() {
        poll(chunk.len() as u64)?;
        for (offset, byte) in chunk.iter().enumerate() {
            let index = chunk_index * 64 + offset;
            if *byte == b'\\' || *byte == 0 {
                return Err(invalid().into());
            }
            if *byte == b'/' {
                let component = &path[component_start..index];
                if component.is_empty() || component == "." || component == ".." {
                    return Err(invalid().into());
                }
                component_start = index + 1;
            }
        }
    }
    let component = &path[component_start..];
    if component.is_empty() || component == "." || component == ".." {
        return Err(invalid().into());
    }
    poll(0)
}

fn build_project_module_resolution_outcomes_with_poll<E: From<ProjectModuleResolutionError>>(
    program: &MergedProgram,
    resolutions: &[ProjectModuleResolution],
    mut poll: impl FnMut(u64) -> std::result::Result<(), E>,
    validate_path: impl Fn(
        &str,
        &mut dyn FnMut(u64) -> std::result::Result<(), E>,
    ) -> std::result::Result<(), E>,
) -> std::result::Result<crate::checker::context::ResolvedModuleRequestOutcomeMap, E> {
    use crate::checker::context::{
        ResolutionRequestKind, ResolvedModuleRequestOutcome, ResolvedModuleRequestOutcomeMap,
    };

    let mut file_indices = FxHashMap::default();
    for (file_idx, file) in program.files.iter().enumerate() {
        validate_path(&file.file_name, &mut poll)?;
        poll(1)?;
        if file_indices
            .insert(file.file_name.as_str(), file_idx)
            .is_some()
        {
            return Err(ProjectModuleResolutionError::DuplicateProgramPath {
                path: file.file_name.clone(),
            }
            .into());
        }
    }

    let mut outcomes = ResolvedModuleRequestOutcomeMap::default();
    let mut reference_importers = FxHashSet::default();
    let mut reference_sites = FxHashMap::default();
    for resolution in resolutions {
        poll(1)?;
        validate_path(&resolution.importer_path, &mut poll)?;
        for chunk in resolution.specifier.as_bytes().chunks(64) {
            poll(chunk.len() as u64)?;
        }
        let importer_idx = *file_indices
            .get(resolution.importer_path.as_str())
            .ok_or_else(|| ProjectModuleResolutionError::ImporterNotInProgram {
                path: resolution.importer_path.clone(),
            })
            .map_err(E::from)?;
        let reference_offset = match resolution.request_kind {
            ProjectModuleRequestKind::ReferencePath { offset }
            | ProjectModuleRequestKind::ReferenceTypes { offset } => Some(offset),
            _ => None,
        };
        if resolution.specifier.is_empty() && reference_offset.is_none() {
            return Err(ProjectModuleResolutionError::EmptySpecifier.into());
        }

        let request_kind = match &resolution.request_kind {
            ProjectModuleRequestKind::EsmImport => ResolutionRequestKind::EsmImport,
            ProjectModuleRequestKind::DynamicImport => ResolutionRequestKind::DynamicImport,
            ProjectModuleRequestKind::CjsRequire => ResolutionRequestKind::CjsRequire,
            ProjectModuleRequestKind::EsmReExport => ResolutionRequestKind::EsmReExport,
            ProjectModuleRequestKind::ReferencePath { offset } => {
                ResolutionRequestKind::ReferencePath { offset: *offset }
            }
            ProjectModuleRequestKind::ReferenceTypes { offset } => {
                ResolutionRequestKind::ReferenceTypes { offset: *offset }
            }
            ProjectModuleRequestKind::Unsupported { syntax_kind } => {
                return Err(ProjectModuleResolutionError::UnsupportedRequestKind {
                    syntax_kind: syntax_kind.clone(),
                }
                .into());
            }
        };
        if let Some(offset) = reference_offset {
            let invalid = || ProjectModuleResolutionError::InvalidReference {
                importer_path: resolution.importer_path.clone(),
                offset,
            };
            if reference_importers.insert(importer_idx) {
                let source = program.files[importer_idx]
                    .arena
                    .source_files
                    .first()
                    .ok_or_else(invalid)
                    .map_err(E::from)?;
                for directive in
                    crate::checker::triple_slash_validator::reference_directives_with_poll(
                        &source.text,
                        &mut poll,
                    )
                {
                    let directive = directive?;
                    let mode = match directive
                        .types
                        .and(directive.resolution_mode)
                        .map(|value| value.text)
                    {
                        None => None,
                        Some("import") => {
                            Some(crate::checker::context::ResolutionModeOverride::Import)
                        }
                        Some("require") => {
                            Some(crate::checker::context::ResolutionModeOverride::Require)
                        }
                        Some(_) => return Err(invalid().into()),
                    };
                    for (value, types) in [(directive.path, false), (directive.types, true)] {
                        if let Some(value) = value {
                            let site_offset =
                                u32::try_from(value.offset).map_err(|_| E::from(invalid()))?;
                            let kind = if types {
                                ResolutionRequestKind::ReferenceTypes {
                                    offset: site_offset,
                                }
                            } else {
                                ResolutionRequestKind::ReferencePath {
                                    offset: site_offset,
                                }
                            };
                            reference_sites.insert(
                                (importer_idx, kind),
                                (value.text, if types { mode } else { None }),
                            );
                        }
                    }
                }
            }
            if reference_sites.get(&(importer_idx, request_kind))
                != Some(&(resolution.specifier.as_str(), resolution.resolution_mode))
                || matches!(
                    resolution.target,
                    ProjectModuleResolutionTarget::External { .. }
                )
            {
                return Err(invalid().into());
            }
        }
        let outcome = match &resolution.target {
            ProjectModuleResolutionTarget::File { path } => {
                validate_path(path, &mut poll)?;
                let target_idx = *file_indices
                    .get(path.as_str())
                    .ok_or_else(|| ProjectModuleResolutionError::TargetNotInProgram {
                        path: path.clone(),
                    })
                    .map_err(E::from)?;
                ResolvedModuleRequestOutcome::File(target_idx)
            }
            ProjectModuleResolutionTarget::External { identity } => {
                for chunk in identity.as_bytes().chunks(64) {
                    poll(chunk.len() as u64)?;
                }
                if identity.trim().is_empty() {
                    return Err(ProjectModuleResolutionError::EmptyExternalIdentity.into());
                }
                ResolvedModuleRequestOutcome::External {
                    identity: identity.clone(),
                }
            }
            ProjectModuleResolutionTarget::Unresolved => ResolvedModuleRequestOutcome::Unresolved,
        };
        let key = (
            importer_idx,
            resolution.specifier.clone(),
            resolution.resolution_mode,
            request_kind,
        );
        if outcomes.insert(key, outcome).is_some() {
            return Err(ProjectModuleResolutionError::DuplicateRequest {
                importer_path: resolution.importer_path.clone(),
                specifier: resolution.specifier.clone(),
            }
            .into());
        }
    }

    poll(0)?;
    Ok(outcomes)
}

impl MergedProgram {
    /// Attach one complete exact outcome map under the caller's shared budget.
    /// A stop or invalid input leaves no resolution witness for publication.
    pub fn set_project_module_resolutions_with_execution_checkpoint(
        &mut self,
        resolutions: &[ProjectModuleResolution],
        checkpoint: &dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<(), ProjectProgramCheckError> {
        self.project_module_resolution_outcomes = None;
        let scope = tsz_common::LatchedExecutionCheckpoint::new(checkpoint);
        let span = tsz_common::ProjectExecutionSpan::new(
            Some(&scope),
            tsz_common::ProjectExecutionPhase::ModuleOutcomes,
            None,
        );
        let outcomes = build_project_module_resolution_outcomes_with_poll(
            self,
            resolutions,
            |work| {
                scope
                    .checkpoint(work)
                    .map_err(ProjectProgramCheckError::ExecutionStopped)
            },
            |path, poll| validate_project_virtual_path_with_poll(path, poll),
        )?;
        scope
            .checkpoint(0)
            .map_err(ProjectProgramCheckError::ExecutionStopped)?;
        self.project_module_resolution_outcomes = Some(Arc::new(outcomes));
        span.returned();
        Ok(())
    }
}
