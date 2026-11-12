//! Exclusive native TypeScript authority retained by one compiler worker lane.
//!
//! The host re-admits the complete forest and source closure on every request.
//! Only that freshly admitted forest may borrow these authorities; this cache
//! never discovers inputs, admits membership, or supplies a closure witness.
//! One package root is resident at a time. Removed nodes and solutions without
//! retained native diagnostics are evicted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use backend_frontend_typescript::{TszProject, TszProjectAuthority, TszUpdateReport};

/// Stable node identity within a package root. The full semantic identity is
/// checked by `TszProjectAuthority::update_with_checker_execution`, including
/// options, library bytes, module outcomes, file formats and source digests.
pub(super) type TypeScriptProgramKey = Option<String>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct TypeScriptProgramWork {
    pub parsed_and_bound: u64,
    pub reused_binds: u64,
    pub checked_programs: u64,
    pub reused_checked_programs: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TypeScriptProgramLaneError {
    DuplicateProgramIdentity,
}

#[derive(Default)]
pub(super) struct TypeScriptProgramAuthorityLane {
    root: Option<PathBuf>,
    authorities: BTreeMap<TypeScriptProgramKey, TszProjectAuthority>,
    work: TypeScriptProgramWork,
}

impl TypeScriptProgramAuthorityLane {
    /// Retain at most the current admitted forest. Exact root equality is
    /// deliberately conservative; no filesystem canonicalization or ambient
    /// access can widen reuse to another package.
    pub(super) fn prepare(
        &mut self,
        root: &Path,
        keys: &[TypeScriptProgramKey],
    ) -> Result<(), TypeScriptProgramLaneError> {
        let retained = keys.iter().collect::<BTreeSet<_>>();
        if retained.len() != keys.len() {
            // Two real programs cannot borrow the same mutable authority.
            self.authorities.clear();
            return Err(TypeScriptProgramLaneError::DuplicateProgramIdentity);
        }
        if self.root.as_deref() != Some(root) {
            self.authorities.clear();
            self.root = Some(root.to_path_buf());
        }
        self.authorities.retain(|key, _| retained.contains(key));
        Ok(())
    }

    pub(super) fn authority(&mut self, key: TypeScriptProgramKey) -> &mut TszProjectAuthority {
        self.authorities.entry(key).or_default()
    }

    pub(super) fn project(&self, key: &TypeScriptProgramKey) -> Option<&TszProject> {
        self.authorities
            .get(key)
            .and_then(TszProjectAuthority::project)
    }

    pub(super) fn record(&mut self, report: TszUpdateReport) {
        self.work.parsed_and_bound = self
            .work
            .parsed_and_bound
            .saturating_add(report.parsed_and_bound as u64);
        self.work.reused_binds = self
            .work
            .reused_binds
            .saturating_add(report.reused_binds as u64);
        if report.reused_project_result {
            self.work.reused_checked_programs = self.work.reused_checked_programs.saturating_add(1);
        } else {
            self.work.checked_programs = self.work.checked_programs.saturating_add(1);
        }
    }

    #[cfg(test)]
    pub(super) fn work(&self) -> TypeScriptProgramWork {
        self.work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_frontend_typescript::{
        TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszProjectOptions,
        TszProjectSemanticOptions,
    };

    fn recipe() -> TszProjectOptions {
        TszProjectOptions {
            checker: TszCheckerOptions {
                no_lib: true,
                ..TszCheckerOptions::default()
            },
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: Vec::new(),
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x31; 32]),
        }
    }

    fn sources() -> Vec<TszFileInput> {
        vec![TszFileInput {
            path: "src/main.ts".into(),
            source: "export const value: number = 1;".into(),
        }]
    }

    #[test]
    fn lane_reuses_only_exact_root_and_retained_configured_programs() {
        let mut lane = TypeScriptProgramAuthorityLane::default();
        let a = Some("tsconfig.a.json".to_owned());
        let b = Some("tsconfig.b.json".to_owned());
        lane.prepare(Path::new("/a"), &[a.clone(), b.clone()])
            .unwrap();
        for key in [&a, &b] {
            let report = lane
                .authority(key.clone())
                .update(sources(), recipe(), &[])
                .unwrap();
            lane.record(report);
        }
        lane.prepare(Path::new("/a"), &[b.clone(), a.clone()])
            .unwrap();
        let report = lane
            .authority(a.clone())
            .update(sources(), recipe(), &[])
            .unwrap();
        assert!(report.reused_project_result);
        lane.record(report);
        assert_eq!(lane.work().checked_programs, 2);
        assert_eq!(lane.work().reused_checked_programs, 1);
        lane.prepare(Path::new("/a"), &[b.clone()]).unwrap();
        assert!(lane.project(&a).is_none());
        assert!(lane.project(&b).is_some());
        lane.prepare(Path::new("/b"), &[b.clone()]).unwrap();
        assert!(lane.project(&b).is_none());
        let report = lane.authority(b).update(sources(), recipe(), &[]).unwrap();
        assert!(!report.reused_project_result);
    }

    #[test]
    fn optional_capture_exhaustion_preserves_checkpoint_stops() {
        struct Check(bool);
        impl backend_frontend_typescript::TszExecutionCheckpoint for Check {
            fn checkpoint(&self, _: u64) -> Result<(), backend_frontend_typescript::TszProjectExecutionStop> {
                if self.0 { Err(backend_frontend_typescript::TszProjectExecutionStop::Cancelled) } else { Ok(()) }
            }
        }
        let mut authority = TszProjectAuthority::default();
        authority.update(sources(), recipe(), &[]).unwrap();
        let project = authority.project().unwrap();
        let text = sources().remove(0).source;
        let source = backend_semantic::ir::SourceIdentity::from_bytes(text.as_bytes()).unwrap();
        let mut remaining = 0;
        let result = project.with_source_nodes("src/main.ts", |arena, retained| {
            super::capture_headers(arena, retained, "src/main.ts", source, &[], &Check(false), &mut remaining)
        }).unwrap();
        assert!(matches!(result, Ok(None)));
        let result = project.with_source_nodes("src/main.ts", |arena, retained| {
            super::capture_headers(arena, retained, "src/main.ts", source, &[], &Check(true), &mut remaining)
        }).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn duplicate_nodes_cannot_alias_one_mutable_authority() {
        let mut lane = TypeScriptProgramAuthorityLane::default();
        lane.prepare(Path::new("/a"), &[None]).unwrap();
        lane.authority(None)
            .update(sources(), recipe(), &[])
            .unwrap();
        assert_eq!(
            lane.prepare(Path::new("/a"), &[None, None]),
            Err(TypeScriptProgramLaneError::DuplicateProgramIdentity)
        );
        assert!(lane.project(&None).is_none());
    }
}

/// Capture only compact claims while the admitted TSZ arena and exact image coexist.
pub(super) fn capture_headers(
    arena: &backend_frontend_typescript::TszNodeArena,
    text: &str,
    path: &str,
    source: backend_semantic::ir::SourceIdentity,
    encoded: &[u8],
    checkpoint: &dyn backend_frontend_typescript::TszExecutionCheckpoint,
    remaining: &mut usize,
) -> Result<Option<super::compiler::StagedNativeTypeScriptHeaders>, String> {
    use backend_semantic::ir::{
        DeclarationPublicationRole, FactAvailability, ImageProvenance, ItemKind,
        SemanticCoreReader as _, SemanticImageView,
    };
    const SCRATCH: usize = 16384 * 512;
    checkpoint
        .checkpoint(0)
        .map_err(|e| format!("header capture stopped: {e:?}"))?;
    if text.len() > 1024 * 1024 || path.len() > 4096 || *remaining < SCRATCH {
        return Ok(None);
    }
    let image = SemanticImageView::reopen(encoded).map_err(|e| format!("header image: {e}"))?;
    let ImageProvenance::Captured {
        source: image_source,
        recipe,
        scope,
        ..
    } = image.image_facts().provenance
    else {
        return Ok(None);
    };
    if image_source != source
        || image.atom(scope.path) != Some(path.as_bytes())
        || recipe.tool != backend_semantic::vocabulary::NativeTool::TypeScriptCompiler
    {
        return Ok(None);
    }
    let mut rows = backend_frontend_typescript::native_project::function_headers_from_arena(
        arena, text, checkpoint,
    )?;
    backend_frontend_typescript::native_project::index_function_headers_by_end(&mut rows, checkpoint)?;
    let session = super::DocumentationSession::new(&image);
    let mut slots = BTreeMap::new();
    for item in session.publication_entities() {
        checkpoint
            .checkpoint(1)
            .map_err(|e| format!("header capture stopped: {e:?}"))?;
        let item = item.map_err(|e| format!("header entity: {e}"))?;
        if item.entity.kind != ItemKind::Function
            || item.entity.authority.publication != DeclarationPublicationRole::PrimaryOwned
            || item.entity.authority.source != FactAvailability::Captured
            || item.entity.authority.source_file != FactAvailability::Captured
        {
            continue;
        }
        let Some(span) = item.entity.source.filter(|span| !span.is_empty()) else {
            continue;
        };
        if image.atom(span.file()) != Some(path.as_bytes()) {
            continue;
        }
        let Some(name) = item
            .name
            .named_bytes()
            .and_then(|name| std::str::from_utf8(name).ok())
        else {
            continue;
        };
        if slots.len() >= 16384 {
            return Err("native header entity limit exceeded".into());
        }
        slots
            .entry((span.start(), span.end(), name))
            .and_modify(|slot| *slot = None)
            .or_insert(Some((item.entity.id.raw, item.entity.version.identity())));
    }
    let mut entries = Vec::new();
    for ((start, end, name), identity) in slots {
        let Some((entity_id, identity)) = identity else {
            continue;
        };
        let Some(header_extent) =
            backend_frontend_typescript::native_project::selected_function_header_range(
                backend_frontend_typescript::native_project::function_headers_with_end(&rows, end, checkpoint)?,
                text,
                (start, end),
                name,
                checkpoint,
            )?
        else {
            continue;
        };
        entries.push(backend_semantic::native_ts_headers::NativeTsHeaderEntry {
            entity_id,
            identity,
            compiler_extent: (start, end),
            header_extent,
        });
    }
    entries.sort_unstable_by_key(|entry| entry.entity_id);
    if entries.is_empty()
        || entries
            .windows(2)
            .any(|pair| pair[0].entity_id == pair[1].entity_id)
    {
        return Ok(None);
    }
    let charge = std::mem::size_of::<super::compiler::StagedNativeTypeScriptHeaders>()
        .checked_add(path.len())
        .and_then(|n| {
            n.checked_add(entries.capacity().checked_mul(std::mem::size_of::<
                backend_semantic::native_ts_headers::NativeTsHeaderEntry,
            >())?)
        })
        .ok_or("header budget overflow")?;
    let Some(after) = remaining.checked_sub(charge) else {
        checkpoint.checkpoint(0).map_err(|e| format!("header capture stopped: {e:?}"))?;
        return Ok(None);
    };
    *remaining = after;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-ts-header-spans.v1\0");
    hash.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
    checkpoint
        .checkpoint(0)
        .map_err(|e| format!("header capture stopped: {e:?}"))?;
    Ok(Some(super::compiler::StagedNativeTypeScriptHeaders {
        path: path.to_owned(),
        source,
        image_identity: *backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(encoded)
            .as_ref(),
        image_byte_len: encoded.len() as u64,
        producer_recipe: *hash.finalize().as_bytes(),
        entries,
    }))
}
