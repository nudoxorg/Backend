use super::super::{
    BuiltinAuthorityVerifier, BuiltinCaptureChange, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinSourceFactsChange,
    BuiltinValidator, BuiltinWorkspaceRelation, ProductSourceRecord, ingest,
};
use super::index_operation::IndexOperationJournal;
use backend_engine::application::{
    CaptureWorkspaceIdentityV2, CapturedFullWorkspaceV2, CompilerBalancingRequest,
    CompilerByteCredits, CompilerCpuCredits, CompilerDemand, CompilerInputAdmissionError,
    CompilerInputAdmissionEvidence, CompilerInputAdmissionVerifier, CompilerMemoryCredits,
    CompilerPackageTargetV2, CompilerResourceCredits, CompilerSessionLineage, CompilerWorkIdentity,
    CompilerWorkspaceEntryV2, ExactInputWitness, FullWorkspaceInputClaim, FullWorkspaceInputError,
    FullWorkspaceInputVerifier, LocalCompilerAvailability, LocalCompilerClient, OwnedPackageSource,
    OwnedPackageSourceSet, PackageLineageId, PackageSemanticError, PackageSemanticRuntimeError,
    StagedSemanticPackage, VerifiedCompilerInput, VerifiedCompilerInputAdmission,
    VerifierAcceptedFullWorkspaceInput, capture_full_workspace_v2_with_prior,
};
use backend_engine::builtin::{
    PartialSemanticCoverage, ProductSemanticCaptureOutcome, ProductSemanticCaptureRelation,
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord, ProductSourceFileFactsRecord,
    ProductSourceFileFactsRelation, ProductSourceFileFactsUpdate, SemanticPublicationClaim,
    SemanticPublicationCoverage, SemanticPublicationSelection, SemanticPublicationVersion,
    SemanticSourceCapture, product_source_file_facts_record_key,
    product_source_file_facts_relation, product_source_file_facts_row_keys,
    semantic_capture_relation,
};
use backend_extension_turso::SourceObservationReceipt;
use backend_library::interface::{
    CompilerRuntimeCause, CompilerTerminal, CorrelationId, GenerateTarget, PackageCompileRequest,
    PackageUrl,
};
use backend_library::{
    CargoPackageAliasEvidenceV1, CompileExecutionIntent, PackageCompilerFailure,
};
use backend_semantic::ir::SemanticInputWitness;
#[cfg(test)]
use backend_semantic::vocabulary::Language;
use backend_semantic::vocabulary::LanguageProfile;
use backend_version::{Coverage, Relation as _, ScopeRoot, WorkspaceRoot};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub(super) fn index_project_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    index_project_intent_at_with_cluster(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        compiler,
        semantic_authority,
        None,
        None,
    )
}

pub(super) fn index_project_intent_with_cluster(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    index_project_intent_at_with_cluster(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
    )
}

pub(super) fn index_project_intent_with_cluster_and_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    index_project_intent_at_with_cluster_and_intent(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        execution_intent,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
    )
}

/// Prepares indexing a local project folder on the owner loop without changing
/// the selected product. When a local compile is needed (and no compiler
/// cluster may take it), the compile is handed back as a [`DeferredIndex`]:
/// the caller runs it off the loop and finishes the whole product transaction
/// on the loop afterward.
pub(super) fn prepare_index_project(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<PreparedIndex, BuiltinModelError> {
    prepare_index_project_at(
        daemon,
        package,
        label,
        Path::new(label),
        None,
        request_id,
        execution_intent,
        compiler,
        semantic_authority,
        None,
        None,
        true,
    )
}

pub(super) fn index_project_intent_at(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    index_project_intent_at_with_cluster(
        daemon,
        package,
        label,
        source_root,
        coordinate,
        request_id,
        compiler,
        semantic_authority,
        None,
        None,
    )
}

fn index_project_intent_at_with_cluster(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    index_project_intent_at_with_cluster_and_intent(
        daemon,
        package,
        label,
        source_root,
        coordinate,
        request_id,
        CompileExecutionIntent::Interactive,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
    )
}

fn index_project_intent_at_with_cluster_and_intent(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
) -> Result<Option<PreparedProductSelection>, BuiltinModelError> {
    match prepare_index_project_at(
        daemon,
        package,
        label,
        source_root,
        coordinate,
        request_id,
        execution_intent,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
        false,
    )? {
        PreparedIndex::Ready(prepared) => Ok(prepared.into_option()),
        PreparedIndex::Compile(_) => Err(BuiltinModelError(
            "an index compile was deferred on a path that runs it in place".to_owned(),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_index_project_at(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
    defer: bool,
) -> Result<PreparedIndex, BuiltinModelError> {
    let work = capture_index_scan(
        daemon,
        package,
        label,
        source_root,
        coordinate,
        request_id,
        execution_intent,
        owner_cluster.is_some(),
        Arc::new(AtomicBool::new(false)),
    )?;
    let mut committed_captures = BTreeMap::new();
    let result = finish_index_scan(
        daemon,
        run_index_scan(work).map_err(IndexScanFailure::into_model_error)?,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
        defer,
        None,
        &mut committed_captures,
        None,
    );
    match result {
        Err(primary) if !committed_captures.is_empty() => {
            match commit_pending_capture_failure(
                daemon,
                package,
                label,
                request_id,
                &committed_captures,
                backend_engine::builtin::SemanticUnavailableReason::Rejected,
                None,
            ) {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(BuiltinModelError(format!(
                    "{primary}; additionally, recording the terminal source-capture outcome failed: {cleanup}"
                ))),
            }
        }
        other => other,
    }
}

/// Captures the owner-backed source frontier and workspace root before an
/// index scan is handed to a worker. This copies only bounded relation data;
/// filesystem discovery stays outside the owner loop.
#[allow(clippy::too_many_arguments)]
pub(super) fn capture_index_scan(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    source_root: &Path,
    coordinate: Option<&PackageUrl>,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    capture_workspace_snapshot: bool,
    cancellation: Arc<AtomicBool>,
) -> Result<IndexScanWork, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let project_key = package.to_bytes();
    let before = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?;
    let old_files = match before.as_ref() {
        Some(record) => {
            super::super::profile::resolve_project_file_keys(project_key, record, |page_key| {
                relation.lookup(page_key).map_err(|error| {
                    BuiltinModelError(format!("read indexed membership page: {error}"))
                })
            })?
        }
        None => Vec::new(),
    };
    if old_files.len() > ProductSourceRecord::MAX_PROJECT_FILES {
        return Err(BuiltinModelError(
            "project source frontier exceeds its bounded file limit".to_owned(),
        ));
    }
    let admitted_files = relation
        .lookup_many_sorted(&old_files)
        .map_err(|error| BuiltinModelError(format!("read reusable source files: {error}")))?;
    let mut reusable = BTreeMap::new();
    for (key, record) in old_files.iter().copied().zip(admitted_files) {
        let record = record.ok_or_else(|| {
            BuiltinModelError("project frontier refers to a missing source file".to_owned())
        })?;
        super::super::profile::validate_project_file(project_key, key, &record)?;
        reusable.insert(key, record);
    }
    Ok(IndexScanWork {
        package,
        label: label.to_owned(),
        source_root: source_root.to_path_buf(),
        coordinate: coordinate.cloned(),
        request_id,
        execution_intent,
        project_key,
        workspace_root: daemon.engine().daemon().owner().head().root(),
        before,
        old_files,
        reusable,
        capture_workspace_snapshot,
        cancellation,
    })
}

/// Performs filesystem discovery and optional full compiler-workspace capture
/// using only the immutable input copied from the owner.
pub(super) fn run_index_scan(work: IndexScanWork) -> Result<IndexScanResult, IndexScanFailure> {
    if work.cancellation.load(Ordering::Acquire) {
        return Err(IndexScanFailure::Cancelled);
    }
    let source_coordinate = work.source_root.to_string_lossy();
    let scan = ingest::scan_project_for_unproven_authorities_cancellable(
        &source_coordinate,
        work.project_key,
        &work.reusable,
        &work.cancellation,
    )
    .map_err(|error| {
        if work.cancellation.load(Ordering::Acquire) {
            IndexScanFailure::Cancelled
        } else {
            IndexScanFailure::Refused(BuiltinModelError(error))
        }
    })?;
    // Remote execution is available only when a complete, confined workspace
    // inventory can be captured. Failure to build that optional evidence
    // leaves the existing local compile path available.
    let workspace_snapshot = work.capture_workspace_snapshot.then(|| {
        match ingest::CompilerWorkspaceSnapshot::open_with_cancellation(
            &work.source_root,
            Some(&work.cancellation),
        ) {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                eprintln!(
                    "locald compiler route fallback: full workspace capture unavailable ({error})"
                );
                None
            }
        }
    }).flatten();
    if work.cancellation.load(Ordering::Acquire) {
        return Err(IndexScanFailure::Cancelled);
    }
    let workspace_is_current = match workspace_snapshot.as_ref() {
        Some(snapshot) => snapshot
            .revalidate_with_cancellation(Some(&work.cancellation))
            .map_err(|error| {
                if work.cancellation.load(Ordering::Acquire) {
                    IndexScanFailure::Cancelled
                } else {
                    IndexScanFailure::Refused(BuiltinModelError(error))
                }
            })?,
        None => true,
    };
    if !ingest::compiler_revision_is_current_with_cancellation(
        &scan.revision_fence,
        Some(&work.cancellation),
    )
    .map_err(|error| {
        if work.cancellation.load(Ordering::Acquire) {
            IndexScanFailure::Cancelled
        } else {
            IndexScanFailure::Refused(BuiltinModelError(error))
        }
    })? || !workspace_is_current
    {
        return Err(IndexScanFailure::Refused(BuiltinModelError(
            "project files changed while the index scan was running; retry indexing".to_owned(),
        )));
    }
    Ok(IndexScanResult {
        work,
        scan,
        workspace_snapshot,
    })
}

/// Completes source and semantic admission on the owner after a worker scan.
/// The captured workspace root fences the immutable scan input from a newer
/// owner selection before any candidate observations are prepared.
#[allow(clippy::too_many_arguments)]
pub(super) fn finish_index_scan(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    result: IndexScanResult,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
    defer: bool,
    operation_key: Option<backend_library::IndexOperationKey>,
    committed_captures: &mut BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    mut index_operations: Option<&mut IndexOperationJournal>,
) -> Result<PreparedIndex, BuiltinModelError> {
    let current_root = daemon.engine().daemon().owner().head().root();
    if current_root != result.work.workspace_root {
        return Err(BuiltinModelError(
            "workspace selection changed while the project scan was running; retry indexing"
                .to_owned(),
        ));
    }
    let IndexScanResult {
        work,
        scan,
        workspace_snapshot,
    } = result;
    let final_revision_fence = scan.revision_fence.clone();
    let cancellation = work.cancellation.clone();
    let base_snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = base_snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let facts_relation = product_source_file_facts_relation(&base_snapshot)
        .map_err(|error| BuiltinModelError(format!("open product source facts: {error}")))?;
    let IndexScanWork {
        package,
        label,
        source_root,
        coordinate,
        request_id,
        execution_intent,
        project_key,
        before,
        old_files,
        reusable,
        ..
    } = work;
    let requested_root = source_root.as_path();
    let coordinate = coordinate.as_ref();
    let revision_fence = &scan.revision_fence;
    if cancellation.load(Ordering::Acquire) {
        return Err(BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()));
    }
    if !ingest::compiler_revision_is_current_with_cancellation(
        revision_fence,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?
    {
        return Err(BuiltinModelError(
            "project root or files changed after source admission; retry indexing".to_owned(),
        ));
    }
    // Use the root pinned by the scan for configuration, compiler-input, and
    // profile reads. Keep the original spelling only for alias-retarget checks
    // at later admission fences.
    let source_root = revision_fence.canonical_root();
    let inputs = compiler_input_admissions(source_root, &scan, compiler);
    let semantic_context = SemanticCompilationContext::admit(
        package,
        &label,
        source_root,
        coordinate,
        request_id,
        compiler,
    )?;
    let live = ingest::live_compiler_profiles(
        source_root,
        &scan.compiler_sources,
        &scan.reused_compiler_files,
    );
    let present =
        ingest::present_compiler_paths(&scan.compiler_sources, &scan.reused_compiler_files);
    let lost = ingest::lost_compiler_profiles(source_root, &reusable, &present)
        .map_err(BuiltinModelError)?;
    // Persist candidate observations first. They fence compilation but remain
    // private to freshness and query paths until the combined product intent
    // commits.
    let mut observed_profiles = live.clone();
    observed_profiles.extend(lost.iter().copied());
    let mut observations = BTreeMap::new();
    for profile in observed_profiles {
        let coordinate =
            super::super::compiler_scope::semantic_coordinate(package, profile, coordinate)?;
        let key = ProductSemanticPublicationKey::new(
            semantic_context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let input_digest = semantic_input_digest(&scan, profile);
        let count = scan
            .compiler_sources
            .iter()
            .filter(|source| source.profile == profile)
            .count()
            .saturating_add(
                scan.reused_compiler_files
                    .iter()
                    .filter(|source| source.profile == profile)
                    .count(),
            );
        let count = u64::try_from(count)
            .map_err(|_| BuiltinModelError("semantic source count exceeds u64".to_owned()))?;
        observations.insert(
            profile,
            semantic_authority.observe(&key, input_digest, count)?,
        );
    }
    let mut file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project_update = ProductSourceRecord::project_with_membership_pages(
        &label,
        scan.source_version,
        file_keys.clone(),
        None,
    )
    .map_err(BuiltinModelError)?;
    let mut project = project_update.project_record().clone();
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project.clone()),
        });
    }
    let new_page_keys = project_update
        .membership_pages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<BTreeSet<_>>();
    for (key, page) in project_update.membership_pages() {
        let current = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read project membership page: {error}")))?;
        if current.as_ref() != Some(page) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            });
        }
    }
    if let Some(previous) = before
        .as_ref()
        .and_then(ProductSourceRecord::project_fields)
    {
        changes.extend(
            previous
                .files
                .page_keys()
                .iter()
                .copied()
                .filter(|key| !new_page_keys.contains(key))
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
    }
    for (key, record) in &scan.files {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed source file: {error}")))?;
        if current.as_ref() != Some(record) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(record.clone()),
            });
        }
    }
    let selected = file_keys.iter().copied().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .iter()
            .copied()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    let mut source_facts_changes = prepare_source_facts_changes(
        &relation,
        facts_relation.as_ref(),
        &scan.source_facts,
        &scan.files,
        &old_files,
    )?;
    // Keep these source rows private until every semantic profile has been
    // admitted. The eventual BuiltinIntent carries source and semantic roots
    // in one workspace transition.
    // This compiler owner does not expose a complete typed present-and-negative
    // read set, so its source/configuration digest cannot authorize reuse.
    // Every live semantic profile rebuilds until the authority can prove its
    // complete input closure.
    let (semantic_changes, selected, cargo_alias_observations, capture_changes) = {
        let fresh_profiles = scan
            .compiler_sources
            .iter()
            .map(|source| {
                ingest::compilation_profile(source_root, &source.relative_path, source.profile)
            })
            .collect::<BTreeSet<_>>();
        let mut reusable_inputs = BTreeSet::new();
        for (profile, admitted) in &inputs {
            if compiler_exact_witness_is_current(
                daemon,
                package,
                &label,
                coordinate,
                compiler,
                source_root,
                *profile,
                *admitted,
            )? {
                reusable_inputs.insert(*profile);
            }
        }
        let dirty = profiles_requiring_compilation(
            &live,
            &lost,
            &fresh_profiles,
            &inputs,
            &reusable_inputs,
        );
        let (fresh, reused) = ingest::select_compiler_inputs(
            source_root,
            scan.compiler_sources,
            scan.reused_compiler_files,
            &dirty,
        );
        if dirty.is_empty() {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            // Admit the exact structural source frontier together with a
            // per-profile Pending marker before entering any compiler path.
            // Candidate observations remain private until this one workspace
            // transition commits; the selected semantic generation remains
            // the prior coherent claim and is stale against the new source.
            let (captures, pending_changes, _capture_observations) =
                prepare_source_capture_changes(
                    daemon,
                    package,
                    &semantic_context,
                    &dirty,
                    &observations,
                    scan.source_version,
                    operation_key,
                )?;
            if !ingest::compiler_revision_is_current_with_cancellation(
                revision_fence,
                Some(cancellation.as_ref()),
            )
            .map_err(BuiltinModelError)?
            {
                return Err(BuiltinModelError(
                    "compiler source or configuration revision changed before source capture; retry indexing"
                        .to_owned(),
                ));
            }
            let mut capture_intent = BuiltinIntent::index_with_capture(
                package,
                &label,
                std::mem::take(&mut changes),
                Vec::new(),
                pending_changes,
            )?;
            if !source_facts_changes.is_empty() {
                capture_intent =
                    capture_intent.with_source_facts(std::mem::take(&mut source_facts_changes))?;
            }
            let capture_intent = match operation_key {
                Some(key) => capture_intent.with_operation_key(key)?,
                None => capture_intent,
            };
            let capture_request = super::adapter::prepare_builtin_intent(daemon, &capture_intent)?;
            let capture_request_identity = capture_request.request_identity();
            let daemon_for_commit = &mut *daemon;
            semantic_authority.commit_product_selection_transaction(Vec::new(), move || {
                super::adapter::commit_prepared_builtin_intent(
                    daemon_for_commit,
                    request_id,
                    capture_request,
                )
            })?;
            *committed_captures = captures.clone();
            if let (Some(operation_key), Some(index_operations)) =
                (operation_key, index_operations.as_deref_mut())
            {
                let receipt = source_capture_receipt_for_root(
                    daemon,
                    &semantic_context.package_reference,
                    operation_key,
                    Some(capture_request_identity),
                )?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "source capture root does not retain its exact keyed operation marker"
                            .to_owned(),
                    )
                })?;
                index_operations
                    .source_captured(operation_key, receipt)
                    .map_err(|error| {
                        BuiltinModelError(format!("persist source-capture receipt: {error}"))
                    })?;
            }

            // Compilation must use the source rows that actually committed.
            // Re-read the committed project and file frontier instead of
            // trusting the private scan image for semantic staging.
            let (captured_project, captured_file_keys, captured_file_frontier) =
                current_project_frontier(daemon, project_key)?;
            project = captured_project.clone();
            file_keys = captured_file_keys.clone();
            let before_capture = Some(captured_project.clone());

            let sources = ingest::admit_compiler_sources_for_scan(
                requested_root,
                revision_fence,
                fresh,
                reused,
                scan.source_admission_policy,
                Some(cancellation.as_ref()),
            )
            .map_err(BuiltinModelError);
            let sources = match sources {
                Ok(sources) => sources,
                Err(error) => {
                    let terminal = terminal_capture_changes(
                        daemon,
                        &captures,
                        backend_engine::builtin::SemanticUnavailableReason::Rejected,
                        None,
                    )?;
                    commit_semantic_terminal(daemon, package, &label, request_id, terminal)?;
                    return Err(error);
                }
            };
            if defer && owner_cluster.is_none() {
                return prepare_deferred_compile(
                    package,
                    &label,
                    project_key,
                    captured_project,
                    before_capture,
                    captured_file_keys,
                    captured_file_frontier,
                    Vec::new(),
                    &semantic_context,
                    sources,
                    revision_fence.clone(),
                    &observations,
                    captures,
                    semantic_authority,
                )
                .map(PreparedIndex::Compile);
            }
            let (semantic_changes, selected, aliases) = match compile_semantic_publications(
                daemon,
                &semantic_context,
                sources,
                &revision_fence,
                &observations,
                semantic_authority,
                workspace_snapshot.as_ref(),
                owner_cluster,
                pending_stored_acks,
                execution_intent,
            ) {
                Ok(result) => result,
                Err(error) => {
                    let terminal = terminal_capture_changes(
                        daemon,
                        &captures,
                        backend_engine::builtin::SemanticUnavailableReason::Rejected,
                        None,
                    )?;
                    commit_semantic_terminal(daemon, package, &label, request_id, terminal)?;
                    return Err(error);
                }
            };
            let capture_changes = completed_capture_changes(daemon, &captures, &semantic_changes)?;
            (semantic_changes, selected, aliases, capture_changes)
        }
    };
    let committed_relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open selected project source: {error}")))?;
    replace_project_cargo_aliases(
        &mut changes,
        project_key,
        project,
        file_keys,
        before.as_ref(),
        |key| {
            committed_relation.lookup(key).map_err(|error| {
                BuiltinModelError(format!("read project membership page: {error}"))
            })
        },
        cargo_alias_observations,
    )?;
    if cancellation.load(Ordering::Acquire) {
        return Err(BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()));
    }
    if !ingest::compiler_revision_is_current_with_cancellation(
        revision_fence,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?
    {
        return Err(BuiltinModelError(
            "project root or files changed before product selection; retry indexing".to_owned(),
        ));
    }
    let intent = if changes.is_empty()
        && semantic_changes.is_empty()
        && capture_changes.is_empty()
        && source_facts_changes.is_empty()
    {
        None
    } else {
        let intent = if !source_facts_changes.is_empty() {
            BuiltinIntent::index_with_source_facts(
                package,
                &label,
                changes,
                semantic_changes,
                capture_changes,
                source_facts_changes,
            )?
        } else if capture_changes.is_empty() {
            BuiltinIntent::index_with_semantics(package, &label, changes, semantic_changes)?
        } else {
            BuiltinIntent::index_with_capture(
                package,
                &label,
                changes,
                semantic_changes,
                capture_changes,
            )?
        };
        Some(intent)
    };
    Ok(PreparedIndex::Ready(PreparedProductSelection {
        intent,
        selected,
        revision_fence: Some(final_revision_fence),
    }))
}

fn replace_project_cargo_aliases(
    changes: &mut Vec<BuiltinSourceChange>,
    project_key: [u8; 32],
    project: ProductSourceRecord,
    file_keys: Vec<[u8; 32]>,
    before: Option<&ProductSourceRecord>,
    mut lookup: impl FnMut(&[u8; 32]) -> Result<Option<ProductSourceRecord>, BuiltinModelError>,
    observations: Vec<CargoPackageAliasEvidenceV1>,
) -> Result<(), BuiltinModelError> {
    if observations.is_empty() {
        // `project` is the fresh alias-free frontier. The caller compares it
        // with the current row before this helper; if that row carried old
        // Cargo evidence, the resulting source change removes it. Do not
        // preserve aliases by matching only the source digest: the compiler
        // lane currently lacks a complete read-set proof for Cargo config,
        // targets, and negative inputs.
        return Ok(());
    }
    let evidence = CargoPackageAliasEvidenceV1::merge(observations).map_err(|_| {
        BuiltinModelError("Cargo package alias observations are invalid".to_owned())
    })?;
    let fields = project.project_fields().ok_or_else(|| {
        BuiltinModelError("Cargo alias update does not contain a project row".to_owned())
    })?;
    let update = ProductSourceRecord::project_with_membership_pages(
        fields.label,
        fields.source_version,
        file_keys,
        Some(evidence),
    )
    .map_err(BuiltinModelError)?;
    let project = update.project_record();
    let previous_page_keys = before
        .and_then(ProductSourceRecord::project_fields)
        .map_or(&[][..], |fields| fields.files.page_keys());
    let previous_pages = previous_page_keys.iter().copied().collect::<BTreeSet<_>>();
    changes.retain(|change| {
        change.key != project_key
            && !previous_pages.contains(&change.key)
            && !change
                .after
                .as_ref()
                .is_some_and(|record| record.membership_page_fields().is_some())
    });
    if before != Some(project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some((*project).clone()),
        });
    }
    let new_page_keys = update
        .membership_pages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<BTreeSet<_>>();
    for (key, page) in update.membership_pages() {
        let current = lookup(key)?;
        if current.as_ref() != Some(page) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            });
        }
    }
    changes.extend(
        previous_pages
            .difference(&new_page_keys)
            .copied()
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    Ok(())
}

pub(in crate::builtin) fn prepare_source_facts_changes(
    source_relation: &backend_engine::WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
    facts_relation: Option<
        &backend_engine::WorkspaceRelationHandle<ProductSourceFileFactsRelation>,
    >,
    updates: &[ProductSourceFileFactsUpdate],
    selected_files: &[([u8; 32], ProductSourceRecord)],
    previous_files: &[[u8; 32]],
) -> Result<Vec<BuiltinSourceFactsChange>, BuiltinModelError> {
    if selected_files.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(BuiltinModelError(
            "selected source file coordinates must be unique and ordered".to_owned(),
        ));
    }
    let mut afters = BTreeMap::<[u8; 32], Option<ProductSourceFileFactsRecord>>::new();
    for update in updates {
        let manifest = ProductSourceFileFactsRecord::Manifest(update.manifest().clone());
        let manifest_key = update.manifest_key();
        if product_source_file_facts_record_key(manifest.file_key().unwrap_or([0; 32]), &manifest)
            .map_err(BuiltinModelError)?
            != manifest_key
        {
            return Err(BuiltinModelError(
                "source facts manifest key does not match its file".to_owned(),
            ));
        }
        insert_source_facts_after(&mut afters, manifest_key, manifest)?;
        for (key, record) in update.pages() {
            if product_source_file_facts_record_key(record.file_key().unwrap_or([0; 32]), record)
                .map_err(BuiltinModelError)?
                != *key
            {
                return Err(BuiltinModelError(
                    "source facts page key does not match its row".to_owned(),
                ));
            }
            insert_source_facts_after(&mut afters, *key, record.clone())?;
        }
    }

    let mut required_preserved = selected_files
        .iter()
        .filter(|(key, row)| {
            !afters.contains_key(key)
                && row
                    .file_fields()
                    .is_some_and(|file| file.source_identity.is_some())
        })
        .count();
    if let Some(facts_relation) = facts_relation {
        for file_key in previous_files.iter().copied() {
            let Some(ProductSourceFileFactsRecord::Manifest(manifest)) =
                facts_relation.lookup(&file_key).map_err(|error| {
                    BuiltinModelError(format!("read prior source facts manifest: {error}"))
                })?
            else {
                continue;
            };
            let source_record = source_relation
                .lookup(&file_key)
                .map_err(|error| {
                    BuiltinModelError(format!("read prior source row for facts cleanup: {error}"))
                })?
                .ok_or_else(|| {
                    BuiltinModelError(
                        "source facts manifest has no matching prior source row".to_owned(),
                    )
                })?;
            let file = source_record.file_fields().ok_or_else(|| {
                BuiltinModelError("source facts owner is not a source file".to_owned())
            })?;
            let retained = selected_files
                .binary_search_by_key(&file_key, |(key, _)| *key)
                .ok()
                .and_then(|index| selected_files.get(index));
            if !afters.contains_key(&file_key)
                && retained.is_some_and(|(_, next)| next == &source_record)
                && file.source_identity.is_some()
            {
                if !manifest.matches_file_identity(file) || manifest.file_key() != file_key {
                    return Err(BuiltinModelError(
                        "retained source facts do not bind the exact unchanged source row"
                            .to_owned(),
                    ));
                }
                required_preserved = required_preserved.checked_sub(1).ok_or_else(|| {
                    BuiltinModelError("duplicate prior source facts owner".to_owned())
                })?;
                // No page reads or copies: this immutable selected tree is
                // retained only for the exact same admitted source row. A
                // changed, unavailable or deleted file cannot use this path.
                continue;
            }
            let owned_keys = product_source_file_facts_row_keys(file, file_key, manifest, |key| {
                facts_relation
                    .lookup(key)
                    .map_err(|error| error.to_string())
            })
            .map_err(|error| {
                BuiltinModelError(format!("verify prior source facts tree: {error}"))
            })?;
            for key in owned_keys {
                afters.entry(key).or_insert(None);
            }
        }
    }

    if required_preserved != 0 {
        return Err(BuiltinModelError(
            "selected unchanged source has no exact complete facts manifest; rescan required"
                .to_owned(),
        ));
    }
    let mut changes = Vec::with_capacity(afters.len());
    for (key, after) in afters {
        let expected = facts_relation
            .map(|relation| relation.lookup(&key))
            .transpose()
            .map_err(|error| BuiltinModelError(format!("read source facts before value: {error}")))?
            .flatten();
        if expected != after {
            changes.push(BuiltinSourceFactsChange {
                key,
                expected,
                after,
            });
        }
    }
    Ok(changes)
}

