//! Checked original-capture evidence for compiler-only native source refusals.

use super::*;
use std::collections::BTreeSet;

const MAX_ANCESTOR_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NATIVE_PROOF_PATHS: usize = 4096;
const MAX_NATIVE_PROOF_BYTES: u64 = 64 * 1024 * 1024;

pub(super) fn validate_original_native_refusal(
    store: &backend_engine::FileStore,
    current_root: [u8; 32],
    current_sequence: u64,
    evidence_root: [u8; 32],
    evidence_sequence: u64,
    selected_source: &WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    key: &ProductSemanticPublicationKey,
    failure: &backend_library::PackageCompilerFailure,
    capture: &ProductSemanticCaptureRecord,
) -> Result<(), BuiltinModelError> {
    let historical = backend_engine::workspace::recovery::selected_ancestor_transition(
        store,
        current_root,
        current_sequence,
        *capture.source_workspace_root(),
        capture.source_workspace_sequence(),
        MAX_ANCESTOR_JOURNAL_BYTES,
    )
    .map_err(|error| BuiltinModelError(format!("open authenticated source capture: {error}")))?;
    if historical.request() != *capture.request_identity() {
        return Err(BuiltinModelError(
            "source capture request identity differs from historical publication".to_owned(),
        ));
    }
    let untrusted =
        WorkspaceManifest::decode_untrusted(historical.manifest_bytes()).map_err(|error| {
            BuiltinModelError(format!("decode captured workspace manifest: {error}"))
        })?;
    let root_for = |schema| {
        untrusted
            .relations()
            .iter()
            .find(|binding| binding.schema() == schema)
            .map(|binding| binding.root())
            .ok_or_else(|| BuiltinModelError("captured workspace relation is absent".to_owned()))
    };
    let source_root = root_for(backend_version::SchemaIdentity::of_relation::<
        BuiltinWorkspaceRelation,
    >())?;
    let semantic_root = root_for(backend_version::SchemaIdentity::of_relation::<
        BuiltinSemanticRelation,
    >())?;
    let source = historical
        .relation::<BuiltinWorkspaceRelation>(store, source_root)
        .map_err(|error| BuiltinModelError(format!("open captured source relation: {error}")))?;
    let semantic = historical
        .relation::<BuiltinSemanticRelation>(store, semantic_root)
        .map_err(|error| BuiltinModelError(format!("open captured semantic relation: {error}")))?;
    let manifest = workspace_manifest_from_root(&source.root_handle(), &semantic.root_handle())?;
    let manifest = admit_manifest(untrusted, &manifest)?;
    if manifest.root().as_bytes() != capture.source_workspace_root() {
        return Err(BuiltinModelError(
            "source capture manifest differs from recorded root".to_owned(),
        ));
    }
    let membership = store
        .open_closure(historical.membership_id().ok_or_else(|| {
            BuiltinModelError("historical capture has no authenticated closure".to_owned())
        })?)
        .map_err(|error| BuiltinModelError(format!("open captured closure: {error:?}")))?;
    for object in [source.root_object(), semantic.root_object()] {
        let object = object
            .map_err(|error| BuiltinModelError(format!("read captured relation root: {error}")))?;
        if !membership
            .contains_object_id(object.id())
            .map_err(|error| {
                BuiltinModelError(format!("check captured root membership: {error:?}"))
            })?
        {
            return Err(BuiltinModelError(
                "captured closure omits its relation root".to_owned(),
            ));
        }
    }
    let commit = Commit::decode_untrusted(historical.commit_bytes())
        .map_err(|error| BuiltinModelError(format!("decode captured commit: {error}")))?;
    let provenance = commit
        .provenance()
        .clone()
        .admit(
            ObjectClosure::from_version(ObjectVersion::<AuthorityVersionSchema>::from_value(
                AUTHORITY_VALUE,
            )),
            ObjectClosure::from_version(historical.transaction().version()),
        )
        .map_err(|error| BuiltinModelError(format!("admit captured provenance: {error}")))?;
    if provenance.detail() != historical.request() {
        return Err(BuiltinModelError(
            "captured commit request differs from its publication".to_owned(),
        ));
    }
    let commit = commit
        .admit(&manifest, provenance)
        .map_err(|error| BuiltinModelError(format!("admit captured commit: {error}")))?;
    if commit.id().as_bytes() != capture.source_commit() {
        return Err(BuiltinModelError(
            "captured commit identity differs from the pending receipt".to_owned(),
        ));
    }
    let objects = historical.closure_manifest().objects();
    let capture_root = selected_capture_root_from_objects(objects)?.ok_or_else(|| {
        BuiltinModelError("historical source capture marker is absent".to_owned())
    })?;
    let captures = historical
        .relation::<ProductSemanticCaptureRelation>(store, capture_root)
        .map_err(|error| BuiltinModelError(format!("open historical source capture: {error}")))?;
    let capture_object = captures
        .root_object()
        .map_err(|error| BuiltinModelError(format!("read captured marker root: {error}")))?;
    if !membership
        .contains_object_id(capture_object.id())
        .map_err(|error| {
            BuiltinModelError(format!("check captured marker membership: {error:?}"))
        })?
    {
        return Err(BuiltinModelError(
            "captured closure omits its marker root".to_owned(),
        ));
    }
    let original = captures
        .lookup(key)
        .map_err(|error| BuiltinModelError(format!("read historical source capture: {error}")))?
        .ok_or_else(|| BuiltinModelError("historical profile capture is absent".to_owned()))?;
    if !matches!(
        original.outcome(),
        ProductSemanticCaptureOutcome::Pending { .. }
    ) || original.capture() != capture.capture()
        || original.operation_key() != capture.operation_key()
        || original.request_identity() != capture.request_identity()
        || original.base_workspace_root() != capture.base_workspace_root()
        || original.base_workspace_sequence() != capture.base_workspace_sequence()
        || original.source_workspace_root() != capture.source_workspace_root()
        || original.source_workspace_sequence() != capture.source_workspace_sequence()
        || original.source_commit() != capture.source_commit()
    {
        return Err(BuiltinModelError(
            "historical pending capture differs from the retained receipt".to_owned(),
        ));
    }
    let project = key.package_key().to_bytes();
    let project_row = source
        .lookup(&project)
        .map_err(|error| BuiltinModelError(format!("read captured project: {error}")))?
        .ok_or_else(|| BuiltinModelError("captured project is absent".to_owned()))?;
    if project_row
        .project_fields()
        .map(|fields| fields.source_version)
        != Some(*capture.capture().source_version())
    {
        return Err(BuiltinModelError(
            "captured project source version differs from receipt".to_owned(),
        ));
    }
    let keys = resolve_project_file_keys(project, &project_row, |page| {
        source
            .lookup(page)
            .map_err(|error| BuiltinModelError(format!("read captured membership: {error}")))
    })?;
    let selected_project = selected_source
        .lookup(&project)
        .map_err(|error| BuiltinModelError(format!("read selected project: {error}")))?
        .ok_or_else(|| BuiltinModelError("selected project is absent".to_owned()))?;
    if selected_project
        .project_fields()
        .map(|fields| fields.source_version)
        != Some(*capture.capture().source_version())
    {
        return Err(BuiltinModelError(
            "selected project source version differs from capture".to_owned(),
        ));
    }
    let selected_keys = resolve_project_file_keys(project, &selected_project, |page| {
        selected_source
            .lookup(page)
            .map_err(|error| BuiltinModelError(format!("read selected membership: {error}")))
    })?;
    if keys != selected_keys {
        return Err(BuiltinModelError(
            "selected source membership differs from the original capture".to_owned(),
        ));
    }
    let mut paths = BTreeSet::new();
    let mut failure_row = None;
    for file_key in keys {
        let row = source
            .lookup(&file_key)
            .map_err(|error| BuiltinModelError(format!("read captured file: {error}")))?
            .ok_or_else(|| BuiltinModelError("captured file row is absent".to_owned()))?;
        if selected_source
            .lookup(&file_key)
            .map_err(|error| BuiltinModelError(format!("read selected file: {error}")))?
            != Some(row.clone())
        {
            return Err(BuiltinModelError(
                "selected source file differs from the original capture".to_owned(),
            ));
        }
        validate_project_file(project, file_key, &row)?;
        let file = row
            .file_fields()
            .ok_or_else(|| BuiltinModelError("captured member is not a file".to_owned()))?;
        if super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?
            == Some(key.profile())
        {
            let expected_language = match key.profile() {
                backend_semantic::vocabulary::LanguageProfile::CSharp(_) => {
                    backend_compile::SourceLanguage::CSharp
                }
                backend_semantic::vocabulary::LanguageProfile::C(_)
                | backend_semantic::vocabulary::LanguageProfile::Cxx(_) => {
                    backend_compile::SourceLanguage::Clang
                }
                _ => {
                    return Err(BuiltinModelError(
                        "refusal is not a native compiler profile".to_owned(),
                    ));
                }
            };
            if file.language != expected_language {
                return Err(BuiltinModelError(
                    "captured native member has a different language".to_owned(),
                ));
            }
            paths.insert(file.path.to_owned());
        }
        if file.path == failure.relative_path() {
            failure_row = Some(row);
        }
    }
    let failure_row = failure_row.ok_or_else(|| {
        BuiltinModelError("refusal path is outside captured membership".to_owned())
    })?;
    let file = failure_row
        .file_fields()
        .ok_or_else(|| BuiltinModelError("refusal source is not a file".to_owned()))?;
    let expected_language = match key.profile() {
        backend_semantic::vocabulary::LanguageProfile::CSharp(_) => {
            backend_compile::SourceLanguage::CSharp
        }
        backend_semantic::vocabulary::LanguageProfile::C(_)
        | backend_semantic::vocabulary::LanguageProfile::Cxx(_) => {
            backend_compile::SourceLanguage::Clang
        }
        _ => {
            return Err(BuiltinModelError(
                "refusal is not a native compiler profile".to_owned(),
            ));
        }
    };
    if file.project != project
        || file.source_identity.is_some()
        || file.language != expected_language
        || file
            .admitted_unavailable_reason()
            .map_err(BuiltinModelError)?
            != Some(backend_engine::SourceUnavailableReason::Unparsed)
        || super::super::ingest::source_record_profile(file).map_err(BuiltinModelError)?
            != Some(key.profile())
    {
        return Err(BuiltinModelError(
            "refusal source is not an exact malformed native member".to_owned(),
        ));
    }
    let pointer_schema = backend_version::SchemaIdentity::new(
        ProductNativeFrontierRootSchema::DOMAIN,
        ProductNativeFrontierRootSchema::TYPE,
        ProductNativeFrontierRootSchema::VERSION,
    );
    let pointer_key = ObjectKey::<ProductNativeFrontierRootSchema>::from_value(&[0x4e; 32]);
    let mut pointers = objects
        .iter()
        .filter(|object| object.schema() == pointer_schema);
    let pointer = pointers
        .next()
        .ok_or_else(|| BuiltinModelError("historical native frontier is absent".to_owned()))?;
    if pointer.key() != pointer_key.as_bytes() || pointers.next().is_some() {
        return Err(BuiltinModelError(
            "historical native frontier is ambiguous".to_owned(),
        ));
    }
    let native_root: [u8; 32] = pointer.bytes().try_into().map_err(|_| {
        BuiltinModelError("historical native frontier root is malformed".to_owned())
    })?;
    let native = historical
        .relation::<ProductNativeFrontierRelation>(store, native_root)
        .map_err(|error| BuiltinModelError(format!("open historical native frontier: {error}")))?;
    let native_object = native
        .root_object()
        .map_err(|error| BuiltinModelError(format!("read captured native root: {error}")))?;
    if !membership
        .contains_object_id(native_object.id())
        .map_err(|error| {
            BuiltinModelError(format!("check captured native membership: {error:?}"))
        })?
    {
        return Err(BuiltinModelError(
            "captured closure omits its native root".to_owned(),
        ));
    }
    if paths.len() > MAX_NATIVE_PROOF_PATHS {
        return Err(BuiltinModelError(
            "native refusal proof exceeds path budget".to_owned(),
        ));
    }
    let mut malformed_bytes = 0_u64;
    for path in &paths {
        let path_key =
            backend_engine::builtin::product_native_frontier_path_key(project, key.profile(), path)
                .map_err(BuiltinModelError)?;
        let record = native.lookup(&path_key).map_err(|error| {
            BuiltinModelError(format!("read native proof disposition: {error}"))
        })?;
        if let Some(ProductNativeFrontierRecord::Path { disposition, .. }) = record {
            let bytes = match disposition {
                NativePathDisposition::CSharpMalformed { byte_length, .. }
                | NativePathDisposition::ClangMalformed { byte_length, .. } => {
                    u64::from(byte_length)
                }
                _ => 0,
            };
            malformed_bytes = malformed_bytes.checked_add(bytes).ok_or_else(|| {
                BuiltinModelError("native refusal proof exceeds byte budget".to_owned())
            })?;
            if malformed_bytes > MAX_NATIVE_PROOF_BYTES {
                return Err(BuiltinModelError(
                    "native refusal proof exceeds byte budget".to_owned(),
                ));
            }
        }
    }
    let checked =
        backend_engine::builtin::admit_native_frontier(&native, project, key.profile(), &paths)
            .map_err(|error| {
                BuiltinModelError(format!("admit historical native frontier: {error}"))
            })?;
    // Native candidate admission requires a complete compilable frontier;
    // its observed count covers every structural or malformed input path.
    if !checked.is_compilable()
        || u64::try_from(checked.dispositions().len()).ok()
            != Some(capture.capture().source_count())
    {
        return Err(BuiltinModelError(
            "native compiler source count differs from the capture".to_owned(),
        ));
    }
    let selected = backend_engine::workspace::recovery::selected_ancestor_transition(
        store,
        current_root,
        current_sequence,
        evidence_root,
        evidence_sequence,
        MAX_ANCESTOR_JOURNAL_BYTES,
    )
    .map_err(|error| BuiltinModelError(format!("open selected native frontier: {error}")))?;
    let mut selected_pointers = selected
        .closure_manifest()
        .objects()
        .iter()
        .filter(|object| object.schema() == pointer_schema);
    let selected_pointer = selected_pointers
        .next()
        .ok_or_else(|| BuiltinModelError("selected native frontier is absent".to_owned()))?;
    if selected_pointer.key() != pointer_key.as_bytes() || selected_pointers.next().is_some() {
        return Err(BuiltinModelError(
            "selected native frontier is ambiguous".to_owned(),
        ));
    }
    let selected_root: [u8; 32] = selected_pointer
        .bytes()
        .try_into()
        .map_err(|_| BuiltinModelError("selected native frontier root is malformed".to_owned()))?;
    let selected_native = selected
        .relation::<ProductNativeFrontierRelation>(store, selected_root)
        .map_err(|error| BuiltinModelError(format!("open selected native relation: {error}")))?;
    let selected_membership = store
        .open_closure(selected.membership_id().ok_or_else(|| {
            BuiltinModelError("selected publication has no authenticated closure".to_owned())
        })?)
        .map_err(|error| BuiltinModelError(format!("open selected closure: {error:?}")))?;
    let selected_object = selected_native
        .root_object()
        .map_err(|error| BuiltinModelError(format!("read selected native root: {error}")))?;
    if !selected_membership
        .contains_object_id(selected_object.id())
        .map_err(|error| {
            BuiltinModelError(format!("check selected native membership: {error:?}"))
        })?
    {
        return Err(BuiltinModelError(
            "selected closure omits its native root".to_owned(),
        ));
    }
    let profile_key =
        backend_engine::builtin::product_native_frontier_profile_key(project, key.profile());
    let original_profile = native
        .lookup(&profile_key)
        .map_err(|error| BuiltinModelError(format!("read captured native profile: {error}")))?;
    let selected_profile = selected_native
        .lookup(&profile_key)
        .map_err(|error| BuiltinModelError(format!("read selected native profile: {error}")))?;
    if original_profile.is_none() || selected_profile != original_profile {
        return Err(BuiltinModelError(
            "selected native profile differs from the original capture".to_owned(),
        ));
    }
    backend_engine::builtin::admit_native_frontier(
        &selected_native,
        project,
        key.profile(),
        &paths,
    )
    .map_err(|error| BuiltinModelError(format!("admit selected native frontier: {error}")))?;
    let target = match key.profile() {
        backend_semantic::vocabulary::LanguageProfile::CSharp(_) => checked
            .csharp_candidate_permit(failure.relative_path())
            .map(|permit| permit.package_target().clone()),
        backend_semantic::vocabulary::LanguageProfile::C(_)
        | backend_semantic::vocabulary::LanguageProfile::Cxx(_) => checked
            .candidate_permit(failure.relative_path())
            .map(|permit| permit.package_target().clone()),
        _ => None,
    };
    if target.as_ref() != Some(key.coordinate()) {
        return Err(BuiltinModelError(
            "native refusal target differs from captured compiler target".to_owned(),
        ));
    }
    let disposition = checked
        .dispositions()
        .iter()
        .find(|(path, _)| path == failure.relative_path())
        .map(|(_, disposition)| *disposition);
    match disposition {
        Some(NativePathDisposition::CSharpMalformed {
            source_identity,
            byte_length,
            ..
        })
        | Some(NativePathDisposition::ClangMalformed {
            source_identity,
            byte_length,
            ..
        }) if source_identity == failure.source_identity()
            && byte_length == failure.source_byte_len() =>
        {
            Ok(())
        }
        _ => Err(BuiltinModelError(
            "typed compiler refusal source identity does not match captured source".to_owned(),
        )),
    }
}