fn insert_source_facts_after(
    afters: &mut BTreeMap<[u8; 32], Option<ProductSourceFileFactsRecord>>,
    key: [u8; 32],
    record: ProductSourceFileFactsRecord,
) -> Result<(), BuiltinModelError> {
    if afters
        .insert(key, Some(record.clone()))
        .is_some_and(|previous| previous.is_some_and(|previous| previous != record))
    {
        return Err(BuiltinModelError(
            "source facts updates contain a content-key collision".to_owned(),
        ));
    }
    Ok(())
}

fn prepare_source_capture_changes(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    context: &SemanticCompilationContext<'_>,
    profiles: &BTreeSet<LanguageProfile>,
    observations: &BTreeMap<LanguageProfile, SourceObservationReceipt>,
    source_version: [u8; 32],
    operation_key: Option<backend_library::IndexOperationKey>,
) -> Result<
    (
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
        Vec<BuiltinCaptureChange>,
        Vec<(ProductSemanticPublicationKey, SourceObservationReceipt)>,
    ),
    BuiltinModelError,
> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open semantic source-capture relation: {error}"))
        })?;
    let capture_relation = semantic_capture_relation(&snapshot).map_err(|error| {
        BuiltinModelError(format!("open prior semantic capture relation: {error}"))
    })?;
    let mut captures = BTreeMap::new();
    let mut changes = Vec::with_capacity(profiles.len());
    let mut receipts = Vec::with_capacity(profiles.len());
    let operation_key = operation_key.map(|key| key.to_bytes());
    for profile in profiles {
        let receipt = observations.get(profile).ok_or_else(|| {
            BuiltinModelError("semantic profile has no source observation receipt".to_owned())
        })?;
        let input_digest = receipt.observation().revision().ok_or_else(|| {
            BuiltinModelError("semantic source observation has no input digest".to_owned())
        })?;
        let source_count = match receipt.observation().value() {
            backend_extension_turso::SourceObservationValue::KnownCount(count) => *count,
            _ => {
                return Err(BuiltinModelError(
                    "semantic source observation is not a complete profile count".to_owned(),
                ));
            }
        };
        let coordinate = super::super::compiler_scope::semantic_coordinate(
            package,
            *profile,
            context.coordinate,
        )?;
        let key = ProductSemanticPublicationKey::new(
            context.package_reference.clone(),
            coordinate,
            *profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let capture = SemanticSourceCapture::new(
            operation_key,
            source_version,
            input_digest,
            receipt.sequence(),
            source_count,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let selected = relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read prior semantic source capture: {error}"))
        })?;
        let prior = selected.as_ref().and_then(|record| match record {
            ProductSemanticPublicationRecord::Published { coverage, claim } => {
                Some(SemanticPublicationVersion::new(*coverage, *claim))
            }
            ProductSemanticPublicationRecord::Unavailable(_) => None,
        });
        let expected = capture_relation
            .as_ref()
            .map(|relation| relation.lookup(&key))
            .transpose()
            .map_err(|error| {
                BuiltinModelError(format!("read prior semantic capture marker: {error}"))
            })?
            .flatten();
        changes.push(BuiltinCaptureChange {
            key: key.clone(),
            expected,
            capture,
            outcome: ProductSemanticCaptureOutcome::Pending { prior },
            compiler_failure: None,
        });
        captures.insert(key.clone(), capture);
        receipts.push((key, receipt.clone()));
    }
    Ok((captures, changes, receipts))
}

/// Reads back exactly the project and file rows selected by the current
/// workspace root after source capture has committed.
fn current_project_frontier(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    project_key: [u8; 32],
) -> Result<
    (
        ProductSourceRecord,
        Vec<[u8; 32]>,
        CapturedProjectFileFrontier,
    ),
    BuiltinModelError,
> {
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open committed source frontier: {error}")))?;
    let project = relation
        .lookup(&project_key)
        .map_err(|error| BuiltinModelError(format!("read committed project row: {error}")))?
        .ok_or_else(|| BuiltinModelError("committed project row is absent".to_owned()))?;
    let file_keys =
        super::super::profile::resolve_project_file_keys(project_key, &project, |page_key| {
            relation.lookup(page_key).map_err(|error| {
                BuiltinModelError(format!("read committed project membership page: {error}"))
            })
        })?;
    let rows = relation
        .lookup_many_sorted(&file_keys)
        .map_err(|error| BuiltinModelError(format!("read committed project files: {error}")))?;
    let frontier = CapturedProjectFileFrontier::capture(
        project_key,
        file_keys.len(),
        file_keys
            .iter()
            .copied()
            .zip(rows.iter())
            .map(|(key, record)| {
                record.as_ref().map(|record| (key, record)).ok_or_else(|| {
                    BuiltinModelError(
                        "committed project frontier refers to a missing source file".to_owned(),
                    )
                })
            }),
    )?;
    Ok((project, file_keys, frontier))
}

/// Reconstructs the exact source-capture receipt from the selected workspace
/// root. Recovery succeeds only when that root itself retains a profile marker
/// carrying the caller's operation key.
pub(in crate::builtin) fn source_capture_receipt_for_root(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
    operation_key: backend_library::IndexOperationKey,
    expected_request_identity: Option<[u8; 32]>,
) -> Result<Option<backend_library::IndexOperationSourceCaptureReceipt>, BuiltinModelError> {
    let owner = daemon.engine().daemon().owner();
    let snapshot = owner.snapshot();
    let Some(relation) = semantic_capture_relation(&snapshot).map_err(|error| {
        BuiltinModelError(format!("open selected source-capture relation: {error}"))
    })?
    else {
        return Ok(None);
    };
    let operation_key = operation_key.to_bytes();
    let mut profiles = Vec::new();
    let mut capture_basis = None;
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page selected source-capture relation: {error}"))
            })?;
        for (key, record) in page.entries() {
            if key.package() != package || !key.is_selected() {
                continue;
            }
            if record.operation_key() != Some(&operation_key)
                || expected_request_identity
                    .is_some_and(|expected| record.request_identity() != &expected)
            {
                continue;
            }
            let capture = record.capture();
            let state = match record.outcome() {
                ProductSemanticCaptureOutcome::Pending { prior } => {
                    backend_library::IndexOperationSemanticProfileState::Pending {
                        prior: prior.map(index_operation_prior_semantic),
                    }
                }
                ProductSemanticCaptureOutcome::Unavailable { reason } => {
                    backend_library::IndexOperationSemanticProfileState::Unavailable {
                        reason: index_operation_unavailable_reason(reason),
                    }
                }
                ProductSemanticCaptureOutcome::Failed { prior, reason } => {
                    backend_library::IndexOperationSemanticProfileState::Failed {
                        prior: index_operation_prior_semantic(prior),
                        reason: index_operation_unavailable_reason(reason),
                    }
                }
                ProductSemanticCaptureOutcome::Published { coverage, claim } => {
                    backend_library::IndexOperationSemanticProfileState::Published {
                        generation: *claim.binding().identity.as_ref(),
                        coverage: index_operation_coverage(coverage),
                    }
                }
            };
            let basis = (
                *record.source_commit(),
                *record.source_workspace_root(),
                record.source_workspace_sequence(),
                *record.request_identity(),
            );
            if capture_basis.is_some_and(|expected| expected != basis) {
                return Err(BuiltinModelError(
                    "semantic profile captures do not share one exact source commit".to_owned(),
                ));
            }
            capture_basis = Some(basis);
            profiles.push(backend_library::IndexOperationSourceProfile {
                profile: backend_library::SemanticLanguageProfile::new(key.profile()),
                source_version: *capture.source_version(),
                input_digest: *capture.input_digest(),
                observation_sequence: capture.observation_sequence(),
                source_count: capture.source_count(),
                state,
            });
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    profiles.sort_by_key(|profile| profile.profile);
    if profiles.is_empty() {
        return Ok(None);
    }
    let Some((commit_identity, workspace_root, workspace_sequence, _)) = capture_basis else {
        return Ok(None);
    };
    let receipt = backend_library::IndexOperationSourceCaptureReceipt::from_checked_parts(
        backend_library::IndexOperationKey::from_bytes(operation_key)
            .map_err(|error| BuiltinModelError(error.to_string()))?,
        commit_identity,
        workspace_root,
        workspace_sequence,
        profiles.into_boxed_slice(),
    )
    .map_err(|error| {
        BuiltinModelError(format!("admit selected source-capture receipt: {error}"))
    })?;
    Ok(Some(receipt))
}

fn index_operation_prior_semantic(
    version: SemanticPublicationVersion,
) -> backend_library::IndexOperationPriorSemantic {
    backend_library::IndexOperationPriorSemantic {
        generation: *version.claim().binding().identity.as_ref(),
        coverage: index_operation_coverage(version.coverage()),
    }
}

fn index_operation_coverage(
    coverage: SemanticPublicationCoverage,
) -> backend_library::IndexOperationSemanticCoverage {
    match coverage {
        SemanticPublicationCoverage::Complete => {
            backend_library::IndexOperationSemanticCoverage::Complete
        }
        SemanticPublicationCoverage::Partial(partial) => {
            backend_library::IndexOperationSemanticCoverage::Partial {
                completed: partial.completed().get(),
                total: partial.total().get(),
            }
        }
    }
}

fn index_operation_unavailable_reason(
    reason: backend_engine::builtin::SemanticUnavailableReason,
) -> backend_library::IndexOperationSemanticUnavailableReason {
    match reason {
        backend_engine::builtin::SemanticUnavailableReason::Toolchain => {
            backend_library::IndexOperationSemanticUnavailableReason::Toolchain
        }
        backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority => {
            backend_library::IndexOperationSemanticUnavailableReason::ProjectAuthority
        }
        backend_engine::builtin::SemanticUnavailableReason::Cancelled => {
            backend_library::IndexOperationSemanticUnavailableReason::Cancelled
        }
        backend_engine::builtin::SemanticUnavailableReason::Rejected => {
            backend_library::IndexOperationSemanticUnavailableReason::Rejected
        }
    }
}

/// Converts every still-Pending capture into an explicit refusal while
/// retaining its old coherent generation, when one existed.
fn terminal_capture_changes(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    captures: &BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    reason: backend_engine::builtin::SemanticUnavailableReason,
    compiler_failure: Option<(LanguageProfile, backend_library::PackageCompilerFailure)>,
) -> Result<Vec<BuiltinCaptureChange>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = semantic_capture_relation(&snapshot)
        .map_err(|error| BuiltinModelError(format!("open pending semantic captures: {error}")))?
        .ok_or_else(|| {
            BuiltinModelError("pending semantic capture relation disappeared".to_owned())
        })?;
    let mut changes = Vec::new();
    let mut failure_profiles = 0usize;
    for (key, capture) in captures {
        let current = relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read pending semantic capture: {error}"))
        })?;
        let Some(current) = current else {
            return Err(BuiltinModelError(
                "semantic capture disappeared before terminal refusal".to_owned(),
            ));
        };
        let outcome = match current.outcome() {
            ProductSemanticCaptureOutcome::Pending { prior } if current.capture() == *capture => {
                match prior {
                    Some(prior) => ProductSemanticCaptureOutcome::Failed { prior, reason },
                    None => ProductSemanticCaptureOutcome::Unavailable { reason },
                }
            }
            _ => {
                return Err(BuiltinModelError(
                    "semantic capture changed before terminal refusal".to_owned(),
                ));
            }
        };
        changes.push(BuiltinCaptureChange {
            key: key.clone(),
            expected: Some(current),
            capture: *capture,
            outcome,
            compiler_failure: compiler_failure
                .as_ref()
                .filter(|(profile, _)| *profile == key.profile())
                .map(|(_, failure)| {
                    failure_profiles += 1;
                    failure.clone()
                }),
        });
    }
    if compiler_failure.is_some() && failure_profiles != 1 {
        return Err(BuiltinModelError(
            "typed compiler refusal did not match exactly one captured profile".to_owned(),
        ));
    }
    Ok(changes)
}

fn completed_capture_changes(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    captures: &BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    semantic_changes: &[BuiltinSemanticChange],
) -> Result<Vec<BuiltinCaptureChange>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = semantic_capture_relation(&snapshot)
        .map_err(|error| BuiltinModelError(format!("open completed semantic captures: {error}")))?
        .ok_or_else(|| BuiltinModelError("semantic capture relation disappeared".to_owned()))?;
    let mut changes = Vec::with_capacity(captures.len());
    for (key, capture) in captures {
        let expected = relation
            .lookup(key)
            .map_err(|error| {
                BuiltinModelError(format!("read completed semantic capture: {error}"))
            })?
            .ok_or_else(|| {
                BuiltinModelError("semantic capture disappeared before completion".to_owned())
            })?;
        let ProductSemanticCaptureOutcome::Pending { prior } = expected.outcome() else {
            return Err(BuiltinModelError(
                "semantic capture is no longer pending at completion".to_owned(),
            ));
        };
        if expected.capture() != *capture {
            return Err(BuiltinModelError(
                "semantic capture input changed before completion".to_owned(),
            ));
        }
        let publication = semantic_changes
            .iter()
            .find(|change| change.key == *key)
            .and_then(|change| change.after.as_ref());
        let outcome = match publication {
            Some(ProductSemanticPublicationRecord::Published { coverage, claim }) => {
                ProductSemanticCaptureOutcome::Published {
                    coverage: *coverage,
                    claim: *claim,
                }
            }
            Some(ProductSemanticPublicationRecord::Unavailable(reason)) => match prior {
                Some(prior) => ProductSemanticCaptureOutcome::Failed {
                    prior,
                    reason: *reason,
                },
                None => ProductSemanticCaptureOutcome::Unavailable { reason: *reason },
            },
            None => {
                let reason = if capture.source_count() == 0 {
                    backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority
                } else {
                    backend_engine::builtin::SemanticUnavailableReason::Rejected
                };
                match prior {
                    Some(prior) => ProductSemanticCaptureOutcome::Failed { prior, reason },
                    None => ProductSemanticCaptureOutcome::Unavailable { reason },
                }
            }
        };
        changes.push(BuiltinCaptureChange {
            key: key.clone(),
            expected: Some(expected),
            capture: *capture,
            outcome,
            compiler_failure: None,
        });
    }
    Ok(changes)
}

fn commit_semantic_terminal(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    capture_changes: Vec<BuiltinCaptureChange>,
) -> Result<(), BuiltinModelError> {
    if capture_changes.is_empty() {
        return Ok(());
    }
    let operation_key = capture_changes
        .iter()
        .find_map(|change| change.capture.operation_key().copied());
    if capture_changes.iter().any(|change| {
        change
            .capture
            .operation_key()
            .is_some_and(|key| Some(*key) != operation_key)
    }) {
        return Err(BuiltinModelError(
            "semantic terminal update spans multiple operation keys".to_owned(),
        ));
    }
    let intent =
        BuiltinIntent::index_with_capture(package, label, Vec::new(), Vec::new(), capture_changes)?;
    let intent = match operation_key {
        Some(bytes) => intent.with_operation_key(
            backend_library::IndexOperationKey::from_bytes(bytes)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        )?,
        None => intent,
    };
    super::adapter::commit_builtin_intent(daemon, request_id, &intent)
}

pub(super) fn commit_pending_capture_failure(
    daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    request_id: u64,
    captures: &BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    reason: backend_engine::builtin::SemanticUnavailableReason,
    compiler_failure: Option<(LanguageProfile, backend_library::PackageCompilerFailure)>,
) -> Result<(), BuiltinModelError> {
    let changes = terminal_capture_changes(daemon, captures, reason, compiler_failure)?;
    commit_semantic_terminal(daemon, package, label, request_id, changes)
}

fn staged_cargo_alias_evidence(
    profile: LanguageProfile,
    source_observation_revision: [u8; 32],
    source_root: &Path,
    staged: Option<&StagedSemanticPackage>,
) -> Result<Option<CargoPackageAliasEvidenceV1>, BuiltinModelError> {
    if !matches!(profile, LanguageProfile::Rust(_)) {
        return Ok(None);
    }
    let facts = if let Some(staged) = staged {
        if staged.profile() != profile {
            return Err(BuiltinModelError(
                "staged Cargo facts belong to another language profile".to_owned(),
            ));
        }
        staged.cargo_workspace_facts()
    } else {
        None
    };
    let manifest = source_root.join("Cargo.toml");
    let evidence = CargoPackageAliasEvidenceV1::from_workspace_facts(
        profile,
        source_observation_revision,
        &manifest,
        facts,
    )
    .map_err(|_| BuiltinModelError("Cargo package aliases failed bounded admission".to_owned()))?;
    Ok(Some(evidence))
}

/// Owner-captured immutable inputs for one filesystem scan.
pub(super) struct IndexScanWork {
    package: backend_engine::PackageKey,
    label: String,
    source_root: PathBuf,
    coordinate: Option<PackageUrl>,
    request_id: u64,
    execution_intent: CompileExecutionIntent,
    project_key: [u8; 32],
    workspace_root: WorkspaceRoot,
    before: Option<ProductSourceRecord>,
    old_files: Vec<[u8; 32]>,
    reusable: BTreeMap<[u8; 32], ProductSourceRecord>,
    capture_workspace_snapshot: bool,
    cancellation: Arc<AtomicBool>,
}

/// Worker scan outcome, distinguishing an explicit job cancellation from an
/// invalid or unreadable project.
pub(super) enum IndexScanFailure {
    Cancelled,
    Refused(BuiltinModelError),
}

impl IndexScanFailure {
    fn into_model_error(self) -> BuiltinModelError {
        match self {
            Self::Cancelled => BuiltinModelError("index scan was cancelled".to_owned()),
            Self::Refused(error) => error,
        }
    }
}

/// Filesystem scan output paired with the owner state it was captured for.
pub(super) struct IndexScanResult {
    work: IndexScanWork,
    scan: ingest::IndexSnapshot,
    workspace_snapshot: Option<ingest::CompilerWorkspaceSnapshot>,
}

/// The complete candidate product transaction. Its intent is the one durable
/// source-plus-semantic selection marker; `selected` advances serving only
/// after that intent commits.
pub(super) struct PreparedProductSelection {
    pub(super) intent: Option<BuiltinIntent>,
    pub(super) selected: Vec<(ProductSemanticPublicationKey, SemanticPublicationClaim)>,
    /// Source/configuration frontier validated immediately before the one
    /// durable product marker commit.
    pub(super) revision_fence: Option<ingest::CompilerRevisionFence>,
}

impl PreparedProductSelection {
    fn into_option(self) -> Option<Self> {
        (self.intent.is_some() || !self.selected.is_empty()).then_some(self)
    }
}

/// What preparing an index job came to.
pub(super) enum PreparedIndex {
    /// All required planes were admitted; the transaction is ready to commit.
    Ready(PreparedProductSelection),
    /// A compile to run off the owner loop, then publish on it.
    Compile(DeferredIndex),
}

/// The compile an index job hands off the owner loop, and everything its
/// publication needs afterwards. Immutable candidates may be retained by the
/// authority, while source changes and serving selections remain private until
/// the combined workspace marker commits.
pub(super) struct DeferredIndex {
    package: backend_engine::PackageKey,
    label: String,
    project_key: [u8; 32],
    project_record: ProductSourceRecord,
    prior_project: Option<ProductSourceRecord>,
    file_keys: Vec<[u8; 32]>,
    prior_file_frontier: CapturedProjectFileFrontier,
    source_root: PathBuf,
    source_changes: Vec<BuiltinSourceChange>,
    revision_fence: ingest::CompilerRevisionFence,
    profiles: VecDeque<DeferredProfile>,
    expected_profiles: usize,
    completed_profiles: usize,
    pub(super) captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    semantic_changes: Vec<BuiltinSemanticChange>,
    selected: Vec<(ProductSemanticPublicationKey, SemanticPublicationClaim)>,
    cargo_alias_observations: BTreeMap<LanguageProfile, CargoPackageAliasEvidenceV1>,
}

/// Compact witness for the exact source-file rows selected by one prior
/// project frontier. Deferred indexing retains this digest rather than
/// cloning every file record across profile compilation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CapturedProjectFileFrontier([u8; 32]);

impl CapturedProjectFileFrontier {
    fn capture<'row>(
        project_key: [u8; 32],
        file_count: usize,
        rows: impl IntoIterator<Item = Result<([u8; 32], &'row ProductSourceRecord), BuiltinModelError>>,
    ) -> Result<Self, BuiltinModelError> {
        let encoded_count = u64::try_from(file_count)
            .map_err(|_| BuiltinModelError("project source file count exceeds u64".to_owned()))?;
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.local.deferred-project-file-frontier.v1\0");
        hash.update(&project_key);
        hash.update(&encoded_count.to_be_bytes());

        let mut observed = 0usize;
        let mut encoded = Vec::new();
        for row in rows {
            let (key, record) = row?;
            super::super::profile::validate_project_file(project_key, key, record)?;
            encoded.clear();
            <BuiltinWorkspaceRelation as backend_engine::Relation>::encode_value(
                record,
                &mut encoded,
            );
            let encoded_len = u64::try_from(encoded.len()).map_err(|_| {
                BuiltinModelError("project source file row length exceeds u64".to_owned())
            })?;
            hash.update(&key);
            hash.update(&encoded_len.to_be_bytes());
            hash.update(&encoded);
            observed = observed.checked_add(1).ok_or_else(|| {
                BuiltinModelError("project source file count overflows usize".to_owned())
            })?;
        }
        if observed != file_count {
            return Err(BuiltinModelError(
                "project source file rows do not match their captured count".to_owned(),
            ));
        }
        Ok(Self(*hash.finalize().as_bytes()))
    }

    fn require_unchanged(self, current: Self) -> Result<(), BuiltinModelError> {
        if self == current {
            Ok(())
        } else {
            Err(BuiltinModelError(
                "project source files changed while deferred profiles were compiling; retry indexing"
                    .to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod captured_project_file_frontier_tests {
    use super::{BuiltinModelError, CapturedProjectFileFrontier, ProductSourceRecord};
    use backend_engine::{
        SourceLanguage, SourceUnavailableReason, package_key, product_source_file_key,
    };
    use std::collections::BTreeMap;

    fn unavailable_file(
        project: [u8; 32],
        reason: SourceUnavailableReason,
    ) -> Result<ProductSourceRecord, BuiltinModelError> {
        ProductSourceRecord::file_unavailable(
            project,
            "src/lib.rs",
            SourceLanguage::Rust,
            [7; 32],
            reason,
        )
        .map_err(BuiltinModelError)
    }

    fn capture_one(
        project: [u8; 32],
        key: [u8; 32],
        record: &ProductSourceRecord,
    ) -> Result<CapturedProjectFileFrontier, BuiltinModelError> {
        CapturedProjectFileFrontier::capture(
            project,
            1,
            std::iter::once(Ok::<_, BuiltinModelError>((key, record))),
        )
    }

    fn capture_project_frontier(
        project_key: [u8; 32],
        project: &ProductSourceRecord,
        relation_rows: &BTreeMap<[u8; 32], ProductSourceRecord>,
    ) -> Result<CapturedProjectFileFrontier, BuiltinModelError> {
        let file_keys = super::super::super::profile::resolve_project_file_keys(
            project_key,
            project,
            |page_key| Ok(relation_rows.get(page_key).cloned()),
        )?;
        let rows = file_keys.iter().map(|key| {
            relation_rows
                .get(key)
                .map(|record| (*key, record))
                .ok_or_else(|| {
                    BuiltinModelError("project frontier refers to a missing source file".to_owned())
                })
        });
        CapturedProjectFileFrontier::capture(project_key, file_keys.len(), rows)
    }

    #[test]
    fn a_changed_unavailable_reason_invalidates_the_deferred_file_frontier()
    -> Result<(), BuiltinModelError> {
        let project = package_key("pkg:deferred-frontier").to_bytes();
        let key = product_source_file_key(project, "src/lib.rs");
        let captured = unavailable_file(project, SourceUnavailableReason::NotText)?;
        let current = unavailable_file(project, SourceUnavailableReason::Unreadable)?;

        let before = capture_one(project, key, &captured)?;
        let after = capture_one(project, key, &current)?;
        let prior_project =
            ProductSourceRecord::project("pkg:deferred-frontier", [6; 32], vec![key])
                .map_err(BuiltinModelError)?;
        let current_project =
            ProductSourceRecord::project("pkg:deferred-frontier", [6; 32], vec![key])
                .map_err(BuiltinModelError)?;
        assert_eq!(prior_project, current_project);
        assert_ne!(before, after);
        let error = match before.require_unchanged(after) {
            Err(error) => error,
            Ok(()) => {
                return Err(BuiltinModelError(
                    "deferred work accepted a newer unavailable file status".to_owned(),
                ));
            }
        };
        assert!(error.to_string().contains("project source files changed"));
        Ok(())
    }

    #[test]
    fn unchanged_prior_file_rows_remain_admissible() -> Result<(), BuiltinModelError> {
        let project = package_key("pkg:deferred-frontier").to_bytes();
        let key = product_source_file_key(project, "src/lib.rs");
        let row = unavailable_file(project, SourceUnavailableReason::NotText)?;

        let captured = capture_one(project, key, &row)?;
        let current = capture_one(project, key, &row)?;
        captured.require_unchanged(current)?;
        Ok(())
    }

    #[test]
    fn an_unrelated_project_publication_does_not_change_the_captured_frontier()
    -> Result<(), BuiltinModelError> {
        let project = package_key("pkg:deferred-frontier").to_bytes();
        let key = product_source_file_key(project, "src/lib.rs");
        let row = unavailable_file(project, SourceUnavailableReason::NotText)?;
        let project_record =
            ProductSourceRecord::project("pkg:deferred-frontier", [6; 32], vec![key])
                .map_err(BuiltinModelError)?;
        let mut relation_rows = BTreeMap::from([(project, project_record.clone()), (key, row)]);
        let captured = capture_project_frontier(project, &project_record, &relation_rows)?;

        let unrelated_label = "pkg:unrelated-frontier";
        let unrelated_project = package_key(unrelated_label).to_bytes();
        let unrelated_file_key = product_source_file_key(unrelated_project, "src/other.rs");
        let unrelated_file = ProductSourceRecord::file(
            unrelated_project,
            "src/other.rs",
            SourceLanguage::Rust,
            [8; 32],
            [9; 32],
            Vec::<backend_engine::SourceDeclaration>::new(),
        )
        .map_err(BuiltinModelError)?;
        let unrelated_project_record =
            ProductSourceRecord::project(unrelated_label, [10; 32], vec![unrelated_file_key])
                .map_err(BuiltinModelError)?;
        relation_rows.insert(unrelated_project, unrelated_project_record);
        relation_rows.insert(unrelated_file_key, unrelated_file);
        let actual = capture_project_frontier(project, &project_record, &relation_rows)?;

        captured.require_unchanged(actual)?;
        Ok(())
    }
}

struct DeferredProfile {
    key: ProductSemanticPublicationKey,
    expected_artifacts: u32,
    attempt: backend_extension_turso::CandidateAttempt,
    sources: OwnedPackageSourceSet,
}

pub(super) struct DeferredProfileTicket {
    key: ProductSemanticPublicationKey,
    expected_artifacts: u32,
    attempt: backend_extension_turso::CandidateAttempt,
    ordinal: u16,
    total: u16,
}

impl DeferredProfileTicket {
    #[must_use]
    pub(super) fn candidate_attempt(&self) -> &backend_extension_turso::CandidateAttempt {
        &self.attempt
    }

    #[must_use]
    pub(super) const fn profile(&self) -> LanguageProfile {
        self.key.profile()
    }

    #[must_use]
    pub(super) const fn ordinal(&self) -> u16 {
        self.ordinal
    }

    #[must_use]
    pub(super) const fn total(&self) -> u16 {
        self.total
    }
}

impl DeferredIndex {
    /// Takes one profile so its staged output can be admitted and dropped
    /// before the next profile consumes compiler output credits.
    pub(super) fn take_next_work(
        &mut self,
    ) -> Option<(DeferredProfileTicket, OwnedPackageSourceSet)> {
        let total = u16::try_from(self.expected_profiles).ok()?;
        let ordinal = u16::try_from(
            self.expected_profiles
                .checked_sub(self.profiles.len())?
                .checked_add(1)?,
        )
        .ok()?;
        self.profiles.pop_front().map(|profile| {
            (
                DeferredProfileTicket {
                    key: profile.key,
                    expected_artifacts: profile.expected_artifacts,
                    attempt: profile.attempt,
                    ordinal,
                    total,
                },
                profile.sources,
            )
        })
    }

    pub(super) fn has_pending_profiles(&self) -> bool {
        !self.profiles.is_empty()
    }

    pub(super) fn pending_attempts(&self) -> Vec<backend_extension_turso::CandidateAttempt> {
        self.profiles
            .iter()
            .map(|profile| profile.attempt.clone())
            .collect()
    }
}

/// Begins the local compile of every profile the sources name, exactly as
/// the in-place local route does (`compile_semantic_publications` with no
/// compiler cluster), stopping short of the compile itself.
fn prepare_deferred_compile(
    package: backend_engine::PackageKey,
    label: &str,
    project_key: [u8; 32],
    project_record: ProductSourceRecord,
    prior_project: Option<ProductSourceRecord>,
    file_keys: Vec<[u8; 32]>,
    prior_file_frontier: CapturedProjectFileFrontier,
    source_changes: Vec<BuiltinSourceChange>,
    context: &SemanticCompilationContext<'_>,
    sources: Vec<ingest::CompilerSource>,
    revision_fence: ingest::CompilerRevisionFence,
    observations: &BTreeMap<LanguageProfile, SourceObservationReceipt>,
    captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<DeferredIndex, BuiltinModelError> {
    let mut by_profile = BTreeMap::<LanguageProfile, Vec<OwnedPackageSource>>::new();
    for source in sources {
        let profile = compile_profile(context.source_root, &source);
        by_profile.entry(profile).or_default().push(
            OwnedPackageSource::from_string(&source.relative_path, source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        );
    }
    let mut profiles = Vec::with_capacity(by_profile.len());
    for (profile, sources) in by_profile {
        let mut attempt_for_error = None;
        let prepared = (|| {
            let expected_artifacts = u32::try_from(sources.len())
                .map_err(|_| BuiltinModelError("semantic source count exceeds u32".to_owned()))?;
            let coordinate = super::super::compiler_scope::semantic_coordinate(
                context.package,
                profile,
                context.coordinate,
            )?;
            let request = PackageCompileRequest::new(
                GenerateTarget {
                    correlation: context.correlation,
                    profile,
                    stage: backend_semantic::vocabulary::Stage::LowerIr,
                },
                coordinate.clone(),
            )
            .map_err(|error| BuiltinModelError(format!("semantic package profile: {error:?}")))?;
            let key = ProductSemanticPublicationKey::new(
                context.package_reference.clone(),
                coordinate,
                profile,
            )
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
            let scan_observation = observations.get(&profile).ok_or_else(|| {
                BuiltinModelError(
                    "semantic compilation has no matching source observation".to_owned(),
                )
            })?;
            let scan_input_digest = scan_observation.observation().revision().ok_or_else(|| {
                BuiltinModelError("semantic compilation observation has no input digest".to_owned())
            })?;
            let local_observation = semantic_authority.observe(
                &key,
                scan_input_digest,
                u64::from(expected_artifacts),
            )?;
            let attempt = semantic_authority.begin_candidate_attempt(&key, &local_observation)?;
            attempt_for_error = Some(attempt.clone());
            let input_claim = SemanticInputWitness::claimed_state(
                scan_input_digest,
                ScopeRoot::from_bytes(scan_input_digest),
                Coverage::Partial,
            );
            let sources = OwnedPackageSourceSet::new(
                request,
                context.source_root.to_path_buf(),
                sources.into_boxed_slice(),
            )
            .map_err(|error| BuiltinModelError(error.to_string()))?
            .with_input_claim(input_claim);
            Ok::<_, BuiltinModelError>(DeferredProfile {
                key,
                expected_artifacts,
                attempt,
                sources,
            })
        })();
        match prepared {
            Ok(profile) => profiles.push(profile),
            Err(error) => {
                let mut attempts = profiles
                    .drain(..)
                    .map(|profile| profile.attempt)
                    .collect::<Vec<_>>();
                attempts.extend(attempt_for_error);
                let mut cleanup_failure = None;
                for attempt in attempts {
                    if let Err(retire_error) = semantic_authority.retire_candidate_attempt(
                        &attempt,
                        backend_extension_turso::CandidateAttemptRetirementReason::Refused,
                    ) {
                        cleanup_failure.get_or_insert(retire_error);
                    }
                }
                return Err(match cleanup_failure {
                    Some(cleanup_failure) => BuiltinModelError(format!(
                        "{error}; retire unprepared profile attempts failed: {cleanup_failure}"
                    )),
                    None => error,
                });
            }
        }
    }
    let expected_profiles = profiles.len();
    Ok(DeferredIndex {
        package,
        label: label.to_owned(),
        project_key,
        project_record,
        prior_project,
        file_keys,
        prior_file_frontier,
        source_root: context.source_root.to_path_buf(),
        source_changes,
        revision_fence,
        profiles: profiles.into(),
        expected_profiles,
        completed_profiles: 0,
        captures,
        semantic_changes: Vec::with_capacity(expected_profiles.saturating_mul(2)),
        selected: Vec::with_capacity(expected_profiles),
        cargo_alias_observations: BTreeMap::new(),
    })
}

/// Compiles one deferred profile off the owner loop. The returned package
/// retains its compiler credit lease only until the owner admits this result.
pub(super) fn run_deferred_compile(
    compiler: &LocalCompilerClient,
    sources: OwnedPackageSourceSet,
    cancelled: Arc<AtomicBool>,
) -> Result<StagedSemanticPackage, PackageSemanticRuntimeError> {
    compiler.compile_package_sources_staged_cancellable(sources, cancelled)
}

pub(super) fn deferred_compile_was_cancelled(
    result: &Result<StagedSemanticPackage, PackageSemanticRuntimeError>,
) -> bool {
    match result {
        Err(PackageSemanticRuntimeError::Runtime(CompilerTerminal::Runtime {
            cause: CompilerRuntimeCause::RequestCancelled,
            ..
        })) => true,
        Err(PackageSemanticRuntimeError::Package(PackageSemanticError::Compile {
            terminal,
            ..
        })) => matches!(terminal.as_ref(), CompilerTerminal::PackageCancelled { .. }),
        _ => false,
    }
}

/// A deferred profile refusal with an optional closed compiler summary for
/// package-fragment terminals.
#[derive(Debug)]
pub(super) struct DeferredProfileFailure {
    pub(super) detail: BuiltinModelError,
    pub(super) compiler_failure: Option<PackageCompilerFailure>,
}

impl From<BuiltinModelError> for DeferredProfileFailure {
    fn from(detail: BuiltinModelError) -> Self {
        Self {
            detail,
            compiler_failure: None,
        }
    }
}

fn typed_package_compiler_failure(
    compiled: &Result<StagedSemanticPackage, PackageSemanticRuntimeError>,
) -> Result<Option<PackageCompilerFailure>, BuiltinModelError> {
    let Err(PackageSemanticRuntimeError::Package(PackageSemanticError::Compile { path, terminal })) =
        compiled
    else {
        return Ok(None);
    };
    PackageCompilerFailure::from_package_terminal(path, terminal).map_err(|error| {
        BuiltinModelError(format!("compiler failure projection was rejected: {error}"))
    })
}

/// Admits exactly one profile candidate on the owner loop and then drops its
/// staged output, releasing the package compiler's bounded output credits.
/// The serving selector remains untouched until every profile has succeeded.
pub(super) fn finish_deferred_profile(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    job: &mut DeferredIndex,
    profile: DeferredProfileTicket,
    compiled: Result<StagedSemanticPackage, PackageSemanticRuntimeError>,
) -> Result<(), DeferredProfileFailure> {
    if job.completed_profiles >= job.expected_profiles {
        return Err(BuiltinModelError(
            "the deferred compile answered more profiles than requested; prior selected semantic generation was preserved"
                .to_owned(),
        )
        .into());
    }
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let compiler_failure = typed_package_compiler_failure(&compiled)?;
    let (staged, publication_coverage) =
        match admit_local_compile(compiled, profile.expected_artifacts) {
            Ok(admitted) => admitted,
            Err(error) => {
                semantic_authority.retire_candidate_attempt(
                    &profile.attempt,
                    backend_extension_turso::CandidateAttemptRetirementReason::Refused,
                )?;
                return Err(DeferredProfileFailure {
                    detail: error,
                    compiler_failure,
                });
            }
        };
    let cargo_alias_evidence = match staged_cargo_alias_evidence(
        profile.profile(),
        *profile.attempt.input_digest(),
        &job.source_root,
        Some(&staged),
    ) {
        Ok(evidence) => evidence,
        Err(error) => {
            semantic_authority.retire_candidate_attempt(
                &profile.attempt,
                backend_extension_turso::CandidateAttemptRetirementReason::Refused,
            )?;
            return Err(error.into());
        }
    };
    // Keep the ticket's exact capability until the publication result is
    // known. `publish_staged` consumes its copy when constructing a candidate,
    // but any refusal before Turso selects that candidate still needs a
    // durable terminal transition.
    let (claim, _selected) = match publish_local_compile(
        semantic_authority,
        &profile.key,
        profile.attempt.clone(),
        &staged,
        &job.revision_fence,
    ) {
        Ok(selected) => selected,
        Err(error) => {
            semantic_authority.retire_candidate_attempt(
                &profile.attempt,
                backend_extension_turso::CandidateAttemptRetirementReason::Refused,
            )?;
            return Err(error.into());
        }
    };
    record_semantic_publication(
        &relation,
        profile.key.clone(),
        publication_coverage,
        claim,
        &mut job.semantic_changes,
    )?;
    let selected_profile = profile.profile();
    job.selected.push((profile.key, claim));
    if let Some(evidence) = cargo_alias_evidence {
        job.cargo_alias_observations
            .insert(selected_profile, evidence);
    }
    job.completed_profiles += 1;
    // `staged` owns the compiler output credit lease. It is dropped here,
    // before the adapter asks the compiler to start the next profile.
    drop(staged);
    Ok(())
}

/// Builds the final source-plus-semantic intent after all profile candidates
/// have been admitted. The caller commits this one intent before advancing
/// the process-local serving selector.
pub(super) fn finish_deferred_index(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    mut job: DeferredIndex,
) -> Result<PreparedProductSelection, BuiltinModelError> {
    if !job.profiles.is_empty() || job.completed_profiles != job.expected_profiles {
        return Err(BuiltinModelError(
            "the deferred compile did not answer every profile; prior selected semantic generation was preserved"
            .to_owned(),
        ));
    }
    let capture_changes = completed_capture_changes(daemon, &job.captures, &job.semantic_changes)?;
    let owner = daemon.engine().daemon().owner();
    let relation = owner
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open deferred project source: {error}")))?;
    let prior_project = relation
        .lookup(&job.project_key)
        .map_err(|error| BuiltinModelError(format!("read deferred prior project row: {error}")))?;
    // The root record commits source_version, aliases, and the exact ordered
    // inline keys or content-addressed page keys. Comparing this package row
    // lets unrelated projects advance while refusing a stale package scan.
    if prior_project != job.prior_project {
        return Err(BuiltinModelError(
            "project source changed while deferred profiles were compiling; retry indexing"
                .to_owned(),
        ));
    }
    let prior_file_keys = match prior_project.as_ref() {
        Some(record) => {
            super::super::profile::resolve_project_file_keys(job.project_key, record, |page_key| {
                relation.lookup(page_key).map_err(|error| {
                    BuiltinModelError(format!("read deferred project membership page: {error}"))
                })
            })?
        }
        None => Vec::new(),
    };
    let prior_file_rows = relation
        .lookup_many_sorted(&prior_file_keys)
        .map_err(|error| BuiltinModelError(format!("read deferred project files: {error}")))?;
    let current_file_frontier = CapturedProjectFileFrontier::capture(
        job.project_key,
        prior_file_keys.len(),
        prior_file_keys
            .iter()
            .copied()
            .zip(prior_file_rows.iter())
            .map(|(key, record)| {
                record.as_ref().map(|record| (key, record)).ok_or_else(|| {
                    BuiltinModelError(
                        "deferred project frontier refers to a missing source file".to_owned(),
                    )
                })
            }),
    )?;
    job.prior_file_frontier
        .require_unchanged(current_file_frontier)?;
    replace_project_cargo_aliases(
        &mut job.source_changes,
        job.project_key,
        job.project_record,
        job.file_keys,
        prior_project.as_ref(),
        |key| {
            relation.lookup(key).map_err(|error| {
                BuiltinModelError(format!("read deferred project membership page: {error}"))
            })
        },
        std::mem::take(&mut job.cargo_alias_observations)
            .into_values()
            .collect(),
    )?;
    let intent = if job.source_changes.is_empty()
        && job.semantic_changes.is_empty()
        && capture_changes.is_empty()
    {
        None
    } else {
        let intent = if capture_changes.is_empty() {
            BuiltinIntent::index_with_semantics(
                job.package,
                &job.label,
                job.source_changes,
                job.semantic_changes,
            )?
        } else {
            BuiltinIntent::index_with_capture(
                job.package,
                &job.label,
                job.source_changes,
                job.semantic_changes,
                capture_changes,
            )?
        };
        Some(intent)
    };
    Ok(PreparedProductSelection {
        intent,
        selected: job.selected,
        revision_fence: Some(job.revision_fence),
    })
}

struct SemanticCompilationContext<'request> {
    package: backend_engine::PackageKey,
    package_reference: backend_engine::PackageReference,
    source_root: &'request Path,
    coordinate: Option<&'request PackageUrl>,
    correlation: CorrelationId,
    compiler: &'request LocalCompilerClient,
}

#[derive(Clone, Copy)]
struct CompilerInputAdmission {
    lineage: Option<CompilerSessionLineage>,
    read_set_completeness: ReadSetCompleteness,
}

#[derive(Clone, Copy)]
enum ReadSetCompleteness {
    /// No complete present-and-negative authority read set is available for
    /// this compiler lane. Dynamic files, negative path lookups, environment
    /// reads, generated inputs, and other external state remain uncertified;
    /// the observed-input digest cannot authorize reuse.
    Unproven,
    /// Exact authority-reported present and negative reads. Only this state
    /// can authorize the exact witness once a lane exposes it.
    Complete(ExactInputWitness),
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct ConfigurationObservation {
    digest: [u8; 32],
    complete: bool,
}

fn compiler_input_admissions(
    source_root: &Path,
    scan: &ingest::IndexSnapshot,
    compiler: &LocalCompilerClient,
) -> BTreeMap<LanguageProfile, CompilerInputAdmission> {
    let mut profiles = BTreeSet::new();
    for source in &scan.compiler_sources {
        profiles.insert(ingest::compilation_profile(
            source_root,
            &source.relative_path,
            source.profile,
        ));
    }
    for source in &scan.reused_compiler_files {
        profiles.insert(ingest::compilation_profile(
            source_root,
            &source.relative_path,
            source.profile,
        ));
    }

    let capabilities = compiler.capabilities();
    profiles
        .into_iter()
        .map(|profile| {
            // LocalCompiler currently exposes no complete present-and-negative
            // read set for any profile. Keep the typed state at the scheduler
            // boundary so proven lanes can opt in incrementally.
            (
                profile,
                CompilerInputAdmission {
                    lineage: capabilities.for_profile(profile).session_lineage(),
                    read_set_completeness: ReadSetCompleteness::Unproven,
                },
            )
        })
        .collect()
}

fn compiler_exact_witness_is_current(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    coordinate: Option<&PackageUrl>,
    compiler: &LocalCompilerClient,
    source_root: &Path,
    profile: LanguageProfile,
    admission: CompilerInputAdmission,
) -> Result<bool, BuiltinModelError> {
    let ReadSetCompleteness::Complete(witness) = admission.read_set_completeness else {
        return Ok(false);
    };
    let Some(lineage) = admission.lineage else {
        return Ok(false);
    };
    let Some(binding) = selected_semantic_binding(daemon, package, label, coordinate, profile)?
    else {
        return Ok(false);
    };
    Ok(compiler.exact_input_witness_is_current(source_root, profile, lineage, witness, binding))
}

fn profiles_requiring_compilation(
    live: &BTreeSet<LanguageProfile>,
    lost: &BTreeSet<LanguageProfile>,
    fresh: &BTreeSet<LanguageProfile>,
    inputs: &BTreeMap<LanguageProfile, CompilerInputAdmission>,
    exact_witnesses_current: &BTreeSet<LanguageProfile>,
) -> BTreeSet<LanguageProfile> {
    let mut dirty = lost.iter().chain(fresh).copied().collect::<BTreeSet<_>>();
    for profile in live {
        let complete_read_set = inputs.get(profile).is_some_and(|admission| {
            matches!(
                admission.read_set_completeness,
                ReadSetCompleteness::Complete(_)
            )
        });
        if !complete_read_set || !exact_witnesses_current.contains(profile) {
            dirty.insert(*profile);
        }
    }
    dirty
}

#[cfg(test)]
fn observe_compiler_configuration(
    snapshot: &ingest::CompilerConfigurationSnapshot,
) -> BTreeMap<Language, ConfigurationObservation> {
    let mut hashes = [
        Language::Rust,
        Language::TypeScript,
        Language::Python,
        Language::Go,
        Language::Java,
        Language::CSharp,
        Language::Clang,
    ]
    .into_iter()
    .map(|language| {
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.local.compiler-configuration.v2\0");
        hash.update(&[u8::from(language.native_tool())]);
        (
            language,
            (hash, snapshot.complete_languages.contains(&language)),
        )
    })
    .collect::<BTreeMap<_, _>>();
    for file in &snapshot.files {
        let Some((hash, _)) = hashes.get_mut(&file.language) else {
            continue;
        };
        let path = file.relative_path.as_os_str().as_encoded_bytes();
        hash.update(&(path.len() as u64).to_be_bytes());
        hash.update(path);
        hash.update(&file.content);
    }
    hashes
        .into_iter()
        .map(|(language, (hash, complete))| {
            (
                language,
                ConfigurationObservation {
                    digest: *hash.finalize().as_bytes(),
                    complete,
                },
            )
        })
        .collect()
}

fn selected_semantic_binding(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    coordinate: Option<&PackageUrl>,
    profile: LanguageProfile,
) -> Result<Option<[u8; 32]>, BuiltinModelError> {
    let package_reference = backend_engine::PackageReference::parse(label.to_owned())
        .map_err(|error| BuiltinModelError(format!("semantic package reference: {error:?}")))?;
    let coordinate =
        super::super::compiler_scope::semantic_coordinate(package, profile, coordinate)?;
    let key = ProductSemanticPublicationKey::new(package_reference, coordinate, profile)
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| {
            BuiltinModelError(format!("open selected semantic publication: {error}"))
        })?;
    Ok(
        match relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read selected semantic publication: {error}"))
        })? {
            Some(ProductSemanticPublicationRecord::Published { claim, .. }) => {
                Some(*claim.binding().identity.as_ref())
            }
            Some(ProductSemanticPublicationRecord::Unavailable(_)) | None => None,
        },
    )
}

impl<'request> SemanticCompilationContext<'request> {
    fn admit(
        package: backend_engine::PackageKey,
        package_label: &str,
        source_root: &'request Path,
        coordinate: Option<&'request PackageUrl>,
        request_id: u64,
        compiler: &'request LocalCompilerClient,
    ) -> Result<Self, BuiltinModelError> {
        let package_reference = backend_engine::PackageReference::parse(package_label.to_owned())
            .map_err(|error| {
            BuiltinModelError(format!("semantic package reference: {error:?}"))
        })?;
        if backend_engine::package_key(package_reference.as_str()) != package {
            return Err(BuiltinModelError(
                "semantic package reference does not match its product key".to_owned(),
            ));
        }
        Ok(Self {
            package,
            package_reference,
            source_root,
            coordinate,
            correlation: CorrelationId(request_id),
            compiler,
        })
    }
}

fn compile_semantic_publications(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    context: &SemanticCompilationContext<'_>,
    sources: Vec<ingest::CompilerSource>,
    revision_fence: &ingest::CompilerRevisionFence,
    observations: &BTreeMap<LanguageProfile, SourceObservationReceipt>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    workspace_snapshot: Option<&ingest::CompilerWorkspaceSnapshot>,
    owner_cluster: Option<&super::super::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pending_stored_acks: Option<&Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>>,
    execution_intent: CompileExecutionIntent,
) -> Result<
    (
        Vec<BuiltinSemanticChange>,
        Vec<(ProductSemanticPublicationKey, SemanticPublicationClaim)>,
        Vec<CargoPackageAliasEvidenceV1>,
    ),
    BuiltinModelError,
> {
    let mut by_profile = BTreeMap::<LanguageProfile, Vec<OwnedPackageSource>>::new();
    let mut source_paths_by_profile = BTreeMap::<LanguageProfile, BTreeSet<String>>::new();
    for source in sources {
        let profile = compile_profile(context.source_root, &source);
        source_paths_by_profile
            .entry(profile)
            .or_default()
            .insert(portable_relative_path(Path::new(&source.relative_path)));
        by_profile.entry(profile).or_default().push(
            OwnedPackageSource::from_string(&source.relative_path, source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        );
    }
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut changes = Vec::with_capacity(by_profile.len().saturating_mul(2));
    let mut selected_claims = Vec::with_capacity(by_profile.len());
    let mut cargo_alias_observations = Vec::new();
    for (profile, sources) in by_profile {
        let expected_artifacts = u32::try_from(sources.len())
            .map_err(|_| BuiltinModelError("semantic source count exceeds u32".to_owned()))?;
        let coordinate = super::super::compiler_scope::semantic_coordinate(
            context.package,
            profile,
            context.coordinate,
        )?;
        let request = PackageCompileRequest::new(
            GenerateTarget {
                correlation: context.correlation,
                profile,
                stage: backend_semantic::vocabulary::Stage::LowerIr,
            },
            coordinate.clone(),
        )
        .map_err(|error| BuiltinModelError(format!("semantic package profile: {error:?}")))?;
        let key = ProductSemanticPublicationKey::new(
            context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let Some(scan_observation) = observations.get(&profile) else {
            return Err(BuiltinModelError(
                "semantic compilation has no matching source observation".to_owned(),
            ));
        };
        let scan_input_digest = scan_observation.observation().revision().ok_or_else(|| {
            BuiltinModelError("semantic compilation observation has no input digest".to_owned())
        })?;
        let source_paths = source_paths_by_profile.remove(&profile).unwrap_or_default();
        let mut captured_work = None;
        let observation = if let (Some(owner), Some(snapshot)) = (owner_cluster, workspace_snapshot)
        {
            let compiler_target = CompilerPackageTargetV2::for_package(key.coordinate().clone());
            let execution_identity = context.compiler.execution_identity(
                compiler_target.target(),
                profile,
                backend_semantic::vocabulary::Stage::LowerIr,
            );
            if let Some(identity) = execution_identity.filter(|identity| {
                identity.target() == compiler_target.target()
                    && identity.profile() == profile
                    && identity.stage() == backend_semantic::vocabulary::Stage::LowerIr
            }) {
                let capture_source = CompilerWorkspaceCaptureView::new(snapshot, &source_paths);
                let package_lineage = PackageLineageId::from_canonical_parts(
                    b"nudox-local-product",
                    key.package().as_str().as_bytes(),
                    key.coordinate().as_str().as_bytes(),
                )
                .map_err(|error| {
                    BuiltinModelError(format!("derive compiler package lineage: {error}"))
                })?;
                let ids = super::super::profile_descriptor(super::super::BuiltinProfile::Product)
                    .map_err(BuiltinModelError)?
                    .ids;
                let max_output_bytes = super::super::execution_resources(ids).output_bytes;
                let capture_identity = CaptureWorkspaceIdentityV2::new(
                    compiler_target,
                    package_lineage.as_bytes(),
                    identity.invocation_recipe(),
                    scan_input_digest,
                    max_output_bytes,
                );
                let prior_capture = owner.prior_compiler_capture(capture_identity.cache_key());
                if ingest::compiler_revision_is_current(revision_fence)
                    .map_err(BuiltinModelError)?
                    && snapshot.revalidate().map_err(BuiltinModelError)?
                {
                    match capture_full_workspace_v2_with_prior(
                        &capture_source,
                        capture_identity,
                        owner.store(),
                        compiler_capture_budget(),
                        prior_capture.as_ref(),
                    ) {
                        Ok((capture, update_stats)) => {
                            owner.remember_compiler_capture(capture.clone());
                            eprintln!(
                                "locald compiler input capture: mode={:?} changed_files={:?} path_copied_pages={} source_bytes_read={}",
                                update_stats.mode,
                                update_stats.changed_file_records,
                                update_stats.path_copied_pages,
                                update_stats.source_bytes_read,
                            );
                            let observation = semantic_authority.observe(
                                &key,
                                capture.input_root(),
                                u64::from(expected_artifacts),
                            )?;
                            let claim = FullWorkspaceInputClaim {
                                identity: capture.manifest().identity_claim().map_err(|error| {
                                    BuiltinModelError(format!(
                                        "admit captured workspace identity: {error}"
                                    ))
                                })?,
                                input_closure_id: *capture.closure().as_bytes(),
                                manifest_object_id: *capture.manifest_object_id().as_bytes(),
                            };
                            let input = VerifierAcceptedFullWorkspaceInput::admit(
                                claim,
                                &CapturedFullWorkspaceVerifier {
                                    capture: &capture,
                                    store: owner.store(),
                                },
                            )
                            .map_err(|error| {
                                BuiltinModelError(format!(
                                    "verify captured compiler workspace: {error}"
                                ))
                            })?;
                            let work = CompilerWorkIdentity::new(
                                claim.identity.scope.package,
                                claim.identity.scope.target,
                                claim.identity.scope.recipe,
                                VerifiedCompilerInput::FullWorkspaceFresh(input),
                                None,
                                max_output_bytes,
                            )
                            .map_err(|error| {
                                BuiltinModelError(format!("admit compiler work identity: {error}"))
                            })?;
                            captured_work = Some((capture, work));
                            observation
                        }
                        Err(error) => {
                            eprintln!(
                                "locald compiler route fallback: workspace snapshot capture failed ({error})"
                            );
                            scan_observation.clone()
                        }
                    }
                } else {
                    scan_observation.clone()
                }
            } else {
                scan_observation.clone()
            }
        } else {
            scan_observation.clone()
        };
        // Only the remote lane may carry the full captured-workspace root.
        // The live local compiler still consumes a source/config subset whose
        // complete positive and negative reads are not proven.
        let mut attempt = if captured_work.is_some() {
            Some(semantic_authority.begin_candidate_attempt(&key, &observation)?)
        } else {
            None
        };
        let mut remote_publication = None;
        let mut execution_route = if owner_cluster.is_some() {
            SemanticExecutionRoute::LocalFallback
        } else {
            SemanticExecutionRoute::LocalOnly
        };
        if let (Some(owner), Some(journal), Some(snapshot), Some((capture, work))) = (
            owner_cluster,
            pending_stored_acks,
            workspace_snapshot,
            captured_work.as_ref(),
        ) {
            let reservation = journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reserve_capacity(*owner.owner_id().as_bytes());
            let reservation = match reservation {
                Ok(reservation) => Some(
                    super::super::pending_stored::PendingAckReservationLease::new(
                        Arc::clone(journal),
                        reservation,
                    ),
                ),
                Err(error) => {
                    eprintln!(
                        "locald compiler route fallback: remote result journal is unavailable ({error})"
                    );
                    None
                }
            };
            if let Some(reservation_guard) = reservation {
                let reservation = reservation_guard.reservation();
                let attempt_ref = attempt
                    .as_ref()
                    .expect("captured remote attempt is present");
                let source_observation = attempt_ref.observation().clone();
                let source_revision = *attempt_ref.input_digest();
                let payload_bytes = capture.payload_bytes();
                let local_cost = owner
                    .local_cost_estimate(*work.recipe().as_ref(), payload_bytes, expected_artifacts)
                    .map_err(|error| {
                        BuiltinModelError(format!("estimate local compiler route: {error}"))
                    })?;
                let ids = super::super::profile_descriptor(super::super::BuiltinProfile::Product)
                    .map_err(BuiltinModelError)?
                    .ids;
                let envelope = super::super::execution_resources(ids);
                let transfer_bytes = payload_bytes
                    .checked_add(work.max_output_bytes())
                    .ok_or_else(|| {
                        BuiltinModelError("compiler transfer demand overflow".to_owned())
                    })?;
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| BuiltinModelError("system clock precedes Unix epoch".to_owned()))?
                    .as_millis();
                let now_ms = u64::try_from(now_ms).map_err(|_| {
                    BuiltinModelError("system clock exceeds owner clock range".to_owned())
                })?;
                // The product execution envelope bounds CPU time in milliseconds; scheduler CPU
                // credits are millicores. Use the configured process count as a conservative
                // one-core-per-process demand instead of confusing those two units.
                let cpu_millicores = envelope.processes.checked_mul(1_000).ok_or_else(|| {
                    BuiltinModelError(
                        "configured compiler process demand overflows scheduler range".to_owned(),
                    )
                })?;
                let request = CompilerBalancingRequest {
                    work: *work,
                    demand: match execution_intent {
                        CompileExecutionIntent::Interactive => CompilerDemand::Interactive,
                        CompileExecutionIntent::Background => CompilerDemand::Background,
                    },
                    local: LocalCompilerAvailability::Ready,
                    local_cost: local_cost.completion(),
                    local_start_delay: 0,
                    submitted_at: now_ms,
                    // This owner probes synchronously before starting local work.
                    // A prior local run supplies a measured cost; a cold estimate
                    // remains ineligible for interactive remote placement.
                    local_first_budget: 0,
                    resources: CompilerResourceCredits {
                        cpu: CompilerCpuCredits::new(cpu_millicores),
                        memory: CompilerMemoryCredits::new(envelope.memory_bytes),
                        transfer: CompilerByteCredits::new(transfer_bytes),
                    },
                    session_affinity: None,
                    deadline_at: now_ms.checked_add(15 * 60 * 1_000),
                };
                let decision = match owner.probe_and_place_assignment(
                    owner.scheduler(),
                    &backend_engine::application::CompilerPlacementPolicy,
                    request,
                    local_cost,
                    attempt_ref,
                    capture,
                    now_ms.saturating_add(15 * 60 * 1_000),
                ) {
                    Ok(decision) => Some(decision),
                    Err(error) => {
                        eprintln!(
                            "locald compiler route fallback: placement probe failed ({error})"
                        );
                        None
                    }
                };
                if let Some(decision) = decision {
                    match decision {
                super::super::cluster_dispatch::CompilerDispatchDecision::Remote {
                    assignment: assignment_lease,
                    ..
                } => {
                    let assignment = assignment_lease.assignment();
                    let namespace_id = attempt_ref.namespace().namespace_id();
                    let product_key = super::super::pending_stored::PendingStoredAckProductKey {
                        package: key.package().as_str().into(),
                        coordinate: key.coordinate().as_str().into(),
                        profile: format!(
                            "{:02x}{:02x}/lower-ir",
                            <[u8; 2]>::from(profile)[0],
                            <[u8; 2]>::from(profile)[1]
                        )
                        .into_boxed_str(),
                    };
                    let preparation = (|| {
                        let worker_grant = trusted_worker_grant_for_assignment(
                            owner,
                            assignment,
                            namespace_id,
                            capture,
                        )?;
                        journal
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .bind_assignment(
                                reservation,
                                assignment,
                                namespace_id,
                                product_key.clone(),
                                &worker_grant,
                                attempt_ref,
                                capture,
                                snapshot.root_path(),
                            )
                            .map_err(|error| {
                                BuiltinModelError(format!(
                                    "persist remote compiler assignment reservation: {error}"
                                ))
                            })?;
                        let evidence = CompilerInputAdmissionEvidence::new(
                            assignment,
                            namespace_id,
                            capture.clone(),
                            source_revision,
                            snapshot.fence_digest(),
                        )
                        .map_err(|error| {
                            BuiltinModelError(format!("bind compiler input admission: {error:?}"))
                        })?;
                        let verifier = ProductCompilerInputAdmissionVerifier {
                            capture,
                            snapshot,
                            store: owner.store(),
                            authority: semantic_authority,
                            key: &key,
                            observation: &source_observation,
                            owner,
                        };
                        let input_admission = VerifiedCompilerInputAdmission::admit(
                            evidence, &verifier,
                        )
                        .map_err(|error| {
                            BuiltinModelError(format!("admit trusted compiler route: {error:?}"))
                        })?;
                        Ok::<_, BuiltinModelError>((worker_grant, input_admission))
                    })();
                    let prepared = pre_offer_result_or_local_fallback(
                        preparation,
                        "remote compiler admission",
                        revision_fence,
                        snapshot,
                    )?;
                    if let Some((worker_grant, input_admission)) = prepared {
                    let reservation_journal = Arc::clone(journal);
                    let selection_record_id = std::cell::Cell::new(None);
                    let selection_attempt_guard = std::cell::RefCell::new(None);
                    let mut journal_hook = |event: super::super::cluster_dispatch::CompilerResultJournalEvent<'_>| {
                        use super::super::cluster_dispatch::CompilerResultJournalEvent;
                        let mut journal = reservation_journal
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        match event {
                            CompilerResultJournalEvent::OfferMayBeSent {
                                assignment,
                                namespace_id,
                            } => {
                                journal
                                    .mark_offer_may_be_sent(reservation, assignment, namespace_id)
                                    .map_err(|error| error.to_string())?;
                                Ok(None)
                            }
                            CompilerResultJournalEvent::NoResultTerminal {
                                assignment,
                                namespace_id,
                            } => {
                                journal
                                    .release_after_no_result_terminal(
                                        reservation,
                                        assignment,
                                        namespace_id,
                                    )
                                    .map_err(|error| error.to_string())?;
                                Ok(None)
                            }
                            CompilerResultJournalEvent::RecoveredNoResultTerminal {
                                assignment,
                                namespace_id,
                                status,
                            } => {
                                journal
                                    .release_after_recovered_no_result(
                                        reservation,
                                        assignment,
                                        namespace_id,
                                        status,
                                    )
                                    .map_err(|error| error.to_string())?;
                                Ok(None)
                            }
                            CompilerResultJournalEvent::PrepareDisposition {
                                identity,
                                disposition,
                            } => {
                                let prepared = match selection_record_id.get() {
                                    Some(id) => journal.prepare_rejection_from_selection(
                                        id,
                                        identity,
                                        disposition,
                                    ),
                                    None => journal.prepare_rejection(
                                        reservation,
                                        identity,
                                        disposition,
                                    ),
                                }
                                .map_err(|error| error.to_string())?;
                                Ok(Some(prepared))
                            }
                            CompilerResultJournalEvent::WorkerRetired { identity, retired } => {
                                journal
                                    .mark_rejected_awaiting_confirm(identity, retired)
                                    .map_err(|error| error.to_string())?;
                                Ok(None)
                            }
                        }
                    };
                    let checked = match owner.run_pre_admitted_assignment(
                        owner.scheduler(),
                        assignment_lease,
                        namespace_id,
                        capture,
                        input_admission,
                        now_ms.saturating_add(15 * 60 * 1_000),
                        &mut journal_hook,
                    ) {
                        Ok(checked) => Some(checked),
                        Err(error) => {
                            eprintln!("locald compiler route fallback: remote result was not admitted ({error})");
                            owner.wake_pending_ack_retry_worker();
                            None
                        }
                    };
                    if let Some(checked) = checked {
                    let (admitted, pending, _worker_output_pin) = checked.into_publication_parts();
                    let worker_receipt = admitted.receipt();
                    let worker_result_receipt = admitted.worker_result_receipt();
                    let trust = super::super::pending_stored::PendingStoredAckTrust {
                        recipe: worker_grant.recipe(),
                        profile: <[u8; 2]>::from(worker_grant.profile()),
                        stage: u8::from(worker_grant.stage()),
                        toolchain: worker_grant.toolchain(),
                        environment: worker_grant.environment(),
                        target_platform: worker_grant.target_platform(),
                    };
                    let candidate_attempt = attempt.take().ok_or_else(|| {
                        BuiltinModelError("remote compiler attempt was already consumed".to_owned())
                    })?;
                    let selected = semantic_authority.publish_checked_remote(
                        &key,
                        &source_observation,
                        candidate_attempt,
                        admitted,
                        expected_artifacts,
                        snapshot.fence_digest(),
                        |candidate| {
                            if !ingest::compiler_revision_is_current(revision_fence)
                                .map_err(BuiltinModelError)?
                                || !snapshot.revalidate().map_err(BuiltinModelError)?
                            {
                                return Err(BuiltinModelError(
                                    "compiler workspace changed before remote semantic selection; retry indexing"
                                        .to_owned(),
                                ));
                            }
                            let row = super::super::pending_stored::PendingStoredAckRecord::from_selected_candidate(
                                *owner.owner_id().as_bytes(),
                                worker_grant.address(),
                                namespace_id,
                                product_key,
                                pending.assignment,
                                worker_receipt,
                                candidate,
                                trust,
                                worker_result_receipt,
                                capture,
                                snapshot.root_path(),
                            )
                            .map_err(|error| {
                                BuiltinModelError(format!("prepare stored-result ACK identity: {error}"))
                            })?;
                            let (prepared, guard) = super::super::pending_stored::PendingStoredAckJournal::prepare_selection_guarded(
                                &reservation_journal,
                                reservation,
                                row.clone(),
                            )
                                .map_err(|error| {
                                BuiltinModelError(format!("persist stored-result ACK intent: {error}"))
                            })?;
                            let _ = prepared;
                            selection_record_id.set(Some(row.id()));
                            selection_attempt_guard.replace(Some(guard));
                            // The foreground path still owns the imminent
                            // compare-and-select; wake recovery only after its
                            // success/error branch settles that authority race.
                            if !ingest::compiler_revision_is_current(revision_fence)
                                .map_err(BuiltinModelError)?
                                || !snapshot.revalidate().map_err(BuiltinModelError)?
                            {
                                return Err(BuiltinModelError(
                                    "compiler workspace changed while persisting the remote ACK intent; semantic selection was withheld"
                                    .to_owned(),
                                ));
                            }
                            Ok(())
                        },
                    );
                    drop(selection_attempt_guard.borrow_mut().take());
                    match selected {
                        Ok(publication) => {
                            if selection_record_id.get().is_some() {
                                // The owner retry worker proves and ACKs this row outside the
                                // CommandAdapter lock.
                                owner.wake_pending_ack_retry_worker();
                            }
                            remote_publication = Some(publication);
                            execution_route = SemanticExecutionRoute::RemoteSelected;
                        }
                        Err(error) => {
                            // Errors before or after the callback can be transient
                            // publication failures or uncertain CAS outcomes. The
                            // durable selection intent or OfferMayBeSent row lets
                            // recovery retry without terminally rejecting a valid
                            // worker result.
                            owner.wake_pending_ack_retry_worker();
                            return Err(error);
                        }
                    }
                    }
                    }
                }
                super::super::cluster_dispatch::CompilerDispatchDecision::LocalFallback {
                    reason,
                    ..
                }
                | super::super::cluster_dispatch::CompilerDispatchDecision::OfflineUnavailable {
                    reason,
                    ..
                } => {
                    eprintln!("locald compiler route fallback: {reason:?}");
                }
                }
                }
            }
        } else if owner_cluster.is_some() {
            eprintln!(
                "locald compiler route fallback: complete workspace capture or ACK journal unavailable"
            );
        }
        let (claim, selected, publication_coverage, cargo_alias_evidence) =
            if let Some((claim, selected)) = remote_publication {
                // Remote result envelopes carry semantic artifacts but no typed
                // source-scope gaps. Remote admission therefore requires the
                // exact complete source-identity multiset before this branch can
                // select a generation; partial worker output is rejected there.
                (
                    claim,
                    selected,
                    SemanticPublicationCoverage::Complete,
                    staged_cargo_alias_evidence(
                        profile,
                        scan_input_digest,
                        context.source_root,
                        None,
                    )?,
                )
            } else {
                // A fallback local compile uses its own fresh, Partial source
                // observation. This prevents a captured full-workspace root from
                // being attached to bytes read later from the live checkout.
                let local_observation = semantic_authority.observe(
                    &key,
                    scan_input_digest,
                    u64::from(expected_artifacts),
                )?;
                drop(attempt.take());
                let local_attempt =
                    semantic_authority.begin_candidate_attempt(&key, &local_observation)?;
                let local_input_claim = SemanticInputWitness::claimed_state(
                    scan_input_digest,
                    ScopeRoot::from_bytes(scan_input_digest),
                    Coverage::Partial,
                );
                let source_set = OwnedPackageSourceSet::new(
                    request,
                    context.source_root.to_path_buf(),
                    sources.into_boxed_slice(),
                )
                .map_err(|error| BuiltinModelError(error.to_string()))?
                .with_input_claim(local_input_claim);
                let local_compile_started = Instant::now();
                let (staged, publication_coverage) = admit_local_compile(
                    context.compiler.compile_package_sources_staged(source_set),
                    expected_artifacts,
                )?;
                let cargo_alias_evidence = match staged_cargo_alias_evidence(
                    profile,
                    scan_input_digest,
                    context.source_root,
                    Some(&staged),
                ) {
                    Ok(evidence) => evidence,
                    Err(error) => {
                        semantic_authority.retire_candidate_attempt(
                            &local_attempt,
                            backend_extension_turso::CandidateAttemptRetirementReason::Refused,
                        )?;
                        return Err(error);
                    }
                };
                if let (Some(owner), Some((capture, work))) =
                    (owner_cluster, captured_work.as_ref())
                {
                    let elapsed_ms = u64::try_from(local_compile_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX);
                    match super::super::cluster_dispatch::LocalCompilerCostObservation::new(
                        *work.recipe().as_ref(),
                        capture.payload_bytes(),
                        expected_artifacts,
                        elapsed_ms.max(1),
                    ) {
                        Ok(observation) => {
                            if let Err(error) = owner.record_local_observation(observation) {
                                eprintln!(
                                    "locald compiler cost observation was not saved: {error}"
                                );
                            }
                        }
                        Err(error) => {
                            eprintln!("locald compiler cost observation was rejected: {error}");
                        }
                    }
                }
                let publication = publish_local_compile(
                    semantic_authority,
                    &key,
                    local_attempt,
                    &staged,
                    revision_fence,
                )?;
                (
                    publication.0,
                    publication.1,
                    publication_coverage,
                    cargo_alias_evidence,
                )
            };
        match execution_route {
            SemanticExecutionRoute::RemoteSelected => {
                eprintln!("locald semantic profile selected from checked remote compiler output");
            }
            SemanticExecutionRoute::LocalFallback => {
                eprintln!("locald semantic profile selected from local compiler fallback");
            }
            SemanticExecutionRoute::LocalOnly => {}
        }
        record_semantic_publication(
            &relation,
            key.clone(),
            publication_coverage,
            claim,
            &mut changes,
        )?;
        selected_claims.push((key, claim));
        if let Some(evidence) = cargo_alias_evidence {
            cargo_alias_observations.push(evidence);
        }
        let _ = selected;
    }
    Ok((changes, selected_claims, cargo_alias_observations))
}

/// Admits one local compile's output: every expected source is accounted
/// for (an artifact or a typed scope gap), and a gap makes the publication
/// partial. The words are the owner's refusal when it is not.
fn admit_local_compile(
    compiled: Result<StagedSemanticPackage, PackageSemanticRuntimeError>,
    expected_artifacts: u32,
) -> Result<(StagedSemanticPackage, SemanticPublicationCoverage), BuiltinModelError> {
    let staged = match compiled {
        Ok(staged)
            if u32::try_from(staged.artifacts().len()).ok()
                == Some(staged.manifest_facts().fragment_count)
                && staged
                    .artifacts()
                    .len()
                    .checked_add(staged.coverage_gaps().len())
                    == Some(expected_artifacts as usize) =>
        {
            staged
        }
        Ok(_) => {
            return Err(BuiltinModelError(
                "local compiler output did not account for every expected source; prior selected semantic generation was preserved"
                    .to_owned(),
            ));
        }
        Err(error) => {
            return Err(BuiltinModelError(local_compile_error_chain(&error)));
        }
    };
    let publication_coverage = if staged.coverage_gaps().is_empty() {
        SemanticPublicationCoverage::Complete
    } else {
        for gap in staged.coverage_gaps() {
            eprintln!(
                "locald semantic source scope gap: path={} source={:?} bytes={} cause={:?}",
                gap.relative_path(),
                gap.source().identity,
                gap.source().byte_len,
                gap.cause(),
            );
        }
        let completed = u32::try_from(staged.artifacts().len())
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| {
                BuiltinModelError(
                    "local compiler found no active semantic source to publish; prior selected semantic generation was preserved"
                        .to_owned(),
                )
            })?;
        let total = NonZeroU32::new(expected_artifacts).ok_or_else(|| {
            BuiltinModelError(
                "local compiler source scope was empty; prior selected semantic generation was preserved"
                    .to_owned(),
            )
        })?;
        SemanticPublicationCoverage::Partial(
            PartialSemanticCoverage::new(completed, total)
                .map_err(|error| BuiltinModelError(error.to_owned()))?,
        )
    };
    Ok((staged, publication_coverage))
}

const MAX_LOCAL_COMPILE_ERROR_BYTES: usize = backend_library::MAX_PRODUCT_TEXT_BYTES;
const MAX_LOCAL_COMPILE_ERROR_CAUSES: usize = 12;
const MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES: usize = 1_024;

/// Renders the bounded typed source chain at the product refusal boundary.
///
/// The outer wording is a stable product diagnostic. Each distinct typed cause follows it, with
/// a fixed byte and depth budget so unusually verbose errors cannot grow the reply without bound.
fn local_compile_error_chain(error: &dyn std::error::Error) -> String {
    const PREFIX: &str =
        "local semantic compilation failed; prior selected semantic generation was preserved: ";
    const CAUSE_PREFIX: &str = "\ncaused by: ";

    let first = bounded_error_display(error, MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES);
    let mut previous = first.clone();
    let mut source = error.source();
    let mut seen: [Option<&dyn std::error::Error>; MAX_LOCAL_COMPILE_ERROR_CAUSES + 1] =
        [None; MAX_LOCAL_COMPILE_ERROR_CAUSES + 1];
    seen[0] = Some(error);
    let mut seen_count = 1;
    let mut causes = Vec::new();
    let mut cycle_detected = false;
    for _ in 0..MAX_LOCAL_COMPILE_ERROR_CAUSES {
        let Some(cause) = source else {
            break;
        };
        if seen[..seen_count]
            .iter()
            .flatten()
            .any(|visited| std::ptr::eq(*visited, cause))
        {
            cycle_detected = true;
            break;
        }
        seen[seen_count] = Some(cause);
        seen_count += 1;

        let message = bounded_error_display(cause, MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES);
        if !previous.ends_with(&message) {
            causes.push(message.clone());
        }
        previous = message;
        source = cause.source();
    }
    let depth_truncated = if !cycle_detected {
        if let Some(next) = source {
            if seen[..seen_count]
                .iter()
                .flatten()
                .any(|visited| std::ptr::eq(*visited, next))
            {
                cycle_detected = true;
                false
            } else {
                true
            }
        } else {
            false
        }
    } else {
        false
    };
    let status = if cycle_detected {
        Some("\nerror cause chain cycle detected".to_owned())
    } else if depth_truncated {
        Some(format!(
            "\nadditional causes omitted after depth limit {MAX_LOCAL_COMPILE_ERROR_CAUSES}"
        ))
    } else {
        None
    };

    let base_bytes = PREFIX.len() + first.len();
    let cause_bytes = causes
        .iter()
        .map(|cause| CAUSE_PREFIX.len() + cause.len())
        .sum::<usize>();
    let status_bytes = status.as_ref().map_or(0, String::len);
    let mut output = BoundedDiagnosticText::new(MAX_LOCAL_COMPILE_ERROR_BYTES);
    let _ = write!(&mut output, "{PREFIX}{first}");

    if base_bytes + cause_bytes + status_bytes <= MAX_LOCAL_COMPILE_ERROR_BYTES {
        for cause in &causes {
            let _ = write!(&mut output, "{CAUSE_PREFIX}{cause}");
        }
    } else if let Some(deepest) = causes.last() {
        let middle = &causes[..causes.len() - 1];
        let deepest_bytes = CAUSE_PREFIX.len() + deepest.len();
        let reserved_tail = deepest_bytes + status_bytes;
        let omission_marker_bytes =
            format!("\nintermediate causes omitted: {}", middle.len()).len();
        let mut used_bytes = base_bytes;
        let mut included = 0;
        for cause in middle {
            let cause_bytes = CAUSE_PREFIX.len() + cause.len();
            if used_bytes + cause_bytes + omission_marker_bytes + reserved_tail
                > MAX_LOCAL_COMPILE_ERROR_BYTES
            {
                break;
            }
            let _ = write!(&mut output, "{CAUSE_PREFIX}{cause}");
            used_bytes += cause_bytes;
            included += 1;
        }
        let omitted = middle.len() - included;
        if omitted > 0 {
            let _ = write!(&mut output, "\nintermediate causes omitted: {omitted}");
        }
        let _ = write!(&mut output, "{CAUSE_PREFIX}{deepest}");
    }
    if let Some(status) = status {
        let _ = write!(&mut output, "{status}");
    }
    output.finish()
}

fn bounded_error_display(error: &dyn std::fmt::Display, maximum_bytes: usize) -> String {
    let mut output = BoundedDiagnosticText::new(maximum_bytes);
    let _ = write!(&mut output, "{error}");
    output.finish()
}

struct BoundedDiagnosticText {
    text: String,
    maximum_bytes: usize,
    truncated: bool,
}

impl BoundedDiagnosticText {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            text: String::new(),
            maximum_bytes,
            truncated: false,
        }
    }

    fn finish(self) -> String {
        self.text
    }

    fn push_sanitized(&mut self, value: &str) {
        for character in value.chars() {
            self.text
                .push(if character == '\0' { ' ' } else { character });
        }
    }
}

impl std::fmt::Write for BoundedDiagnosticText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if self.truncated {
            return Err(std::fmt::Error);
        }
        const TRUNCATION_MARKER: &str = "…";
        let remaining = self.maximum_bytes.saturating_sub(self.text.len());
        if value.len() <= remaining {
            self.push_sanitized(value);
            return Ok(());
        }
        let content_budget = remaining.saturating_sub(TRUNCATION_MARKER.len());

        let mut boundary = value.len().min(content_budget);
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        self.push_sanitized(&value[..boundary]);
        if remaining >= TRUNCATION_MARKER.len() {
            self.text.push_str(TRUNCATION_MARKER);
        }
        self.truncated = true;
        Err(std::fmt::Error)
    }
}

#[cfg(test)]
mod local_compile_error_chain_tests {
    use super::{
        MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES, MAX_LOCAL_COMPILE_ERROR_BYTES,
        MAX_LOCAL_COMPILE_ERROR_CAUSES, admit_local_compile, local_compile_error_chain,
    };
    use backend_engine::application::{
        PackageSemanticError, PackageSemanticRuntimeError, StagedSemanticPackage,
    };
    use backend_engine::publication::{
        GenerationBuildError, PublishCompiledError, PublishSemanticError,
    };
    use std::error::Error;
    use std::fmt;
    use std::sync::OnceLock;

    #[derive(Debug)]
    struct DiagnosticCause {
        message: String,
        source: Option<Box<dyn Error + 'static>>,
    }

    impl fmt::Display for DiagnosticCause {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl Error for DiagnosticCause {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source.as_deref()
        }
    }

    fn numeric_generation_failure() -> PackageSemanticRuntimeError {
        PackageSemanticRuntimeError::Package(PackageSemanticError::StagedOutput(
            PublishSemanticError::Publication(PublishCompiledError::Generation(
                GenerationBuildError::ReopenedSemanticBytesLength {
                    expected: 4_096,
                    observed: 2_048,
                },
            )),
        ))
    }

    #[test]
    fn refusal_includes_numeric_generation_cause_through_typed_source_chain() {
        let refusal = match admit_local_compile(
            Err::<StagedSemanticPackage, _>(numeric_generation_failure()),
            1,
        ) {
            Err(refusal) => refusal,
            Ok(_) => panic!("the package compile failure must remain a refusal"),
        };
        let detail = refusal.to_string();

        assert!(detail.starts_with(
            "local semantic compilation failed; prior selected semantic generation was preserved: package semantic output could not be prepared for transport: semantic publication failed after all paired artifacts were prepared"
        ));
        assert!(detail.contains(
            "caused by: compiler package could not construct a verified complete generation"
        ));
        assert!(
            detail.contains("caused by: stored semantic-image bytes have 2048 bytes, require 4096")
        );
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
    }

    #[test]
    fn verbose_middle_causes_do_not_hide_the_deepest_numeric_generation_cause() {
        let mut source: Box<dyn Error> = Box::new(numeric_generation_failure());
        for ordinal in 0..4 {
            source = Box::new(DiagnosticCause {
                message: format!("intermediate cause {ordinal}: {}", "m".repeat(1_500)),
                source: Some(source),
            });
        }
        let error = DiagnosticCause {
            message: "package semantic output could not be prepared for transport".to_owned(),
            source: Some(source),
        };

        let detail = local_compile_error_chain(&error);

        assert!(detail.contains("intermediate causes omitted:"));
        assert!(detail.contains("stored semantic-image bytes have 2048 bytes, require 4096"));
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
        assert!(backend_library::ProductText::new(detail).is_ok());
    }

    #[test]
    fn exact_product_text_boundary_keeps_the_complete_terminal_cause() {
        let terminal = "terminal numeric cause: expected 4096 bytes, observed 2048";
        let make_error = |terminal_message: String| {
            let mut source: Box<dyn Error> = Box::new(DiagnosticCause {
                message: terminal_message,
                source: None,
            });
            for ordinal in (0..2).rev() {
                source = Box::new(DiagnosticCause {
                    message: format!("middle{ordinal}: {}", "m".repeat(991)),
                    source: Some(source),
                });
            }
            DiagnosticCause {
                message: format!("root: {}", "r".repeat(994)),
                source: Some(source),
            }
        };
        let initial = make_error(terminal.to_owned());
        let initial_detail = local_compile_error_chain(&initial);
        let padding_bytes = MAX_LOCAL_COMPILE_ERROR_BYTES - initial_detail.len();
        assert!(terminal.len() + padding_bytes <= MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES);

        let error = make_error(format!("{}{terminal}", "x".repeat(padding_bytes)));
        let detail = local_compile_error_chain(&error);

        assert_eq!(detail.len(), MAX_LOCAL_COMPILE_ERROR_BYTES);
        assert!(detail.ends_with(terminal));
        assert!(!detail.contains('…'));
        assert!(backend_library::ProductText::new(detail).is_ok());
    }

    #[test]
    fn unicode_and_nul_are_sanitized_before_protocol_text_admission() {
        let error = DiagnosticCause {
            message: format!("semantic output: café\0{}", "🌲".repeat(600)),
            source: None,
        };

        let detail = local_compile_error_chain(&error);

        assert!(detail.contains("café "));
        assert!(detail.contains('…'));
        assert!(!detail.contains('\0'));
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
        assert!(backend_library::ProductText::new(detail).is_ok());
    }

    #[test]
    fn source_depth_limit_reports_omitted_causes_and_keeps_last_admitted_cause() {
        let mut source: Box<dyn Error> = Box::new(DiagnosticCause {
            message: "cause beyond depth limit".to_owned(),
            source: None,
        });
        for ordinal in (0..=MAX_LOCAL_COMPILE_ERROR_CAUSES).rev() {
            source = Box::new(DiagnosticCause {
                message: format!("cause {ordinal}"),
                source: Some(source),
            });
        }
        let error = DiagnosticCause {
            message: "stable package failure".to_owned(),
            source: Some(source),
        };

        let detail = local_compile_error_chain(&error);

        assert!(detail.contains(&format!("cause {}", MAX_LOCAL_COMPILE_ERROR_CAUSES - 1)));
        assert!(detail.contains(&format!(
            "additional causes omitted after depth limit {MAX_LOCAL_COMPILE_ERROR_CAUSES}"
        )));
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
    }

    #[derive(Debug)]
    struct CyclicSource;

    static CYCLIC_SOURCE: OnceLock<CyclicSource> = OnceLock::new();

    impl fmt::Display for CyclicSource {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("cyclic source")
        }
    }

    impl Error for CyclicSource {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            CYCLIC_SOURCE
                .get()
                .map(|source| source as &(dyn Error + 'static))
        }
    }

    #[test]
    fn cyclic_source_chain_is_reported_without_exceeding_its_depth_budget() {
        let error = CYCLIC_SOURCE.get_or_init(|| CyclicSource);

        let detail = local_compile_error_chain(error);

        assert!(detail.contains("error cause chain cycle detected"));
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
    }
}

/// Selects one local compile's generation, while the source it was compiled
/// from is still the source on disk.
fn publish_local_compile(
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    key: &ProductSemanticPublicationKey,
    attempt: backend_extension_turso::CandidateAttempt,
    staged: &StagedSemanticPackage,
    revision_fence: &ingest::CompilerRevisionFence,
) -> Result<
    (
        SemanticPublicationClaim,
        backend_extension_turso::SelectedGeneration,
    ),
    BuiltinModelError,
> {
    if !ingest::compiler_revision_is_current(revision_fence).map_err(BuiltinModelError)? {
        return Err(BuiltinModelError(
            "compiler source or configuration revision changed during semantic compilation; retry indexing"
                .to_owned(),
        ));
    }
    semantic_authority.publish_staged(key, attempt, staged, |_| {
        let source_is_current =
            ingest::compiler_revision_is_current(revision_fence).map_err(BuiltinModelError)?;
        if source_is_current {
            Ok(())
        } else {
            Err(BuiltinModelError(
                "compiler source or configuration revision changed before semantic selection; retry indexing"
                    .to_owned(),
            ))
        }
    })
}

/// The semantic relation changes one selected generation makes: its history
/// row, and the selection itself when it moved.
fn record_semantic_publication(
    relation: &backend_engine::WorkspaceRelationHandle<BuiltinSemanticRelation>,
    key: ProductSemanticPublicationKey,
    publication_coverage: SemanticPublicationCoverage,
    claim: SemanticPublicationClaim,
    changes: &mut Vec<BuiltinSemanticChange>,
) -> Result<(), BuiltinModelError> {
    {
        let value = ProductSemanticPublicationRecord::Published {
            // Package bytes remain bound by the input witness. A typed
            // Rust scope gap downgrades publication coverage while retaining
            // every active semantic artifact in this generation.
            coverage: publication_coverage,
            claim,
        };
        let history_key = key.for_generation(claim.binding().identity);
        match relation.lookup(&history_key).map_err(|error| {
            BuiltinModelError(format!("read semantic publication history: {error}"))
        })? {
            None => changes.push(BuiltinSemanticChange {
                key: history_key,
                after: Some(value.clone()),
            }),
            Some(prior) if prior == value => {}
            Some(_) => {
                return Err(BuiltinModelError(
                    "immutable semantic generation was rebound to another claim".to_owned(),
                ));
            }
        }
        if relation.lookup(&key).map_err(|error| {
            BuiltinModelError(format!("read selected semantic publication: {error}"))
        })? != Some(value.clone())
        {
            changes.push(BuiltinSemanticChange {
                key,
                after: Some(value),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SemanticExecutionRoute {
    RemoteSelected,
    LocalFallback,
    LocalOnly,
}

/// Converts a failed remote admission step into the local route only while the
/// source state observed by this indexing attempt is still current. This is
/// used exclusively before the runner can persist `OfferMayBeSent`; once that
/// boundary is crossed, its result must be recovered through the ACK journal.
fn pre_offer_result_or_local_fallback<T>(
    result: Result<T, BuiltinModelError>,
    stage: &str,
    revision_fence: &ingest::CompilerRevisionFence,
    snapshot: &ingest::CompilerWorkspaceSnapshot,
) -> Result<Option<T>, BuiltinModelError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let compiler_revision_is_current =
                ingest::compiler_revision_is_current(revision_fence).map_err(BuiltinModelError)?;
            let workspace_snapshot_is_current = snapshot.revalidate().map_err(BuiltinModelError)?;
            if !compiler_revision_is_current || !workspace_snapshot_is_current {
                return Err(BuiltinModelError(
                    "compiler workspace changed before remote Offer; retry indexing".to_owned(),
                ));
            }
            eprintln!("locald compiler route fallback: {stage} failed before Offer ({error})");
            Ok(None)
        }
    }
}

struct CompilerWorkspaceCaptureView<'snapshot> {
    snapshot: &'snapshot ingest::CompilerWorkspaceSnapshot,
    entries: Vec<CompilerWorkspaceEntryV2>,
}

impl<'snapshot> CompilerWorkspaceCaptureView<'snapshot> {
    fn new(
        snapshot: &'snapshot ingest::CompilerWorkspaceSnapshot,
        source_paths: &BTreeSet<String>,
    ) -> Self {
        let entries = snapshot
            .entries()
            .iter()
            .map(|entry| match entry.kind {
                ingest::CompilerWorkspaceEntryKind::Directory => {
                    CompilerWorkspaceEntryV2::directory(entry.path.as_str())
                }
                ingest::CompilerWorkspaceEntryKind::File => {
                    let length = entry.byte_length.unwrap_or_default();
                    let role = if source_paths.contains(&entry.path) {
                        backend_engine::compiler_cluster_transport::CompilerWorkspaceFileRoleV2::Source
                    } else if is_lockfile(&entry.path) {
                        backend_engine::compiler_cluster_transport::CompilerWorkspaceFileRoleV2::Lock
                    } else if is_compiler_configuration(&entry.path) {
                        backend_engine::compiler_cluster_transport::CompilerWorkspaceFileRoleV2::Configuration
                    } else {
                        backend_engine::compiler_cluster_transport::CompilerWorkspaceFileRoleV2::Other
                    };
                    CompilerWorkspaceEntryV2::file(entry.path.as_str(), length, role)
                }
            })
            .collect();
        Self { snapshot, entries }
    }
}

impl backend_engine::compiler_cluster_transport::WorkspaceSnapshotSourceV2
    for CompilerWorkspaceCaptureView<'_>
{
    fn entries(&self) -> &[CompilerWorkspaceEntryV2] {
        &self.entries
    }

    fn policy_identity(&self) -> &str {
        self.snapshot.policy_identity()
    }

    fn fence_digest(&self) -> [u8; 32] {
        self.snapshot.fence_digest()
    }

    fn revalidate(&self) -> Result<bool, String> {
        self.snapshot.revalidate()
    }

    fn stream_file(
        &self,
        path: &str,
        max_bytes: u64,
        chunk_bytes: usize,
        consume: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String> {
        self.snapshot
            .stream_file(path, max_bytes, chunk_bytes, consume)
    }
}

struct CapturedFullWorkspaceVerifier<'capture> {
    capture: &'capture CapturedFullWorkspaceV2,
    store: &'capture backend_store::FileStore,
}

impl FullWorkspaceInputVerifier for CapturedFullWorkspaceVerifier<'_> {
    fn verify_full_workspace_capture(
        &self,
        claim: FullWorkspaceInputClaim,
    ) -> Result<(), FullWorkspaceInputError> {
        let manifest_identity = self
            .capture
            .manifest()
            .identity_claim()
            .map_err(|_| FullWorkspaceInputError::Rejected)?;
        if claim.identity != manifest_identity
            || claim.input_closure_id != *self.capture.closure().as_bytes()
            || claim.manifest_object_id != *self.capture.manifest_object_id().as_bytes()
        {
            return Err(FullWorkspaceInputError::Rejected);
        }
        self.capture
            .verify_in_store(self.store)
            .map_err(|_| FullWorkspaceInputError::Rejected)?;
        Ok(())
    }
}

struct ProductCompilerInputAdmissionVerifier<'a> {
    capture: &'a CapturedFullWorkspaceV2,
    snapshot: &'a ingest::CompilerWorkspaceSnapshot,
    store: &'a backend_store::FileStore,
    authority: &'a super::super::semantic_authority::SemanticAuthority,
    key: &'a ProductSemanticPublicationKey,
    observation: &'a SourceObservationReceipt,
    owner: &'a super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
}

fn trusted_worker_grant_for_assignment(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    assignment: backend_engine::application::CompilerAssignment,
    namespace_id: [u8; 16],
    capture: &CapturedFullWorkspaceV2,
) -> Result<crate::compiler_trust::TrustedCompilerWorkerGrant, BuiltinModelError> {
    use backend_engine::application::CompilerAssignmentRoute;

    let CompilerAssignmentRoute::Remote(peer) = assignment.route() else {
        return Err(BuiltinModelError(
            "compiler placement returned a local assignment for remote admission".to_owned(),
        ));
    };
    let manifest = capture.manifest();
    let policy = owner
        .trusted_workers()
        .map_err(|error| BuiltinModelError(format!("reload compiler trust policy: {error}")))?;
    policy
        .grants()
        .iter()
        .find(|grant| {
            *grant.peer().as_bytes() == peer.as_bytes()
                && grant.namespace_id() == namespace_id
                && grant.recipe() == manifest.recipe()
                && grant.profile() == manifest.profile()
                && grant.stage() == manifest.stage()
                && grant.toolchain() == manifest.toolchain()
                && grant.environment() == manifest.environment()
                && grant.target_platform() == manifest.target_platform()
        })
        .cloned()
        .ok_or_else(|| {
            BuiltinModelError(
                "compiler assignment has no exact persisted trusted-worker grant".to_owned(),
            )
        })
}

struct RecoveredCompilerRoute {
    key: ProductSemanticPublicationKey,
    snapshot: ingest::CompilerWorkspaceSnapshot,
    capture: CapturedFullWorkspaceV2,
    attempt: backend_extension_turso::CandidateAttempt,
    assignment: backend_engine::application::CompilerAssignment,
    input_admission: VerifiedCompilerInputAdmission,
    worker_grant: crate::compiler_trust::TrustedCompilerWorkerGrant,
    source_observation: SourceObservationReceipt,
    expected_artifacts: u32,
}

struct ReopenedPersistedCompilerAssignment {
    capture: CapturedFullWorkspaceV2,
    assignment: backend_engine::application::CompilerAssignment,
    worker_grant: crate::compiler_trust::TrustedCompilerWorkerGrant,
    namespace_id: [u8; 16],
}

/// Reopens only the durable assignment facts needed for terminal cleanup.
/// This deliberately does not require a current Turso attempt, current source
/// fence, current trust grant, or `VerifiedCompilerInputAdmission`; callers must
/// hold a separate exact stale/superseded authority proof before using it.
fn reopen_persisted_compiler_assignment(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    identity: &super::super::pending_stored::PendingAckAssignmentIdentity,
    captured: &super::super::pending_stored::PendingAckCapturedWork,
) -> Result<ReopenedPersistedCompilerAssignment, String> {
    use backend_engine::application::{
        CompilerAssignment, CompilerAttemptToken, CompilerWorkIdentity, FullWorkspaceInputClaim,
        VerifiedCompilerInput, VerifierAcceptedFullWorkspaceInput,
    };
    use backend_replication::{AttemptId, Fence};

    let key = identity
        .product_key
        .product_semantic_key()
        .map_err(|error| format!("reopen persisted compiler product key: {error}"))?;
    let namespace = super::super::semantic_authority::SemanticAuthority::namespace(
        key.package(),
        key.coordinate(),
        key.profile(),
    )
    .map_err(|error| format!("rebuild persisted compiler namespace: {error}"))?;
    let namespace_id = namespace.namespace_id();
    if namespace_id != identity.namespace_id {
        return Err("persisted compiler namespace differs from its product key".to_owned());
    }

    let store = owner.store();
    let capture_objects = usize::try_from(captured.object_count)
        .map_err(|_| "persisted compiler capture object count exceeds this target".to_owned())?;
    let capture = CapturedFullWorkspaceV2::reopen_pinned_in_store(
        &store,
        captured.reopen_expectation(identity.work_id, identity.trust),
        backend_store::ArtifactBudget::new(
            capture_objects,
            capture_objects,
            captured.payload_bytes,
            64 * 1024,
            200_000,
        ),
    )
    .map_err(|error| format!("reopen pinned persisted compiler input: {error}"))?;
    if capture.payload_bytes() != captured.payload_bytes
        || capture.input_root() != captured.input_root
        || capture.source_fence_digest() != captured.source_fence_digest
        || capture.manifest().package_lineage() != captured.package_lineage
    {
        return Err("persisted V2 capture differs from its durable identity".to_owned());
    }
    let manifest_identity = capture
        .manifest()
        .identity_claim()
        .map_err(|error| format!("read persisted compiler identity: {error}"))?;
    if *manifest_identity.scope.recipe.as_ref() != identity.trust.recipe
        || capture.manifest().profile()
            != LanguageProfile::try_from(identity.trust.profile)
                .map_err(|_| "persisted compiler profile is invalid".to_owned())?
        || capture.manifest().stage()
            != backend_semantic::vocabulary::Stage::try_from(identity.trust.stage)
                .map_err(|_| "persisted compiler stage is invalid".to_owned())?
        || capture.manifest().toolchain() != identity.trust.toolchain
        || capture.manifest().environment() != identity.trust.environment
        || capture.manifest().target_platform() != identity.trust.target_platform
    {
        return Err("persisted V2 input differs from its compiler grant facts".to_owned());
    }
    let input_claim = FullWorkspaceInputClaim {
        identity: manifest_identity,
        input_closure_id: *capture.closure().as_bytes(),
        manifest_object_id: *capture.manifest_object_id().as_bytes(),
    };
    let input = VerifierAcceptedFullWorkspaceInput::admit(
        input_claim,
        &CapturedFullWorkspaceVerifier {
            capture: &capture,
            store: &store,
        },
    )
    .map_err(|error| format!("verify persisted V2 input claim: {error}"))?;
    let work = CompilerWorkIdentity::new(
        manifest_identity.scope.package,
        manifest_identity.scope.target,
        manifest_identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(input),
        None,
        capture.manifest().max_output_bytes(),
    )
    .map_err(|error| format!("rebuild persisted compiler work identity: {error}"))?;
    if work.transfer_work_id() != identity.work_id {
        return Err("persisted compiler work differs from its assignment identity".to_owned());
    }
    let peer = backend_engine::application::CompilerPeerId::new(identity.worker_peer)
        .map_err(|error| format!("reopen persisted worker peer: {error}"))?;
    let token = CompilerAttemptToken::new(
        AttemptId::new(identity.assignment_attempt)
            .map_err(|error| format!("reopen persisted assignment ordinal: {error}"))?,
        Fence::from_bytes(identity.assignment_fence)
            .map_err(|error| format!("reopen persisted assignment fence: {error}"))?,
    );
    let assignment = CompilerAssignment::recover_exact(
        work,
        token,
        peer,
        identity.work_id,
        token.attempt(),
        token.fence(),
    )
    .map_err(|error| format!("rebuild persisted compiler assignment: {error}"))?;
    let endpoint = backend_engine::cluster_transport::EndpointId::from_bytes(&identity.worker_peer)
        .map_err(|_| "persisted worker endpoint is invalid".to_owned())?;
    let worker_grant = crate::compiler_trust::TrustedCompilerWorkerGrant::new(
        endpoint,
        identity.worker_address,
        identity.namespace_id,
        identity.trust.recipe,
        LanguageProfile::try_from(identity.trust.profile)
            .map_err(|_| "persisted worker profile is invalid".to_owned())?,
        backend_semantic::vocabulary::Stage::try_from(identity.trust.stage)
            .map_err(|_| "persisted worker stage is invalid".to_owned())?,
        identity.trust.toolchain,
        identity.trust.environment,
        identity.trust.target_platform,
    )
    .map_err(|error| format!("rebuild persisted worker grant: {error}"))?;
    Ok(ReopenedPersistedCompilerAssignment {
        capture,
        assignment,
        worker_grant,
        namespace_id,
    })
}

/// Rebuilds the exact assignment behind a durable OfferMayBeSent or
/// AwaitingSelection row. Journal fields are only lookup claims: the source
/// tree, V2 closure, Turso attempt, current worker grant, and input admission
/// are all reopened and checked before a transport or selection path can use
/// the returned values.
fn recover_compiler_route(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    authority: &super::super::semantic_authority::SemanticAuthority,
    identity: &super::super::pending_stored::PendingAckAssignmentIdentity,
    captured: &super::super::pending_stored::PendingAckCapturedWork,
) -> Result<RecoveredCompilerRoute, String> {
    use backend_engine::application::{
        CompilerAssignment, CompilerAttemptToken, CompilerWorkIdentity, FullWorkspaceInputClaim,
        VerifiedCompilerInput, VerifierAcceptedFullWorkspaceInput,
    };
    use backend_replication::{AttemptId, Fence};

    let key = identity
        .product_key
        .product_semantic_key()
        .map_err(|error| format!("reopen pending compiler product key: {error}"))?;
    let namespace = super::super::semantic_authority::SemanticAuthority::namespace(
        key.package(),
        key.coordinate(),
        key.profile(),
    )
    .map_err(|error| format!("rebuild pending compiler namespace: {error}"))?;
    let namespace_id = namespace.namespace_id();
    if namespace_id != identity.namespace_id {
        return Err("pending assignment namespace differs from canonical product key".to_owned());
    }

    let store = owner.store();
    let capture_objects = usize::try_from(captured.object_count)
        .map_err(|_| "persisted compiler capture object count exceeds this target".to_owned())?;
    let capture = CapturedFullWorkspaceV2::reopen_pinned_in_store(
        &store,
        captured.reopen_expectation(identity.work_id, identity.trust),
        backend_store::ArtifactBudget::new(
            capture_objects,
            capture_objects,
            captured.payload_bytes,
            64 * 1024,
            200_000,
        ),
    )
    .map_err(|error| format!("reopen pinned V2 compiler capture: {error}"))?;
    if capture.source_fence_digest() != captured.source_fence_digest
        || capture.input_root() != captured.input_root
        || capture.payload_bytes() != captured.payload_bytes
        || capture.manifest().recipe() != identity.trust.recipe
        || capture.manifest().profile()
            != LanguageProfile::try_from(identity.trust.profile)
                .map_err(|_| "pending compiler profile is invalid".to_owned())?
        || capture.manifest().stage()
            != backend_semantic::vocabulary::Stage::try_from(identity.trust.stage)
                .map_err(|_| "pending compiler stage is invalid".to_owned())?
        || capture.manifest().toolchain() != identity.trust.toolchain
        || capture.manifest().environment() != identity.trust.environment
        || capture.manifest().target_platform() != identity.trust.target_platform
    {
        return Err("reopened compiler capture differs from persisted invocation facts".to_owned());
    }

    let source_root = Path::new(captured.source_root.as_ref());
    let snapshot = ingest::CompilerWorkspaceSnapshot::open(source_root)
        .map_err(|error| format!("re-scan source workspace for pending compile: {error}"))?;
    if snapshot.root_path().to_str() != Some(captured.source_root.as_ref())
        || snapshot.fence_digest() != captured.source_fence_digest
        || !snapshot
            .revalidate()
            .map_err(|error| format!("revalidate pending source workspace: {error}"))?
    {
        return Err("pending compiler source fence is stale".to_owned());
    }

    let attempt = authority
        .recover_candidate_attempt(&captured.recovery_claim(namespace.clone()))
        .map_err(|error| format!("reopen exact Turso compiler attempt: {error}"))?;
    if attempt.namespace() != &namespace
        || attempt.epoch() != captured.turso_epoch
        || attempt.attempt_id() != &captured.turso_attempt_id
        || attempt.fence_bytes() != captured.turso_fence
        || attempt.input_digest() != &captured.turso_input_digest
        || attempt.base_generation() != captured.turso_base_generation
        || attempt.base_root() != captured.turso_base_root
        || attempt.observation().sequence() != captured.source_observation_sequence
        || attempt.observation().observation().revision()
            != Some(captured.source_observation_revision)
        || attempt.observation().observation().observed_at_ms()
            != captured.source_observation_observed_at_ms
        || !matches!(
            attempt.observation().observation().value(),
            backend_extension_turso::SourceObservationValue::KnownCount(count)
                if *count == captured.source_observation_count
        )
        || !authority
            .source_observation_is_current(&key, attempt.observation())
            .map_err(|error| format!("recheck pending source observation: {error}"))?
    {
        return Err("pending Turso attempt or source observation changed".to_owned());
    }

    let manifest_identity = capture
        .manifest()
        .identity_claim()
        .map_err(|error| format!("admit reopened compiler identity: {error}"))?;
    let input_claim = FullWorkspaceInputClaim {
        identity: manifest_identity,
        input_closure_id: *capture.closure().as_bytes(),
        manifest_object_id: *capture.manifest_object_id().as_bytes(),
    };
    let input = VerifierAcceptedFullWorkspaceInput::admit(
        input_claim,
        &CapturedFullWorkspaceVerifier {
            capture: &capture,
            store: &store,
        },
    )
    .map_err(|error| format!("verify reopened compiler input claim: {error}"))?;
    let work = CompilerWorkIdentity::new(
        manifest_identity.scope.package,
        manifest_identity.scope.target,
        manifest_identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(input),
        None,
        capture.manifest().max_output_bytes(),
    )
    .map_err(|error| format!("rebuild exact compiler work identity: {error}"))?;
    let peer = backend_engine::application::CompilerPeerId::new(identity.worker_peer)
        .map_err(|error| format!("reopen assigned worker identity: {error}"))?;
    let token = CompilerAttemptToken::new(
        AttemptId::new(identity.assignment_attempt)
            .map_err(|error| format!("reopen scheduler attempt ordinal: {error}"))?,
        Fence::from_bytes(identity.assignment_fence)
            .map_err(|error| format!("reopen scheduler attempt fence: {error}"))?,
    );
    let assignment = CompilerAssignment::recover_exact(
        work,
        token,
        peer,
        identity.work_id,
        token.attempt(),
        token.fence(),
    )
    .map_err(|error| format!("rebuild exact remote compiler assignment: {error}"))?;
    let evidence = CompilerInputAdmissionEvidence::new(
        assignment,
        namespace_id,
        capture.clone(),
        captured.source_observation_revision,
        captured.source_fence_digest,
    )
    .map_err(|error| format!("rebind compiler input admission evidence: {error:?}"))?;
    let source_observation = attempt.observation().clone();
    let verifier = ProductCompilerInputAdmissionVerifier {
        capture: &capture,
        snapshot: &snapshot,
        store: &store,
        authority,
        key: &key,
        observation: &source_observation,
        owner,
    };
    let input_admission = VerifiedCompilerInputAdmission::admit(evidence, &verifier)
        .map_err(|error| format!("readmit current compiler input and worker trust: {error:?}"))?;
    let policy = owner
        .trusted_workers()
        .map_err(|error| format!("reload current compiler worker policy: {error}"))?;
    let worker_grant = policy
        .grants()
        .iter()
        .find(|grant| {
            *grant.peer().as_bytes() == identity.worker_peer
                && grant.namespace_id() == namespace_id
                && grant.recipe() == identity.trust.recipe
                && <[u8; 2]>::from(grant.profile()) == identity.trust.profile
                && u8::from(grant.stage()) == identity.trust.stage
                && grant.toolchain() == identity.trust.toolchain
                && grant.environment() == identity.trust.environment
                && grant.target_platform() == identity.trust.target_platform
        })
        .cloned()
        .ok_or_else(|| {
            "assigned worker is no longer trusted for the exact invocation".to_owned()
        })?;
    let verified_capture = capture
        .verify_in_store(&store)
        .map_err(|error| format!("verify reopened full-workspace closure: {error}"))?;
    let expected_artifacts = u32::try_from(
        verified_capture
            .workspace_records()
            .filter(|record| {
                matches!(
                    record,
                    backend_engine::compiler_cluster_transport::CompilerInputTreeRecordV2::File {
                        role: backend_engine::compiler_cluster_transport::CompilerWorkspaceFileRoleV2::Source,
                        ..
                    }
                )
            })
            .count(),
    )
    .map_err(|_| "captured source count exceeds the compiler bound".to_owned())?;
    if expected_artifacts == 0 || !snapshot.revalidate().map_err(|error| error.to_string())? {
        return Err(
            "reopened capture has no complete sources or its source fence changed".to_owned(),
        );
    }

    Ok(RecoveredCompilerRoute {
        key,
        snapshot,
        capture,
        attempt,
        assignment,
        input_admission,
        worker_grant,
        source_observation,
        expected_artifacts,
    })
}

/// Rechecks the complete persisted source fence without treating a changed
/// inventory as an I/O failure. A successful walk whose digest or revisions
/// differ is a definitive stale-source observation; the caller may then
/// terminally reject a still-unselected result, but may never select it.
fn compiler_source_fence_is_current(
    captured: &super::super::pending_stored::PendingAckCapturedWork,
) -> Result<bool, String> {
    let root = Path::new(captured.source_root.as_ref());
    let snapshot = ingest::CompilerWorkspaceSnapshot::open(root)
        .map_err(|error| format!("re-scan pending compiler source workspace: {error}"))?;
    if snapshot.root_path().to_str() != Some(captured.source_root.as_ref()) {
        return Err("pending compiler source root no longer resolves canonically".to_owned());
    }
    let revisions_current = snapshot
        .revalidate()
        .map_err(|error| format!("revalidate pending compiler source fence: {error}"))?;
    Ok(revisions_current && snapshot.fence_digest() == captured.source_fence_digest)
}

fn verify_observation_invalidation_proof(
    proof: &backend_extension_turso::AttemptInvalidatedByObservationProof,
    namespace: &backend_extension_turso::AuthorityNamespace,
    captured: &super::super::pending_stored::PendingAckCapturedWork,
) -> Result<(), String> {
    if proof.namespace() != namespace
        || proof.attempt_id() != &captured.turso_attempt_id
        || proof.epoch() != captured.turso_epoch
        || proof.fence() != &captured.turso_fence
        || proof.input_digest() != &captured.turso_input_digest
        || proof.attempt_observation_sequence() != captured.source_observation_sequence
        || proof.current_observation_sequence() <= captured.source_observation_sequence
    {
        return Err(
            "Turso observation invalidation proof differs from durable compiler attempt".to_owned(),
        );
    }
    Ok(())
}

/// Reconstructs only the exact assignment/result identity needed to retire a
/// stale, unselected result. The Turso attempt must still reopen as its exact
/// current state-0 attempt, or have a sealed proof that a newer source
/// observation invalidated it before replacement. The V2 input closure is
/// pinned and verified; this helper cannot authorize selection.
enum StaleSelectionAuthorization<'a> {
    CurrentAttempt,
    InvalidatedByObservation(&'a backend_extension_turso::AttemptInvalidatedByObservationProof),
}

fn recover_stale_selection_identity(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    authority: &super::super::semantic_authority::SemanticAuthority,
    identity: &super::super::pending_stored::PendingAckAssignmentIdentity,
    captured: &super::super::pending_stored::PendingAckCapturedWork,
    result_closure_id: [u8; 32],
    authorization: StaleSelectionAuthorization<'_>,
) -> Result<super::super::cluster_dispatch::CompilerResultIdentity, String> {
    use backend_engine::application::{
        CompilerAssignment, CompilerAttemptToken, CompilerWorkIdentity, FullWorkspaceInputClaim,
        VerifiedCompilerInput, VerifierAcceptedFullWorkspaceInput,
    };
    use backend_replication::{AttemptId, Fence};

    let key = identity
        .product_key
        .product_semantic_key()
        .map_err(|error| format!("reopen stale-result product key: {error}"))?;
    let namespace = super::super::semantic_authority::SemanticAuthority::namespace(
        key.package(),
        key.coordinate(),
        key.profile(),
    )
    .map_err(|error| format!("rebuild stale-result namespace: {error}"))?;
    if namespace.namespace_id() != identity.namespace_id {
        return Err("stale result journal has a noncanonical namespace".to_owned());
    }

    let store = owner.store();
    let capture_objects = usize::try_from(captured.object_count)
        .map_err(|_| "persisted capture object count exceeds this target".to_owned())?;
    let capture = CapturedFullWorkspaceV2::reopen_pinned_in_store(
        &store,
        captured.reopen_expectation(identity.work_id, identity.trust),
        backend_store::ArtifactBudget::new(
            capture_objects,
            capture_objects,
            captured.payload_bytes,
            64 * 1024,
            200_000,
        ),
    )
    .map_err(|error| format!("reopen pinned stale-result V2 capture: {error}"))?;
    if capture.payload_bytes() != captured.payload_bytes
        || capture.source_fence_digest() != captured.source_fence_digest
        || capture.input_root() != captured.input_root
    {
        return Err("stale-result V2 capture differs from its durable claim".to_owned());
    }

    match authorization {
        StaleSelectionAuthorization::CurrentAttempt => {
            let attempt = authority
                .recover_candidate_attempt(&captured.recovery_claim(namespace.clone()))
                .map_err(|error| format!("prove stale result remains unselected: {error}"))?;
            if attempt.namespace() != &namespace
                || attempt.epoch() != captured.turso_epoch
                || attempt.attempt_id() != &captured.turso_attempt_id
                || attempt.fence_bytes() != captured.turso_fence
                || attempt.input_digest() != &captured.turso_input_digest
                || attempt.base_generation() != captured.turso_base_generation
                || attempt.base_root() != captured.turso_base_root
                || attempt.observation().sequence() != captured.source_observation_sequence
                || attempt.observation().observation().revision()
                    != Some(captured.source_observation_revision)
                || attempt.observation().observation().observed_at_ms()
                    != captured.source_observation_observed_at_ms
                || !matches!(
                    attempt.observation().observation().value(),
                    backend_extension_turso::SourceObservationValue::KnownCount(count)
                        if *count == captured.source_observation_count
                )
            {
                return Err("stale result differs from the exact current Turso attempt".to_owned());
            }
        }
        StaleSelectionAuthorization::InvalidatedByObservation(proof) => {
            verify_observation_invalidation_proof(proof, &namespace, captured)?;
        }
    }

    let verified_capture = capture
        .verify_in_store(&store)
        .map_err(|error| format!("verify stale-result V2 capture: {error}"))?;
    if verified_capture.workspace_records().count() == 0 {
        return Err("stale-result V2 capture has no admitted workspace records".to_owned());
    }
    let manifest_identity = capture
        .manifest()
        .identity_claim()
        .map_err(|error| format!("read stale-result compiler identity: {error}"))?;
    let input_claim = FullWorkspaceInputClaim {
        identity: manifest_identity,
        input_closure_id: *capture.closure().as_bytes(),
        manifest_object_id: *capture.manifest_object_id().as_bytes(),
    };
    let input = VerifierAcceptedFullWorkspaceInput::admit(
        input_claim,
        &CapturedFullWorkspaceVerifier {
            capture: &capture,
            store: &store,
        },
    )
    .map_err(|error| format!("verify stale-result input identity: {error}"))?;
    let work = CompilerWorkIdentity::new(
        manifest_identity.scope.package,
        manifest_identity.scope.target,
        manifest_identity.scope.recipe,
        VerifiedCompilerInput::FullWorkspaceFresh(input),
        None,
        capture.manifest().max_output_bytes(),
    )
    .map_err(|error| format!("rebuild stale-result compiler work: {error}"))?;
    if work.transfer_work_id() != identity.work_id
        || work.recipe().as_ref() != &identity.trust.recipe
    {
        return Err("stale-result assignment work differs from its durable identity".to_owned());
    }
    let peer = backend_engine::application::CompilerPeerId::new(identity.worker_peer)
        .map_err(|error| format!("reopen stale-result worker identity: {error}"))?;
    let token = CompilerAttemptToken::new(
        AttemptId::new(identity.assignment_attempt)
            .map_err(|error| format!("reopen stale-result attempt ordinal: {error}"))?,
        Fence::from_bytes(identity.assignment_fence)
            .map_err(|error| format!("reopen stale-result attempt fence: {error}"))?,
    );
    let assignment = CompilerAssignment::recover_exact(
        work,
        token,
        peer,
        identity.work_id,
        token.attempt(),
        token.fence(),
    )
    .map_err(|error| format!("rebuild stale-result assignment: {error}"))?;
    let worker_endpoint =
        backend_engine::cluster_transport::EndpointId::from_bytes(&identity.worker_peer)
            .map_err(|_| "stale-result worker endpoint is invalid".to_owned())?;
    let worker_grant = crate::compiler_trust::TrustedCompilerWorkerGrant::new(
        worker_endpoint,
        identity.worker_address,
        identity.namespace_id,
        identity.trust.recipe,
        LanguageProfile::try_from(identity.trust.profile)
            .map_err(|_| "stale-result worker profile is invalid".to_owned())?,
        backend_semantic::vocabulary::Stage::try_from(identity.trust.stage)
            .map_err(|_| "stale-result worker stage is invalid".to_owned())?,
        identity.trust.toolchain,
        identity.trust.environment,
        identity.trust.target_platform,
    )
    .map_err(|error| format!("reconstruct stale-result worker grant: {error}"))?;
    if result_closure_id == [0; 32] {
        return Err("stale-result closure identity is empty".to_owned());
    }
    Ok(super::super::cluster_dispatch::CompilerResultIdentity {
        assignment,
        namespace_id: identity.namespace_id,
        worker_grant,
        closure_id: result_closure_id,
    })
}

fn reject_awaiting_selection(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    journal: &Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
    selection_id: [u8; 32],
    result: &super::super::cluster_dispatch::CompilerResultIdentity,
) -> Result<bool, String> {
    let rejection_journal = Arc::clone(journal);
    let mut journal_hook =
        |event: super::super::cluster_dispatch::CompilerResultJournalEvent<'_>| {
            use backend_engine::cluster_transport::ResultAckDisposition;

            match event {
            super::super::cluster_dispatch::CompilerResultJournalEvent::PrepareDisposition {
                identity,
                disposition,
            } => {
                if disposition
                    != ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Admission,
                    )
                {
                    return Err(
                        "stale selection recovery attempted a non-admission rejection".into(),
                    );
                }
                let prepared = rejection_journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .prepare_rejection_from_selection(selection_id, identity, disposition)
                    .map_err(|error| error.to_string())?;
                Ok(Some(prepared))
            }
            super::super::cluster_dispatch::CompilerResultJournalEvent::WorkerRetired {
                identity,
                retired,
            } => {
                rejection_journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .mark_rejected_awaiting_confirm(identity, retired)
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            _ => Err("stale selection rejection emitted an invalid journal event".into()),
        }
        };
    owner
        .reject_recovered_identity(result, &mut journal_hook)
        .map_err(|error| format!("retire stale unselected compiler result: {error}"))?;
    owner.wake_pending_ack_retry_worker();
    Ok(true)
}

/// Finishes a persisted `AwaitingSelection` row after an owner crash that
/// happened between the durable ACK intent and Turso compare-and-select.
/// Every output byte is read back from its pinned worker closure and admitted
/// through the same V2 coordinator boundary used by a live result.
pub(in crate::builtin) fn recover_awaiting_selection(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    authority: &mut super::super::semantic_authority::SemanticAuthority,
    journal: &Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
    record: &super::super::pending_stored::PendingStoredAckRecord,
) -> Result<bool, String> {
    use backend_store::{ArtifactBudget, ArtifactClosureClaim};

    if record.state != super::super::pending_stored::PendingStoredAckState::AwaitingSelection {
        return Ok(false);
    }
    let _selection_attempt_guard =
        super::super::pending_stored::PendingStoredAckJournal::guard_selection_attempt(
            journal,
            record.id(),
        )
        .map_err(|error| format!("claim pending semantic selection recovery: {error}"))?;
    let identity = record.assignment_identity();
    let key = identity
        .product_key
        .product_semantic_key()
        .map_err(|error| format!("reopen pending selection product key: {error}"))?;
    let namespace = super::super::semantic_authority::SemanticAuthority::namespace(
        key.package(),
        key.coordinate(),
        key.profile(),
    )
    .map_err(|error| format!("rebuild pending selection namespace: {error}"))?;
    if namespace.namespace_id() != identity.namespace_id {
        return Err("pending selection namespace differs from canonical product key".to_owned());
    }
    let attempt_claim = record.captured_work.recovery_claim(namespace.clone());
    if let Some(proof) = authority
        .prove_attempt_invalidated_by_observation(&attempt_claim)
        .map_err(|error| format!("classify pending selection observation: {error}"))?
    {
        verify_observation_invalidation_proof(&proof, &namespace, &record.captured_work)?;
        let result = recover_stale_selection_identity(
            owner,
            authority,
            &identity,
            &record.captured_work,
            record.worker_closure_id,
            StaleSelectionAuthorization::InvalidatedByObservation(&proof),
        )?;
        return reject_awaiting_selection(owner, journal, record.id(), &result);
    }
    if !compiler_source_fence_is_current(&record.captured_work)? {
        let result = recover_stale_selection_identity(
            owner,
            authority,
            &identity,
            &record.captured_work,
            record.worker_closure_id,
            StaleSelectionAuthorization::CurrentAttempt,
        )?;
        return reject_awaiting_selection(owner, journal, record.id(), &result);
    }
    let recovered = recover_compiler_route(owner, authority, &identity, &record.captured_work)?;
    let store = owner.store();
    let result_objects = usize::try_from(record.worker_object_count)
        .map_err(|_| "stored worker object count exceeds this target".to_owned())?;
    let output_pin = store
        .reopen_pinned_stored_closure(
            ArtifactClosureClaim::from_bytes(record.worker_closure_id),
            ArtifactBudget::new(
                result_objects,
                result_objects,
                record.worker_bytes_verified,
                64 * 1024,
                1_000_000,
            ),
        )
        .map_err(|error| format!("reopen pinned worker result closure: {error:?}"))?;
    if output_pin.receipt().closure().as_bytes() != &record.worker_closure_id
        || output_pin.receipt().object_count() != u64::from(record.worker_object_count)
        || output_pin.receipt().bytes_verified() != record.worker_bytes_verified
    {
        return Err("reopened worker result closure differs from durable receipt".to_owned());
    }
    let recovered_receipt = output_pin
        .admit_recovered_payload(record.worker_payload_bytes)
        .map_err(|error| format!("admit recovered worker payload total: {error:?}"))?;
    let admitted = backend_engine::application::readmit_remote_compiler_candidate(
        &store,
        recovered.assignment,
        recovered.attempt.namespace().namespace_id(),
        record.worker_result_receipt,
        &output_pin,
        &recovered.capture,
        recovered.input_admission.clone(),
        ArtifactBudget::new(
            result_objects,
            result_objects,
            record.worker_bytes_verified,
            64 * 1024,
            1_000_000,
        ),
    )
    .map_err(|error| format!("readmit durable remote compiler result: {error}"))?;
    let expected_peer = match recovered.assignment.route() {
        backend_engine::application::CompilerAssignmentRoute::Remote(peer) => peer,
        backend_engine::application::CompilerAssignmentRoute::Local => {
            return Err("recovered compiler assignment is not routed to a worker".to_owned());
        }
    };
    if admitted.receipt() != recovered_receipt
        || admitted.candidate().closure_receipt() != recovered_receipt
        || admitted.candidate().peer() != expected_peer
    {
        return Err("readmitted result changed its exact worker receipt or assignment".to_owned());
    }

    let selected = authority
        .publish_checked_remote(
            &recovered.key,
            &recovered.source_observation,
            recovered.attempt,
            admitted,
            recovered.expected_artifacts,
            recovered.snapshot.fence_digest(),
            |candidate| {
                if !recovered.snapshot.revalidate().map_err(BuiltinModelError)? {
                    return Err(BuiltinModelError(
                        "source fence changed before recovered semantic selection".to_owned(),
                    ));
                }
                journal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .check_awaiting_selection_candidate(&record.id(), candidate)
                    .map_err(|error| {
                        BuiltinModelError(format!(
                            "recovered candidate differs from durable selection intent: {error}"
                        ))
                    })
            },
        )
        .map_err(|error| format!("complete recovered semantic selection: {error}"))?;
    let _selected = selected;
    // The source capture and both CAS pins remain alive through Turso commit.
    let _capture_pin = recovered.capture;
    let _result_pin = output_pin;
    Ok(true)
}

/// Reconciles an `OfferMayBeSent` reservation after owner restart. The caller
/// snapshots the row under the journal mutex, then this helper performs source,
/// Turso, CAS, trust, and network work without holding that mutex.
pub(in crate::builtin) fn recover_offered_reservation(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    authority: &mut super::super::semantic_authority::SemanticAuthority,
    journal: &Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
    id: [u8; 32],
) -> Result<bool, String> {
    use super::super::cluster_dispatch::{
        CompilerResultJournalEvent, RecoveredCompilerAssignmentOutcome,
    };
    use backend_engine::application::CompilerAttemptToken;
    use backend_replication::{AttemptId, Fence};

    let claims = journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .offered_recovery_claim(&id)
        .map_err(|error| format!("read durable offered-assignment claim: {error}"))?;
    let reservation = journal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .reservation_for_recovery(&id)
        .map_err(|error| format!("reopen offered-assignment reservation: {error}"))?;
    if claims.owner_endpoint_id != *owner.owner_id().as_bytes() {
        return Err("offered assignment belongs to a different owner endpoint".to_owned());
    }

    // Classify authority before rebuilding an executable route. A still-current
    // attempt with a changed source fence or revoked worker grant can only be
    // rejected as Admission. A newer Turso attempt requires the exact opaque
    // superseded proof and receives Rejected(Scope). Authority I/O failures
    // remain retryable and never become terminal dispositions.
    let key = claims
        .identity
        .product_key
        .product_semantic_key()
        .map_err(|error| format!("reopen offered compiler product key: {error}"))?;
    let namespace = super::super::semantic_authority::SemanticAuthority::namespace(
        key.package(),
        key.coordinate(),
        key.profile(),
    )
    .map_err(|error| format!("rebuild offered compiler namespace: {error}"))?;
    if namespace.namespace_id() != claims.identity.namespace_id {
        return Err("offered assignment namespace differs from its product key".to_owned());
    }
    let attempt_claim = claims.captured_work.recovery_claim(namespace.clone());
    match authority.recover_candidate_attempt(&attempt_claim) {
        Ok(_) => {
            let source_current = compiler_source_fence_is_current(&claims.captured_work)?;
            let persisted = reopen_persisted_compiler_assignment(
                owner,
                &claims.identity,
                &claims.captured_work,
            )?;
            let current_policy = owner
                .trusted_workers()
                .map_err(|error| format!("reload offered compiler worker trust: {error}"))?;
            let grant_current = current_policy
                .grants()
                .iter()
                .any(|grant| grant == &persisted.worker_grant);
            if !source_current || !grant_current {
                return terminalize_stale_offered_assignment(
                    owner,
                    journal,
                    reservation,
                    persisted,
                    StaleOfferedAuthorization::CurrentAttemptStale,
                );
            }
        }
        Err(current_error) => {
            let invalidated = authority
                .prove_attempt_invalidated_by_observation(&attempt_claim)
                .map_err(|error| {
                    format!("classify offered source-observation invalidation: {error}")
                })?;
            if let Some(proof) = invalidated {
                verify_observation_invalidation_proof(&proof, &namespace, &claims.captured_work)?;
                let persisted = reopen_persisted_compiler_assignment(
                    owner,
                    &claims.identity,
                    &claims.captured_work,
                )?;
                return terminalize_stale_offered_assignment(
                    owner,
                    journal,
                    reservation,
                    persisted,
                    StaleOfferedAuthorization::ObservationInvalidated(&proof),
                );
            }
            let proof = authority
                .prove_superseded_attempt(
                    &namespace,
                    claims.captured_work.turso_attempt_id,
                    claims.captured_work.turso_epoch,
                    claims.captured_work.turso_fence,
                    claims.captured_work.turso_input_digest,
                )
                .map_err(|error| format!("classify offered compiler attempt: {error}"))?;
            if let Some(proof) = proof {
                let persisted = reopen_persisted_compiler_assignment(
                    owner,
                    &claims.identity,
                    &claims.captured_work,
                )?;
                return terminalize_stale_offered_assignment(
                    owner,
                    journal,
                    reservation,
                    persisted,
                    StaleOfferedAuthorization::Superseded(&proof),
                );
            }
            return Err(format!(
                "offered compiler attempt is neither current nor durably superseded: {current_error}"
            ));
        }
    }

    let recovered_route =
        recover_compiler_route(owner, authority, &claims.identity, &claims.captured_work)?;
    let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
        recovered_route.assignment,
        claims.identity.namespace_id,
    )
    .map_err(|error| format!("rebuild offered assignment scope: {error}"))?;
    let token = CompilerAttemptToken::new(
        AttemptId::new(claims.identity.assignment_attempt)
            .map_err(|error| format!("reopen offered attempt ordinal: {error}"))?,
        Fence::from_bytes(claims.identity.assignment_fence)
            .map_err(|error| format!("reopen offered attempt fence: {error}"))?,
    );
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock precedes Unix epoch".to_owned())?
        .as_millis();
    let deadline_ms = u64::try_from(now_ms)
        .map_err(|_| "system clock exceeds owner clock range".to_owned())?
        .saturating_add(15 * 60 * 1_000);
    let recovered_assignment = owner
        .recover_assignment(
            recovered_route.assignment.work(),
            token,
            match recovered_route.assignment.route() {
                backend_engine::application::CompilerAssignmentRoute::Remote(peer) => peer,
                backend_engine::application::CompilerAssignmentRoute::Local => {
                    return Err("offered reservation no longer names a remote route".to_owned());
                }
            },
            scope,
            recovered_route.capture.clone(),
            recovered_route.input_admission.clone(),
            recovered_route.worker_grant.clone(),
            deadline_ms,
        )
        .map_err(|error| format!("admit recovered compiler assignment: {error}"))?;

    let reservation_journal = Arc::clone(journal);
    let selection_id = std::cell::Cell::new(None);
    let mut journal_hook = |event: CompilerResultJournalEvent<'_>| {
        let mut journal = reservation_journal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event {
            CompilerResultJournalEvent::RecoveredNoResultTerminal {
                assignment,
                namespace_id,
                status,
            } => {
                journal
                    .release_after_recovered_no_result(
                        reservation,
                        assignment,
                        namespace_id,
                        status,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            CompilerResultJournalEvent::PrepareDisposition {
                identity,
                disposition,
            } => {
                let prepared = match selection_id.get() {
                    Some(selection_id) => journal.prepare_rejection_from_selection(
                        selection_id,
                        identity,
                        disposition,
                    ),
                    None => journal.prepare_rejection(reservation, identity, disposition),
                }
                .map_err(|error| error.to_string())?;
                Ok(Some(prepared))
            }
            CompilerResultJournalEvent::WorkerRetired { identity, retired } => {
                journal
                    .mark_rejected_awaiting_confirm(identity, retired)
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            CompilerResultJournalEvent::OfferMayBeSent { .. }
            | CompilerResultJournalEvent::NoResultTerminal { .. } => {
                Err("cold result recovery emitted an invalid offer-stage event".to_owned())
            }
        }
    };
    let outcome = owner
        .recover_offered_assignment(owner.scheduler(), recovered_assignment, &mut journal_hook)
        .map_err(|error| format!("query retained result for offered assignment: {error}"))?;
    let checked = match outcome {
        RecoveredCompilerAssignmentOutcome::Candidate(checked) => checked,
        RecoveredCompilerAssignmentOutcome::NoResult => return Ok(true),
        RecoveredCompilerAssignmentOutcome::Running
        | RecoveredCompilerAssignmentOutcome::Retired(_) => return Ok(false),
    };

    let (admitted, pending, _output_pin) = checked.into_publication_parts();
    let worker_receipt = admitted.receipt();
    let worker_result_receipt = admitted.worker_result_receipt();
    let trust = super::super::pending_stored::PendingStoredAckTrust {
        recipe: recovered_route.worker_grant.recipe(),
        profile: <[u8; 2]>::from(recovered_route.worker_grant.profile()),
        stage: u8::from(recovered_route.worker_grant.stage()),
        toolchain: recovered_route.worker_grant.toolchain(),
        environment: recovered_route.worker_grant.environment(),
        target_platform: recovered_route.worker_grant.target_platform(),
    };
    let source_root = recovered_route.snapshot.root_path().to_path_buf();
    let product_key = claims.identity.product_key.clone();
    let selection_attempt_guard = std::cell::RefCell::new(None);
    let published = authority.publish_checked_remote(
        &recovered_route.key,
        &recovered_route.source_observation,
        recovered_route.attempt,
        admitted,
        recovered_route.expected_artifacts,
        recovered_route.snapshot.fence_digest(),
        |candidate| {
            if !recovered_route
                .snapshot
                .revalidate()
                .map_err(BuiltinModelError)?
            {
                return Err(BuiltinModelError(
                    "source fence changed before recovered semantic selection".to_owned(),
                ));
            }
            let row =
                super::super::pending_stored::PendingStoredAckRecord::from_selected_candidate(
                    claims.owner_endpoint_id,
                    recovered_route.worker_grant.address(),
                    claims.identity.namespace_id,
                    product_key,
                    pending.assignment,
                    worker_receipt,
                    candidate,
                    trust,
                    worker_result_receipt,
                    &recovered_route.capture,
                    &source_root,
                )
                .map_err(|error| {
                    BuiltinModelError(format!("build recovered selection ACK row: {error}"))
                })?;
            let row_id = row.id();
            let (prepared, guard) =
                super::super::pending_stored::PendingStoredAckJournal::prepare_selection_guarded(
                    &reservation_journal,
                    reservation,
                    row,
                )
                .map_err(|error| {
                    BuiltinModelError(format!("persist recovered selection ACK intent: {error}"))
                })?;
            let _ = prepared;
            selection_attempt_guard.replace(Some(guard));
            selection_id.set(Some(row_id));
            if !recovered_route
                .snapshot
                .revalidate()
                .map_err(BuiltinModelError)?
            {
                return Err(BuiltinModelError(
                    "source fence changed while persisting recovered selection intent".to_owned(),
                ));
            }
            Ok(())
        },
    );
    drop(selection_attempt_guard.borrow_mut().take());
    if let Err(error) = published {
        // Before the callback the row remains OfferMayBeSent; after it the
        // AwaitingSelection intent remains durable. Both states are retried.
        owner.wake_pending_ack_retry_worker();
        return Err(format!("publish recovered remote compiler result: {error}"));
    }
    owner.wake_pending_ack_retry_worker();
    // Retain both CAS pins and the full workspace pin through selection.
    let _capture_pin = recovered_route.capture;
    let _output_pin = _output_pin;
    Ok(true)
}

/// Queries a stale assignment only after exact source/current-grant rejection
/// or a sealed Turso superseded proof. The journal records the selected
/// rejection disposition before any ACK and retains the retirement-confirm
/// stage for the normal retry worker.
fn terminalize_stale_offered_assignment(
    owner: &super::super::cluster_dispatch::OwnerCompilerClusterRuntime,
    journal: &Arc<Mutex<super::super::pending_stored::PendingStoredAckJournal>>,
    reservation: super::super::pending_stored::PendingAckCapacityReservation,
    persisted: ReopenedPersistedCompilerAssignment,
    authorization: StaleOfferedAuthorization<'_>,
) -> Result<bool, String> {
    use super::super::cluster_dispatch::{
        CompilerResultJournalEvent, StaleOfferedAssignmentOutcome,
    };

    let journal_for_hook = Arc::clone(journal);
    let mut journal_hook = |event: CompilerResultJournalEvent<'_>| {
        let mut journal = journal_for_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event {
            CompilerResultJournalEvent::RecoveredNoResultTerminal {
                assignment,
                namespace_id,
                status,
            } => {
                journal
                    .release_after_recovered_no_result(
                        reservation,
                        assignment,
                        namespace_id,
                        status,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            CompilerResultJournalEvent::PrepareDisposition {
                identity,
                disposition,
            } => {
                let prepared = journal
                    .prepare_rejection(reservation, identity, disposition)
                    .map_err(|error| error.to_string())?;
                Ok(Some(prepared))
            }
            CompilerResultJournalEvent::WorkerRetired { identity, retired } => {
                journal
                    .mark_rejected_awaiting_confirm(identity, retired)
                    .map_err(|error| error.to_string())?;
                Ok(None)
            }
            _ => Err("stale offered cleanup emitted an invalid journal event".to_owned()),
        }
    };
    let outcome = match authorization {
        StaleOfferedAuthorization::Superseded(proof) => owner.reject_superseded_offered_assignment(
            persisted.assignment,
            persisted.namespace_id,
            persisted.worker_grant.clone(),
            proof,
            &mut journal_hook,
        ),
        StaleOfferedAuthorization::CurrentAttemptStale
        | StaleOfferedAuthorization::ObservationInvalidated(_) => owner
            .reject_stale_offered_assignment(
                persisted.assignment,
                persisted.namespace_id,
                persisted.worker_grant.clone(),
                &mut journal_hook,
            ),
    }
    .map_err(|error| format!("query and retire stale offered result: {error}"))?;
    // Preserve the V2 input pin through the authenticated exact-scope query.
    let _capture_pin = persisted.capture;
    match outcome {
        StaleOfferedAssignmentOutcome::NoResult
        | StaleOfferedAssignmentOutcome::RejectedAdmission
        | StaleOfferedAssignmentOutcome::RejectedSuperseded => {
            owner.wake_pending_ack_retry_worker();
            Ok(true)
        }
        StaleOfferedAssignmentOutcome::Running => Ok(false),
        StaleOfferedAssignmentOutcome::Retired(retired) => {
            let disposition = match authorization {
                StaleOfferedAuthorization::Superseded(_) => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Scope,
                    )
                }
                StaleOfferedAuthorization::CurrentAttemptStale
                | StaleOfferedAuthorization::ObservationInvalidated(_) => {
                    backend_engine::cluster_transport::ResultAckDisposition::Rejected(
                        backend_engine::cluster_transport::ResultRejectReason::Admission,
                    )
                }
            };
            let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
                persisted.assignment,
                persisted.namespace_id,
            )
            .map_err(|error| format!("rebuild stale offered result scope: {error}"))?;
            let ack = backend_engine::cluster_transport::ControlResultAck::new(
                scope,
                retired.closure_id(),
                disposition,
            )
            .map_err(|error| format!("validate prior worker retirement identity: {error}"))?;
            if !retired.matches_ack(&ack, persisted.worker_grant.peer()) {
                return Err(
                    "prior worker retirement differs from the exact stale-result disposition"
                        .to_owned(),
                );
            }
            let result = super::super::cluster_dispatch::CompilerResultIdentity {
                assignment: persisted.assignment,
                namespace_id: persisted.namespace_id,
                worker_grant: persisted.worker_grant,
                closure_id: retired.closure_id(),
            };
            let mut journal_guard = journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            journal_guard
                .prepare_rejection(reservation, &result, disposition)
                .and_then(|_| journal_guard.mark_rejected_awaiting_confirm(&result, &retired))
                .map_err(|error| {
                    format!("persist prior stale-result retirement receipt: {error}")
                })?;
            owner.wake_pending_ack_retry_worker();
            Ok(true)
        }
    }
}

#[derive(Clone, Copy)]
enum StaleOfferedAuthorization<'a> {
    CurrentAttemptStale,
    ObservationInvalidated(&'a backend_extension_turso::AttemptInvalidatedByObservationProof),
    Superseded(&'a backend_extension_turso::SupersededAttemptProof),
}

impl CompilerInputAdmissionVerifier for ProductCompilerInputAdmissionVerifier<'_> {
    fn verify_source_observation(
        &self,
        evidence: &CompilerInputAdmissionEvidence,
    ) -> Result<(), CompilerInputAdmissionError> {
        let current = self
            .authority
            .source_observation_is_current(self.key, self.observation)
            .unwrap_or(false);
        let scanner_current = self.snapshot.revalidate().unwrap_or(false);
        let capture_valid = evidence.capture().closure() == self.capture.closure()
            && evidence.source_fence_digest() == self.snapshot.fence_digest()
            && evidence.capture().verify_in_store(self.store).is_ok();
        if !current || !scanner_current || !capture_valid {
            return Err(CompilerInputAdmissionError::SourceAuthorityRejected);
        }
        Ok(())
    }

    fn authorize_trusted_worker(
        &self,
        peer: backend_engine::application::CompilerPeerId,
        namespace_id: [u8; 16],
        manifest: &backend_engine::compiler_cluster_transport::CompilerInputManifestV2,
    ) -> bool {
        let Ok(peer) = backend_engine::cluster_transport::EndpointId::from_bytes(&peer.as_bytes())
        else {
            return false;
        };
        self.owner.trusted_workers().is_ok_and(|policy| {
            policy.authorizes(
                peer,
                namespace_id,
                manifest.recipe(),
                manifest.profile(),
                manifest.stage(),
                manifest.toolchain(),
                manifest.environment(),
                manifest.target_platform(),
            )
        })
    }
}

fn compiler_capture_budget() -> backend_store::StreamingClosureBudget {
    backend_store::StreamingClosureBudget::new(
        100_002,
        512 * 1024 * 1024,
        512 * 1024 * 1024,
        64 * 1024,
        200_000,
        64 * 1024 * 1024,
    )
}

fn portable_relative_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_lockfile(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some(
            "Cargo.lock"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "poetry.lock"
                | "uv.lock"
                | "go.sum"
                | "go.work.sum"
        )
    )
}

fn is_compiler_configuration(path: &str) -> bool {
    matches!(
        Path::new(path).file_name().and_then(|name| name.to_str()),
        Some(
            "Cargo.toml"
                | "tsconfig.json"
                | "jsconfig.json"
                | "pyproject.toml"
                | "setup.cfg"
                | "go.mod"
                | "go.work"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "Directory.Build.props"
                | "*.csproj"
                | "CMakeLists.txt"
        )
    ) || path.ends_with(".csproj")
}

fn semantic_input_digest(scan: &ingest::IndexSnapshot, profile: LanguageProfile) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.local-service.semantic-input.v2\0");
    hasher.update(&scan.source_version);
    hasher.update(&<[u8; 2]>::from(profile));
    for source in scan
        .compiler_sources
        .iter()
        .filter(|source| source.profile == profile)
    {
        let path = source.relative_path.as_bytes();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(&source.content);
    }
    for source in scan
        .reused_compiler_files
        .iter()
        .filter(|source| source.profile == profile)
    {
        let path = source.relative_path.as_bytes();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
        hasher.update(&source.content);
    }
    for configuration in scan
        .compiler_configuration
        .files
        .iter()
        .filter(|file| file.language == profile.language())
    {
        let path = configuration.relative_path.to_string_lossy();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&configuration.content);
    }
    hasher.update(&[u8::from(
        scan.compiler_configuration
            .complete_languages
            .contains(&profile.language()),
    )]);
    *hasher.finalize().as_bytes()
}

/// Selects the exact profile one source is compiled under.
fn compile_profile(source_root: &Path, source: &ingest::CompilerSource) -> LanguageProfile {
    ingest::compilation_profile(source_root, &source.relative_path, source.profile)
}

pub(super) fn remove_project_intent(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: backend_engine::PackageKey,
    label: &str,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<Option<BuiltinIntent>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let Some(record) = relation
        .lookup(&package.to_bytes())
        .map_err(|error| BuiltinModelError(format!("read indexed project: {error}")))?
    else {
        return Ok(None);
    };
    let project_fields = record
        .project_fields()
        .ok_or_else(|| BuiltinModelError("project key contains a source file record".to_owned()))?;
    let files = super::super::profile::resolve_project_file_keys(
        package.to_bytes(),
        &record,
        |page_key| {
            relation.lookup(page_key).map_err(|error| {
                BuiltinModelError(format!("read indexed membership page: {error}"))
            })
        },
    )?;
    let file_rows = relation
        .lookup_many_sorted(&files)
        .map_err(|error| BuiltinModelError(format!("read indexed source files: {error}")))?;
    for (key, file) in files.iter().copied().zip(file_rows) {
        let file = file.ok_or_else(|| {
            BuiltinModelError("project frontier refers to a missing source file".to_owned())
        })?;
        super::super::profile::validate_project_file(package.to_bytes(), key, &file)?;
    }
    let membership_pages = project_fields.files.page_keys().to_vec();
    let semantic = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut semantic_changes = Vec::new();
    let mut after = None;
    loop {
        let page = semantic
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| BuiltinModelError(format!("page semantic publications: {error}")))?;
        for (key, record) in page.entries() {
            if key.package_key() != package || !key.is_selected() {
                continue;
            }
            let ProductSemanticPublicationRecord::Published { .. } = record else {
                continue;
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"backend.local-service.semantic-package-removed.v1\0");
            hasher.update(package.as_bytes());
            hasher.update(&<[u8; 2]>::from(key.profile()));
            semantic_authority.observe(key, *hasher.finalize().as_bytes(), 0)?;
            semantic_changes.push(BuiltinSemanticChange {
                key: key.clone(),
                after: Some(ProductSemanticPublicationRecord::Unavailable(
                    backend_engine::builtin::SemanticUnavailableReason::ProjectAuthority,
                )),
            });
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    BuiltinIntent::remove_project(package, label, &files, &membership_pages, semantic_changes)
        .map(Some)
}

pub(super) fn semantic_version_record(
    key: &ProductSemanticPublicationKey,
    coverage: SemanticPublicationCoverage,
    claim: SemanticPublicationClaim,
    selected: bool,
    freshness: backend_engine::SemanticVersionFreshness,
) -> backend_engine::SemanticVersionRecord {
    let binding = claim.binding();
    let manifest = claim.manifest();
    backend_engine::SemanticVersionRecord {
        package: key.package().clone(),
        coordinate: key.coordinate().clone(),
        profile: backend_engine::SemanticLanguageProfile::new(key.profile()),
        generation: backend_engine::SemanticGenerationId::new(*binding.identity.as_ref()),
        generation_root: *binding.generation.pinned_root.as_ref(),
        dependency_set: *binding.generation.dep_set.as_ref(),
        manifest: *manifest.identity.as_ref(),
        artifacts: manifest.fragment_count,
        semantic_bytes: manifest.byte_length,
        complete: matches!(coverage, SemanticPublicationCoverage::Complete),
        selected,
        freshness,
        history_status: backend_engine::SemanticHistoryPublicationStatus::NotSelected,
        selected_source_frontier: None,
    }
}

fn selected_project_source_frontier(
    snapshot: &backend_engine::WorkspaceSnapshot,
    package: &backend_engine::PackageReference,
) -> Result<Option<backend_engine::SelectedProjectSourceFrontier>, BuiltinModelError> {
    let backend_engine::PackageReference::Local(label) = package else {
        return Ok(None);
    };
    let package_key = backend_engine::PackageKey::from_value(label.as_str());
    let source_key = package_key.to_bytes();
    let relation = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open selected source relation: {error}")))?;
    let Some(record) = relation
        .lookup(&source_key)
        .map_err(|error| BuiltinModelError(format!("read selected source Project row: {error}")))?
    else {
        return Ok(None);
    };
    let fields = record.project_fields().ok_or_else(|| {
        BuiltinModelError("selected source key does not name a Project row".to_owned())
    })?;
    if fields.label != label.as_str()
        || backend_engine::PackageKey::from_value(fields.label) != package_key
    {
        return Err(BuiltinModelError(
            "selected source frontier does not match the requested local package".to_owned(),
        ));
    }
    let file_keys =
        super::super::profile::resolve_project_file_keys(source_key, &record, |page_key| {
            relation
                .lookup(page_key)
                .map_err(|error| BuiltinModelError(format!("read source membership page: {error}")))
        })?;
    match relation
        .visit_many_sorted(&file_keys, |file_key, file| match file {
            Some(file) => {
                match super::super::profile::validate_project_file(source_key, *file_key, file) {
                    Ok(()) => std::ops::ControlFlow::Continue(()),
                    Err(error) => std::ops::ControlFlow::Break(error),
                }
            }
            None => std::ops::ControlFlow::Break(BuiltinModelError(
                "Project membership refers to a missing source file row".to_owned(),
            )),
        })
        .map_err(|error| {
            BuiltinModelError(format!(
                "validate selected Project file membership: {error}"
            ))
        })? {
        std::ops::ControlFlow::Continue(()) => {}
        std::ops::ControlFlow::Break(error) => return Err(error),
    }
    let file_count = u32::try_from(file_keys.len()).map_err(|_| {
        BuiltinModelError("selected source membership count exceeds u32".to_owned())
    })?;
    Ok(Some(backend_engine::SelectedProjectSourceFrontier {
        package: package.clone(),
        source_relation_root: *relation.root().as_bytes(),
        source_version: fields.source_version,
        file_count,
    }))
}

/// Reads one bounded page from the exact selected Project membership. The
/// relation root, Project version, and cursor positions all come from the same
/// immutable snapshot; each request reopens only the referenced membership
/// pages and the returned File rows.
pub(super) fn package_source_membership_page(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    request: &backend_library::PackageSourceMembershipPageRequestV1,
) -> Result<backend_library::PackageSourceMembershipPageResultV1, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    package_source_membership_page_from_snapshot(&snapshot, request)
}

pub(in crate::builtin) fn package_source_membership_page_from_snapshot(
    snapshot: &backend_engine::WorkspaceSnapshot,
    request: &backend_library::PackageSourceMembershipPageRequestV1,
) -> Result<backend_library::PackageSourceMembershipPageResultV1, BuiltinModelError> {
    use backend_engine::ProductProjectFileMembership;
    use backend_library::{
        PackageSourceMembershipCursorV1, PackageSourceMembershipExclusionsV1,
        PackageSourceMembershipFileV1, PackageSourceMembershipLanguageV1,
        PackageSourceMembershipPageResultV1, PackageSourceMembershipScopeV1,
        PackageSourceMembershipUnavailableV1,
    };

    if !request.has_admissible_shape() {
        return Err(BuiltinModelError(
            "package source membership request is malformed".to_owned(),
        ));
    }
    let backend_engine::PackageReference::Local(label) = &request.package else {
        return Err(BuiltinModelError(
            "source membership requires a local Project package".to_owned(),
        ));
    };
    let package = request.package.clone();
    let package_key = backend_engine::PackageKey::from_value(label.as_str());
    let project_key = package_key.to_bytes();
    let relation = match snapshot.relation::<BuiltinWorkspaceRelation>() {
        Ok(relation) => relation,
        Err(_) => {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
    };
    let source_relation_root = *relation.root().as_bytes();
    let project_record = match relation.lookup(&project_key) {
        Ok(Some(record)) => record,
        Ok(None) => {
            if request.expected_source_relation_root.is_some() || request.cursor.is_some() {
                return Ok(PackageSourceMembershipPageResultV1::Stale {
                    package,
                    current_source_relation_root: Some(source_relation_root),
                    current_source_version: None,
                });
            }
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::ProjectNotSelected,
            });
        }
        Err(_) => {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
    };
    let Some(project) = project_record.project_fields() else {
        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
            package,
            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
        });
    };
    if project.label != label.as_str()
        || project_key != backend_engine::package_key(project.label).to_bytes()
    {
        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
            package,
            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
        });
    }
    if request
        .expected_source_relation_root
        .is_some_and(|expected| expected != source_relation_root)
        || request
            .expected_source_version
            .is_some_and(|expected| expected != project.source_version)
    {
        return Ok(PackageSourceMembershipPageResultV1::Stale {
            package,
            current_source_relation_root: Some(source_relation_root),
            current_source_version: Some(project.source_version),
        });
    }

    let load_membership_page = |page_key: &[u8; 32]| -> Result<Vec<[u8; 32]>, String> {
        let page_record = relation
            .lookup(page_key)
            .map_err(|error| format!("read membership page: {error}"))?
            .ok_or_else(|| "selected Project membership page is missing".to_owned())?;
        let fields = page_record
            .membership_page_fields()
            .ok_or_else(|| "selected membership reference does not name a page".to_owned())?;
        if fields.project != project_key
            || fields.files.is_empty()
            || fields.files.len() > backend_engine::MAX_PROJECT_MEMBERSHIP_PAGE_FILES
            || fields.files.windows(2).any(|pair| pair[0] >= pair[1])
            || backend_engine::product_source_membership_page_key(project_key, fields.files)
                .map_or(true, |expected| expected != *page_key)
        {
            return Err("selected Project membership page identity is invalid".to_owned());
        }
        Ok(fields.files.to_vec())
    };

    let (file_count, inline_files, page_keys) = match project.files {
        ProductProjectFileMembership::Inline(files) => (files.len(), Some(files), None),
        ProductProjectFileMembership::Paged {
            file_count,
            page_keys,
        } => (file_count, None, Some(page_keys)),
    };
    if file_count > backend_library::MAX_SELECTED_PROJECT_FRONTIER_FILES {
        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
            package,
            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
        });
    }

    let mut start_offset = 0_u32;
    let mut page_index = 0_usize;
    let mut membership_offset = 0_usize;
    let mut active_page = None::<Vec<[u8; 32]>>;
    let mut previous_file_key = None;
    if let Some(cursor) = &request.cursor {
        if cursor.package != package || cursor.project_key != project_key {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
        if cursor.source_relation_root != source_relation_root
            || cursor.source_version != project.source_version
        {
            return Ok(PackageSourceMembershipPageResultV1::Stale {
                package,
                current_source_relation_root: Some(source_relation_root),
                current_source_version: Some(project.source_version),
            });
        }
        let (membership, actual_key) = match (inline_files, page_keys) {
            (Some(files), None) => {
                let offset = usize::from(cursor.membership_offset);
                if cursor.membership_page != 0 || files.get(offset).is_none() {
                    return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                        package,
                        reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                    });
                }
                (offset, files[offset])
            }
            (None, Some(pages)) => {
                page_index = usize::from(cursor.membership_page);
                let Some(page_key) = pages.get(page_index) else {
                    return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                        package,
                        reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                    });
                };
                let mut ordinal_before_page = 0_u32;
                for prior_page_key in pages.iter().take(page_index) {
                    let Ok(prior) = load_membership_page(prior_page_key) else {
                        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                            package,
                            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                        });
                    };
                    let Ok(prior_len) = u32::try_from(prior.len()) else {
                        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                            package,
                            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                        });
                    };
                    let Some(next) = ordinal_before_page.checked_add(prior_len) else {
                        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                            package,
                            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                        });
                    };
                    ordinal_before_page = next;
                }
                let Ok(current) = load_membership_page(page_key) else {
                    return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                        package,
                        reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                    });
                };
                let offset = usize::from(cursor.membership_offset);
                if current.get(offset).is_none()
                    || current[offset] != cursor.last_file_key
                    || ordinal_before_page.saturating_add(cursor.membership_offset as u32)
                        != cursor.ordinal
                {
                    return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                        package,
                        reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                    });
                }
                active_page = Some(current.clone());
                (offset, current[offset])
            }
            _ => {
                return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                    package,
                    reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                });
            }
        };
        if actual_key != cursor.last_file_key
            || (inline_files.is_some() && membership as u32 != cursor.ordinal)
            || cursor.ordinal.saturating_add(1) >= file_count as u32
        {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
        membership_offset = membership.saturating_add(1);
        start_offset = cursor.ordinal.saturating_add(1);
        previous_file_key = Some(cursor.last_file_key);
    }

    let mut files = Vec::with_capacity(usize::from(request.limit));
    let mut last_position = None::<(usize, usize, [u8; 32])>;
    let mut exhausted = false;
    while files.len() < usize::from(request.limit) {
        let next = if let Some(inline) = inline_files {
            let Some(key) = inline.get(membership_offset).copied() else {
                exhausted = true;
                break;
            };
            let position = membership_offset;
            membership_offset += 1;
            (key, 0, position)
        } else {
            let pages = page_keys.unwrap_or_default();
            let mut found = None;
            loop {
                if active_page.is_none() {
                    let Some(page_key) = pages.get(page_index) else {
                        exhausted = true;
                        break;
                    };
                    let Ok(page) = load_membership_page(page_key) else {
                        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                            package,
                            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                        });
                    };
                    if previous_file_key
                        .is_some_and(|previous| page.first().is_none_or(|first| previous >= *first))
                    {
                        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                            package,
                            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                        });
                    }
                    active_page = Some(page);
                }
                let Some(page) = active_page.as_ref() else {
                    return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                        package,
                        reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                    });
                };
                if let Some(key) = page.get(membership_offset).copied() {
                    let position = (key, page_index, membership_offset);
                    membership_offset += 1;
                    found = Some(position);
                    break;
                }
                active_page = None;
                membership_offset = 0;
                page_index = page_index.saturating_add(1);
            }
            let Some(found) = found else {
                break;
            };
            found
        };
        let (file_key, file_page, file_offset) = next;
        if previous_file_key.is_some_and(|previous| previous >= file_key)
            || start_offset >= file_count as u32
        {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
        let file_record = match relation.lookup(&file_key) {
            Ok(Some(record)) => record,
            _ => {
                return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                    package,
                    reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
                });
            }
        };
        if super::super::profile::validate_project_file(project_key, file_key, &file_record)
            .is_err()
        {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        }
        let Some(source) = file_record.file_fields() else {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        };
        files.push(PackageSourceMembershipFileV1 {
            file_key,
            path: source.path.to_owned(),
            language: PackageSourceMembershipLanguageV1::from(source.language),
            content_version: source.content_version,
            source_identity: source.source_identity.map(|identity| *identity.as_ref()),
        });
        previous_file_key = Some(file_key);
        start_offset += 1;
        last_position = Some((file_page, file_offset, file_key));
    }

    let has_more = if exhausted {
        false
    } else if let Some(inline) = inline_files {
        membership_offset < inline.len()
    } else {
        let pages = page_keys.unwrap_or_default();
        active_page
            .as_ref()
            .is_some_and(|page| membership_offset < page.len())
            || page_index.saturating_add(1) < pages.len()
    };
    if (!has_more && start_offset != file_count as u32)
        || (has_more && start_offset >= file_count as u32)
    {
        return Ok(PackageSourceMembershipPageResultV1::Unavailable {
            package,
            reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
        });
    }

    let next = if has_more {
        let Some((membership_page, membership_offset, last_file_key)) = last_position else {
            return Ok(PackageSourceMembershipPageResultV1::Unavailable {
                package,
                reason: PackageSourceMembershipUnavailableV1::SelectedRelationInvalid,
            });
        };
        Some(PackageSourceMembershipCursorV1 {
            schema: backend_library::PACKAGE_SOURCE_MEMBERSHIP_SCHEMA,
            package: package.clone(),
            project_key,
            source_relation_root,
            source_version: project.source_version,
            membership_page: u16::try_from(membership_page)
                .map_err(|_| BuiltinModelError("membership page cursor exceeds u16".to_owned()))?,
            membership_offset: u16::try_from(membership_offset)
                .map_err(|_| BuiltinModelError("membership file cursor exceeds u16".to_owned()))?,
            last_file_key,
            ordinal: start_offset - 1,
        })
    } else {
        None
    };
    Ok(PackageSourceMembershipPageResultV1::Page {
        package,
        project_key,
        source_relation_root,
        source_version: project.source_version,
        file_count: u32::try_from(file_count).map_err(|_| {
            BuiltinModelError("selected project source count exceeds u32".to_owned())
        })?,
        start_offset: if request.cursor.is_some() {
            request
                .cursor
                .as_ref()
                .map_or(0, |cursor| cursor.ordinal + 1)
        } else {
            0
        },
        scope: PackageSourceMembershipScopeV1::IndexedProjectMembership,
        exclusions: PackageSourceMembershipExclusionsV1::NotCaptured,
        files: files.into_boxed_slice(),
        next,
    })
}

pub(super) fn semantic_versions(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
    workspace: Option<&Path>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<Box<[backend_engine::SemanticVersionRecord]>, BuiltinModelError> {
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let selected_source_frontier = selected_project_source_frontier(&snapshot, package)?;
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic version history: {error}")))?;
    let capture_relation = semantic_capture_relation(&snapshot)
        .map_err(|error| BuiltinModelError(format!("open semantic capture history: {error}")))?;
    let mut selected = BTreeMap::<(PackageUrl, LanguageProfile), [u8; 32]>::new();
    let mut unavailable = None;
    let mut generations = Vec::new();
    let mut after = None;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|error| {
                BuiltinModelError(format!("page semantic version history: {error}"))
            })?;
        for (key, record) in page.entries() {
            if key.package() != package {
                continue;
            }
            key.admit_record(record).map_err(|error| {
                BuiltinModelError(format!("admit semantic version history: {error}"))
            })?;
            let (coverage, claim) = match record {
                ProductSemanticPublicationRecord::Published { coverage, claim } => {
                    (*coverage, *claim)
                }
                // One language whose authority is unavailable must not hide
                // the history every other language of the package published.
                // The typed refusal is kept for a package with no published
                // target at all, below.
                ProductSemanticPublicationRecord::Unavailable(reason) if key.is_selected() => {
                    let capture_outcome = capture_relation
                        .as_ref()
                        .map(|relation| relation.lookup(key))
                        .transpose()
                        .map_err(|error| {
                            BuiltinModelError(format!("read semantic capture outcome: {error}"))
                        })?
                        .flatten()
                        .map(|record| record.outcome());
                    let reason = match capture_outcome {
                        Some(ProductSemanticCaptureOutcome::Unavailable { reason })
                        | Some(ProductSemanticCaptureOutcome::Failed { reason, .. }) => reason,
                        _ => *reason,
                    };
                    unavailable.get_or_insert(reason);
                    continue;
                }
                ProductSemanticPublicationRecord::Unavailable(_) => continue,
            };
            let target = (key.coordinate().clone(), key.profile());
            match key.selection() {
                SemanticPublicationSelection::Selected => {
                    if selected
                        .insert(target, *claim.binding().identity.as_ref())
                        .is_some()
                    {
                        return Err(BuiltinModelError(
                            "semantic version history has duplicate selected targets".to_owned(),
                        ));
                    }
                }
                SemanticPublicationSelection::Generation(_) => {
                    if generations.len() >= backend_engine::MAX_PRODUCT_ROWS {
                        return Err(BuiltinModelError(
                            "semantic version history exceeds the product row bound".to_owned(),
                        ));
                    }
                    let selected_key = ProductSemanticPublicationKey::new(
                        key.package().clone(),
                        key.coordinate().clone(),
                        key.profile(),
                    )
                    .map_err(|error| {
                        BuiltinModelError(format!("admit selected semantic target: {error}"))
                    })?;
                    let freshness_key =
                        super::super::semantic_authority::SelectedSemanticPublicationKey::new(
                            &selected_key,
                        )
                        .map_err(|error| BuiltinModelError(error.to_owned()))?;
                    let freshness = semantic_authority.freshness(freshness_key, claim);
                    generations.push((
                        target,
                        selected_key,
                        claim,
                        semantic_version_record(
                            key, coverage, claim, false,
                            // Freshness observations are keyed by the live
                            // selected product, while this history row is
                            // keyed by its immutable generation. Query the
                            // selected key so a newly published generation is
                            // not incorrectly exposed as Unverified.
                            freshness,
                        ),
                    ));
                }
            }
        }
        let Some(next) = page.next().cloned() else {
            break;
        };
        after = Some(next);
    }
    if generations.is_empty() {
        let view = daemon.engine().daemon().library().view();
        let fallback =
            super::super::product_state::indexed_semantic_versions(view, package, workspace)
                .map_err(BuiltinModelError)?;
        if !fallback.is_empty() {
            return Ok(fallback);
        }
    }
    if selected.is_empty()
        && let Some(reason) = unavailable
    {
        return Err(BuiltinModelError(format!(
            "semantic publication unavailable: {reason}"
        )));
    }
    for (target, history_key, claim, record) in &mut generations {
        record.selected = selected.get(target).copied() == Some(record.generation.to_bytes());
        if record.selected {
            record.selected_source_frontier = selected_source_frontier.clone();
            record.history_status =
                semantic_authority.native_history_status(history_key, *claim)?;
        }
    }
    if selected.iter().any(|(selected_target, identity)| {
        !generations.iter().any(|(generation_target, _, _, record)| {
            generation_target == selected_target && record.generation.to_bytes() == *identity
        })
    }) {
        return Err(BuiltinModelError(
            "selected semantic generation is absent from immutable history".to_owned(),
        ));
    }
    Ok(generations
        .into_iter()
        .map(|(_, _, _, record)| record)
        .collect::<Vec<_>>()
        .into_boxed_slice())
}

#[cfg(test)]
mod compiler_input_witness_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn a_recompile_without_staged_cargo_facts_clears_prior_alias_authority()
    -> Result<(), &'static str> {
        use backend_library::{
            CargoPackageAliasCargoFactsV1, CargoPackageAliasCoverageV1,
            CargoPackageAliasEvidenceV1, CargoPackageAliasObservationV1,
            CargoPackageAliasUnavailableV1, CargoPackageAliasV1, CargoPackageNameV1,
        };
        use backend_semantic::vocabulary::RustEdition;

        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let stale_alias = CargoPackageAliasV1::CargoPackageName(
            CargoPackageNameV1::new("old-cargo-name").expect("bounded stale alias"),
        );
        let stale_observation = CargoPackageAliasObservationV1::from_wire_parts(
            profile,
            [44; 32],
            Some(CargoPackageAliasCargoFactsV1::from_wire_parts(
                [1; 32], [2; 32], [3; 32],
            )),
            vec![0],
            CargoPackageAliasCoverageV1::Complete,
        )
        .expect("well-shaped previous observation");
        let stale_evidence = CargoPackageAliasEvidenceV1::from_wire_parts(
            vec![stale_alias],
            vec![stale_observation],
        )
        .expect("well-shaped previous Cargo evidence");
        let project_key = [9; 32];
        let previously_indexed = ProductSourceRecord::project_with_cargo_aliases(
            "fixture",
            [7; 32],
            Vec::new(),
            stale_evidence,
        )
        .expect("prior source row with Cargo authority");
        let fresh_project = ProductSourceRecord::project("fixture", [7; 32], Vec::new())
            .expect("fresh scan starts with no Cargo aliases");
        assert_ne!(previously_indexed, fresh_project);

        // This is the no-reuse/recompile result when the staged compiler has
        // no retained metadata: the current revision is explicitly
        // unavailable, and the previous name does not cross the rebuild.
        let current = staged_cargo_alias_evidence(profile, [45; 32], Path::new("/workspace"), None)
            .expect("no retained full Cargo facts is a typed unavailable observation")
            .expect("Rust profile carries an explicit alias observation");
        let mut changes = vec![BuiltinSourceChange {
            key: project_key,
            after: Some(fresh_project.clone()),
        }];
        replace_project_cargo_aliases(
            &mut changes,
            project_key,
            fresh_project,
            Vec::new(),
            Some(&previously_indexed),
            |_| Ok(None),
            vec![current],
        )
        .expect("current observation replaces previous source authority");

        let indexed = changes[0]
            .after
            .as_ref()
            .and_then(ProductSourceRecord::project_fields)
            .and_then(|fields| fields.cargo_aliases)
            .expect("new profile observation remains explicit");
        assert!(indexed.aliases().is_empty());
        assert!(!indexed.admits_for_profile(profile, "old-cargo-name"));
        let observation = indexed
            .observation(profile)
            .ok_or("current profile observation remains explicit")?;
        assert_eq!(observation.source_observation_revision(), [45; 32]);
        assert_eq!(
            observation.coverage(),
            CargoPackageAliasCoverageV1::Unavailable(
                CargoPackageAliasUnavailableV1::MetadataFactsUnavailable
            )
        );
        Ok(())
    }

    fn scratch_directory() -> PathBuf {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-compiler-input-witness-{}-{sequence}",
            std::process::id()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(&path)
                .expect("create private compiler input witness fixture");
        }
        #[cfg(not(unix))]
        {
            fs::create_dir(&path).expect("create compiler input witness fixture");
            crate::test_support::make_private(&path)
                .expect("make the compiler input witness fixture private");
        }
        path
    }

    fn pre_offer_fixture() -> (
        PathBuf,
        ingest::IndexSnapshot,
        ingest::CompilerWorkspaceSnapshot,
    ) {
        let root = scratch_directory();
        fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\n")
            .expect("write fixture manifest");
        fs::write(root.join("main.rs"), "pub fn fixture() {}\n").expect("write fixture source");
        let root_text = root.to_str().expect("UTF-8 fixture path");
        let scan =
            ingest::scan_project_for_unproven_authorities(root_text, [73; 32], &BTreeMap::new())
                .expect("scan pre-Offer fixture");
        let snapshot = ingest::CompilerWorkspaceSnapshot::open(&root)
            .expect("capture pre-Offer workspace snapshot");
        (root, scan, snapshot)
    }

    #[test]
    fn revoked_grant_and_journal_bind_failures_fall_back_before_offer() {
        let (source_root, scan, snapshot) = pre_offer_fixture();
        let journal_root = scratch_directory();
        let journal = Arc::new(Mutex::new(
            super::super::super::pending_stored::PendingStoredAckJournal::open(&journal_root)
                .expect("open pre-Offer journal"),
        ));

        for (stage, detail) in [
            (
                "trusted worker grant reload",
                "compiler assignment has no exact persisted trusted-worker grant",
            ),
            (
                "assignment reservation bind",
                "persist remote compiler assignment reservation: injected journal failure",
            ),
        ] {
            let owner_endpoint =
                backend_engine::cluster_transport::SecretKey::from_bytes(&[74; 32]).public();
            let reservation = journal
                .lock()
                .expect("lock pre-Offer journal")
                .reserve_capacity(*owner_endpoint.as_bytes())
                .expect("reserve pre-Offer capacity");
            let reservation_lease =
                super::super::super::pending_stored::PendingAckReservationLease::new(
                    Arc::clone(&journal),
                    reservation,
                );

            let fallback = pre_offer_result_or_local_fallback(
                Err::<(), _>(BuiltinModelError(detail.to_owned())),
                stage,
                &scan.revision_fence,
                &snapshot,
            )
            .expect("stable source state allows local fallback");
            assert!(fallback.is_none(), "{stage} must skip remote Offer");

            drop(reservation_lease);
            assert_eq!(
                journal
                    .lock()
                    .expect("lock pre-Offer journal")
                    .capacity_count(),
                0,
                "failed pre-Offer admission releases its reservation"
            );
        }

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(journal_root);
    }

    #[test]
    fn pre_offer_fallback_retries_when_the_source_snapshot_changed() {
        let (source_root, scan, snapshot) = pre_offer_fixture();
        fs::write(source_root.join("main.rs"), "pub fn changed() {}\n")
            .expect("change source after snapshot capture");

        let error = pre_offer_result_or_local_fallback::<()>(
            Err(BuiltinModelError("revoked worker grant".to_owned())),
            "trusted worker grant reload",
            &scan.revision_fence,
            &snapshot,
        )
        .expect_err("invalidated source snapshot must stop this indexing attempt");
        assert!(error.0.contains("retry indexing"));

        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn configuration_observation_tracks_manifests_and_ignores_unrelated_files() {
        let root = scratch_directory();
        fs::create_dir_all(root.join(".next/cache")).expect("create ignored build directory");
        fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\n")
            .expect("write Cargo manifest");
        fs::write(root.join("main.rs"), "pub fn fixture() {}\n").expect("write Rust source");
        fs::write(root.join("package.json"), "{\"name\":\"fixture\"}\n")
            .expect("write package manifest");
        fs::write(root.join("app.ts"), "export const fixture = 1;\n")
            .expect("write TypeScript source");
        fs::write(root.join("package-lock.json"), "{\"lockfileVersion\":3}\n")
            .expect("write package lockfile");
        let ignored_lockfile = fs::File::create(root.join(".next/cache/package-lock.json"))
            .expect("create ignored large lockfile");
        ignored_lockfile
            .set_len((ingest::MAX_COMPILER_CONFIGURATION_FILE_BYTES as u64) * 2)
            .expect("make ignored lockfile large");
        drop(ignored_lockfile);

        let root_text = root.to_str().expect("UTF-8 fixture path");
        let cold =
            ingest::scan_project(root_text, [31; 32], &BTreeMap::new()).expect("cold project scan");
        let first = observe_compiler_configuration(&cold.compiler_configuration);
        assert!(
            cold.compiler_configuration
                .complete_languages
                .contains(&Language::TypeScript)
        );
        assert!(
            cold.compiler_configuration
                .files
                .iter()
                .all(|file| !file.relative_path.starts_with(".next"))
        );

        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(root.join("README.md"), "documentation only\n").expect("write README");
        let no_op =
            ingest::scan_project(root_text, [31; 32], &reusable).expect("repeat project scan");
        let after_unrelated = observe_compiler_configuration(&no_op.compiler_configuration);
        assert!(
            ingest::compiler_revision_is_current(&no_op.revision_fence)
                .expect("revalidate captured project revision")
        );
        assert_eq!(
            first[&Language::Rust].digest,
            after_unrelated[&Language::Rust].digest
        );
        assert_eq!(
            first[&Language::TypeScript].digest,
            after_unrelated[&Language::TypeScript].digest
        );
        assert_eq!(no_op.source_version, cold.source_version);
        assert!(no_op.compiler_sources.is_empty());
        assert_eq!(no_op.reused_compiler_files.len(), 2);
        let runtime_no_op =
            ingest::scan_project_for_unproven_authorities(root_text, [31; 32], &reusable)
                .expect("no-op scan for unproven authorities");
        assert!(runtime_no_op.compiler_configuration.files.is_empty());
        assert!(
            runtime_no_op
                .compiler_configuration
                .complete_languages
                .is_empty()
        );
        let live_profiles = runtime_no_op
            .reused_compiler_files
            .iter()
            .map(|source| ingest::compilation_profile(&root, &source.relative_path, source.profile))
            .collect::<BTreeSet<_>>();
        let inputs = live_profiles
            .iter()
            .map(|profile| {
                (
                    *profile,
                    CompilerInputAdmission {
                        lineage: None,
                        read_set_completeness: ReadSetCompleteness::Unproven,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let dirty = profiles_requiring_compilation(
            &live_profiles,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &inputs,
            &BTreeSet::new(),
        );
        assert_eq!(
            dirty, live_profiles,
            "unproven lanes fail closed on no-op; persisted Cargo aliases cannot skip their rebuild"
        );
        let (fresh, reused) = ingest::select_compiler_inputs(
            &root,
            runtime_no_op.compiler_sources,
            runtime_no_op.reused_compiler_files,
            &dirty,
        );
        assert!(fresh.is_empty());
        assert_eq!(reused.len(), 2);
        assert_eq!(
            reused.len(),
            live_profiles.len(),
            "a no-op currently compiles one package source per live profile"
        );

        fs::write(root.join("Cargo.toml"), "[package]\nname='fixture-v2'\n")
            .expect("edit Cargo manifest");
        assert!(
            !ingest::compiler_revision_is_current(&no_op.revision_fence)
                .expect("notice manifest revision change")
        );
        let manifest_scan =
            ingest::scan_project(root_text, [31; 32], &reusable).expect("scan after manifest edit");
        let after_manifest = observe_compiler_configuration(&manifest_scan.compiler_configuration);
        assert_eq!(manifest_scan.source_version, cold.source_version);
        assert!(manifest_scan.compiler_sources.is_empty());
        assert_eq!(manifest_scan.reused_compiler_files.len(), 2);
        assert_ne!(
            after_unrelated[&Language::Rust].digest,
            after_manifest[&Language::Rust].digest
        );
        assert_eq!(
            after_unrelated[&Language::TypeScript].digest,
            after_manifest[&Language::TypeScript].digest
        );
        let manifest_dirty = profiles_requiring_compilation(
            &live_profiles,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &inputs,
            &BTreeSet::new(),
        );
        assert_eq!(manifest_dirty, live_profiles);
        let (_, manifest_reused) = ingest::select_compiler_inputs(
            &root,
            manifest_scan.compiler_sources.clone(),
            manifest_scan.reused_compiler_files.clone(),
            &manifest_dirty,
        );
        assert_eq!(
            manifest_reused.len(),
            2,
            "manifest-only changes compile all current unproven profile inputs"
        );

        fs::write(root.join("Cargo.lock"), "version = 3\n").expect("create Cargo lockfile");
        let lockfile_scan =
            ingest::scan_project(root_text, [31; 32], &reusable).expect("scan after lockfile edit");
        let after_lockfile = observe_compiler_configuration(&lockfile_scan.compiler_configuration);
        assert_eq!(lockfile_scan.source_version, cold.source_version);
        assert!(lockfile_scan.compiler_sources.is_empty());
        assert_eq!(lockfile_scan.reused_compiler_files.len(), 2);
        assert_ne!(
            after_manifest[&Language::Rust].digest,
            after_lockfile[&Language::Rust].digest
        );
        assert_eq!(
            after_manifest[&Language::TypeScript].digest,
            after_lockfile[&Language::TypeScript].digest
        );

        fs::write(root.join("package-lock.json"), "{\"lockfileVersion\":4}\n")
            .expect("edit package lockfile");
        let package_lock_scan = ingest::scan_project(root_text, [31; 32], &reusable)
            .expect("scan after package lockfile edit");
        let after_package_lock =
            observe_compiler_configuration(&package_lock_scan.compiler_configuration);
        assert_ne!(
            after_lockfile[&Language::TypeScript].digest,
            after_package_lock[&Language::TypeScript].digest
        );
        assert_eq!(
            after_lockfile[&Language::Rust].digest,
            after_package_lock[&Language::Rust].digest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unproven_dynamic_and_newly_present_inputs_force_profile_compilation() {
        let root = scratch_directory();
        fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\n")
            .expect("write Cargo manifest");
        fs::write(
            root.join("main.rs"),
            "pub const DYNAMIC: &str = include_str!(\"generated-input.txt\");\n",
        )
        .expect("write source with a dynamic read");
        fs::write(root.join("generated-input.txt"), "first version\n")
            .expect("write initial dynamic input");

        let root_text = root.to_str().expect("UTF-8 fixture path");
        let cold = ingest::scan_project(root_text, [32; 32], &BTreeMap::new())
            .expect("scan initial dynamic input");
        let profile =
            ingest::compilation_profile(&root, "main.rs", cold.compiler_sources[0].profile);
        let known_configuration =
            observe_compiler_configuration(&cold.compiler_configuration)[&Language::Rust].digest;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(root.join("generated-input.txt"), "changed dynamic input\n")
            .expect("change unlisted dynamic input");
        let changed = ingest::scan_project(root_text, [32; 32], &reusable)
            .expect("scan changed dynamic input");
        assert_eq!(changed.source_version, cold.source_version);
        assert!(changed.compiler_sources.is_empty());
        assert_eq!(changed.reused_compiler_files.len(), 1);
        assert_eq!(
            observe_compiler_configuration(&changed.compiler_configuration)[&Language::Rust].digest,
            known_configuration,
            "the unlisted read is outside the allowlist witness"
        );

        let admission = CompilerInputAdmission {
            lineage: None,
            read_set_completeness: ReadSetCompleteness::Unproven,
        };
        let inputs = BTreeMap::from([(profile, admission)]);
        let live = BTreeSet::from([profile]);
        let dirty = profiles_requiring_compilation(
            &live,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &inputs,
            &BTreeSet::new(),
        );
        assert_eq!(dirty, live, "dynamic reads cannot authorize an exact skip");

        fs::remove_file(root.join("generated-input.txt")).expect("remove dynamic input");
        let missing = ingest::scan_project(root_text, [32; 32], &reusable)
            .expect("scan source with now-missing dynamic input");
        assert_eq!(missing.source_version, cold.source_version);
        assert_eq!(missing.reused_compiler_files.len(), 1);
        assert!(
            ingest::compiler_revision_is_current(&missing.revision_fence)
                .expect("revalidate tree with the input still absent")
        );
        let appeared = root.join("generated-input.txt");
        fs::write(&appeared, "newly appeared negative read\n")
            .expect("create formerly missing input");
        assert!(
            !ingest::compiler_revision_is_current(&missing.revision_fence)
                .expect("directory revision detects newly present input")
        );
        let appeared_scan = ingest::scan_project(root_text, [32; 32], &reusable)
            .expect("scan newly appeared input");
        assert_eq!(appeared_scan.source_version, cold.source_version);
        assert_eq!(appeared_scan.reused_compiler_files.len(), 1);
        let appeared_dirty = profiles_requiring_compilation(
            &live,
            &BTreeSet::new(),
            &BTreeSet::new(),
            &inputs,
            &BTreeSet::new(),
        );
        assert_eq!(appeared_dirty, live, "negative reads remain unproven");
        fs::write(
            root.join("main.rs"),
            "pub const DYNAMIC: &str = include_str!(\"generated-input.txt\");\npub const CHANGED: bool = true;\n",
        )
        .expect("change source while compilation is in flight");
        assert!(
            !ingest::compiler_revision_is_current(&appeared_scan.revision_fence)
                .expect("revision fence observes source change")
        );
        let _ = fs::remove_dir_all(root);
    }
}
