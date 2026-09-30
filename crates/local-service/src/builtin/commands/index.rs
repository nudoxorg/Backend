use super::super::{
    BuiltinAuthorityVerifier, BuiltinIntent, BuiltinModel, BuiltinModelError,
    BuiltinSemanticChange, BuiltinSemanticRelation, BuiltinSourceChange, BuiltinValidator,
    BuiltinWorkspaceRelation, Command, CommandReply, ProductSourceRecord, RegistryGateway,
    WireCertificate, WireClaim, WorkspaceModel, activate_semantic_publication, ingest, projection,
    publish_builtin_view,
};
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
    PartialSemanticCoverage, ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    SemanticPublicationClaim, SemanticPublicationCoverage, SemanticPublicationSelection,
};
use backend_extension_turso::SourceObservationReceipt;
use backend_library::CompileExecutionIntent;
use backend_library::interface::{CompilerRuntimeCause, CompilerTerminal};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompileRequest, PackageUrl,
};
use backend_semantic::ir::SemanticInputWitness;
use backend_semantic::vocabulary::{Language, LanguageProfile};
use backend_version::{Coverage, ScopeRoot, WorkspaceRoot};
use std::collections::{BTreeMap, BTreeSet};
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
    finish_index_scan(
        daemon,
        run_index_scan(work).map_err(IndexScanFailure::into_model_error)?,
        compiler,
        semantic_authority,
        owner_cluster,
        pending_stored_acks,
        defer,
    )
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
        Some(record) => record
            .project_fields()
            .map(|fields| fields.files.to_vec())
            .ok_or_else(|| {
                BuiltinModelError("project key contains a source file record".to_owned())
            })?,
        None => Vec::new(),
    };
    if old_files.len() > ProductSourceRecord::MAX_PROJECT_FILES {
        return Err(BuiltinModelError(
            "project source frontier exceeds its bounded file limit".to_owned(),
        ));
    }
    let mut reusable = BTreeMap::new();
    for key in &old_files {
        let record = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read reusable source file: {error}")))?
            .ok_or_else(|| {
                BuiltinModelError("project frontier refers to a missing source file".to_owned())
            })?;
        if record.file_fields().is_none() {
            return Err(BuiltinModelError(
                "project frontier refers to a non-file record".to_owned(),
            ));
        }
        reusable.insert(*key, record);
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
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
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
    let source_root = source_root.as_path();
    let coordinate = coordinate.as_ref();
    let revision_fence = &scan.revision_fence;
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
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let project = ProductSourceRecord::project(&label, scan.source_version, file_keys.clone())
        .map_err(BuiltinModelError)?;
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project),
        });
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
    let selected = file_keys.into_iter().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .into_iter()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    // Keep these source rows private until every semantic profile has been
    // admitted. The eventual BuiltinIntent carries source and semantic roots
    // in one workspace transition.
    // This compiler owner does not expose a complete typed present-and-negative
    // read set, so its source/configuration digest cannot authorize reuse.
    // Every live semantic profile rebuilds until the authority can prove its
    // complete input closure.
    let (semantic_changes, selected) = {
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
            (Vec::new(), Vec::new())
        } else {
            let sources = ingest::admit_compiler_sources(source_root, fresh, reused)
                .map_err(BuiltinModelError)?;
            if defer && owner_cluster.is_none() {
                return prepare_deferred_compile(
                    package,
                    &label,
                    changes,
                    &semantic_context,
                    sources,
                    revision_fence.clone(),
                    &observations,
                    semantic_authority,
                )
                .map(PreparedIndex::Compile);
            }
            compile_semantic_publications(
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
            )?
        }
    };
    let intent = if changes.is_empty() && semantic_changes.is_empty() {
        None
    } else {
        Some(BuiltinIntent::index_with_semantics(
            package,
            &label,
            changes,
            semantic_changes,
        )?)
    };
    Ok(PreparedIndex::Ready(PreparedProductSelection {
        intent,
        selected,
    }))
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
/// publication needs afterwards. Source changes and authority observations
/// remain private until the candidate transaction succeeds.
pub(super) struct DeferredIndex {
    package: backend_engine::PackageKey,
    label: String,
    source_changes: Vec<BuiltinSourceChange>,
    revision_fence: ingest::CompilerRevisionFence,
    profiles: Vec<DeferredProfile>,
}

struct DeferredProfile {
    key: ProductSemanticPublicationKey,
    expected_artifacts: u32,
    attempt: backend_extension_turso::CandidateAttempt,
    sources: Option<OwnedPackageSourceSet>,
}

impl DeferredIndex {
    /// The sources each profile compiles, in profile order, taken once.
    pub(super) fn take_work(&mut self) -> Vec<OwnedPackageSourceSet> {
        self.profiles
            .iter_mut()
            .filter_map(|profile| profile.sources.take())
            .collect()
    }
}

/// Begins the local compile of every profile the sources name, exactly as
/// the in-place local route does (`compile_semantic_publications` with no
/// compiler cluster), stopping short of the compile itself.
fn prepare_deferred_compile(
    package: backend_engine::PackageKey,
    label: &str,
    source_changes: Vec<BuiltinSourceChange>,
    context: &SemanticCompilationContext<'_>,
    sources: Vec<ingest::CompilerSource>,
    revision_fence: ingest::CompilerRevisionFence,
    observations: &BTreeMap<LanguageProfile, SourceObservationReceipt>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
) -> Result<DeferredIndex, BuiltinModelError> {
    let mut by_profile = BTreeMap::<LanguageProfile, Vec<OwnedPackageSource>>::new();
    for source in sources {
        let profile = compile_profile(context.source_root, &source);
        by_profile.entry(profile).or_default().push(
            OwnedPackageSource::new(&source.relative_path, &source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))?,
        );
    }
    let mut profiles = Vec::with_capacity(by_profile.len());
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
        let scan_observation = observations.get(&profile).ok_or_else(|| {
            BuiltinModelError("semantic compilation has no matching source observation".to_owned())
        })?;
        let scan_input_digest = scan_observation.observation().revision().ok_or_else(|| {
            BuiltinModelError("semantic compilation observation has no input digest".to_owned())
        })?;
        let local_observation =
            semantic_authority.observe(&key, scan_input_digest, u64::from(expected_artifacts))?;
        let attempt = semantic_authority.begin_candidate_attempt(&key, &local_observation)?;
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
        profiles.push(DeferredProfile {
            key,
            expected_artifacts,
            attempt,
            sources: Some(sources),
        });
    }
    Ok(DeferredIndex {
        package,
        label: label.to_owned(),
        source_changes,
        revision_fence,
        profiles,
    })
}

/// Compiles a deferred index's sources, off the owner loop: the compiler
/// runtime does the work; this thread only waits for it.
pub(super) fn run_deferred_compile(
    compiler: &LocalCompilerClient,
    work: Vec<OwnedPackageSourceSet>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> Vec<Result<StagedSemanticPackage, PackageSemanticRuntimeError>> {
    work.into_iter()
        .map(|sources| {
            compiler.compile_package_sources_staged_cancellable(sources, Arc::clone(&cancelled))
        })
        .collect()
}

pub(super) fn deferred_compile_was_cancelled(
    compiled: &[Result<StagedSemanticPackage, PackageSemanticRuntimeError>],
) -> bool {
    compiled.iter().any(|result| match result {
        Err(PackageSemanticRuntimeError::Runtime(CompilerTerminal::Runtime {
            cause: CompilerRuntimeCause::RequestCancelled,
            ..
        })) => true,
        Err(PackageSemanticRuntimeError::Package(PackageSemanticError::Compile {
            terminal,
            ..
        })) => matches!(terminal.as_ref(), CompilerTerminal::PackageCancelled { .. }),
        _ => false,
    })
}

/// Publishes a deferred compile's semantic candidates on the owner loop and
/// returns one source-plus-semantic intent. The caller commits that intent
/// before advancing the process-local serving selector.
pub(super) fn finish_deferred_index<E: std::fmt::Display>(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    semantic_authority: &mut super::super::semantic_authority::SemanticAuthority,
    job: DeferredIndex,
    compiled: Vec<Result<StagedSemanticPackage, E>>,
) -> Result<PreparedProductSelection, BuiltinModelError> {
    if compiled.len() != job.profiles.len() {
        return Err(BuiltinModelError(
            "the deferred compile did not answer every profile; prior selected semantic generation was preserved"
                .to_owned(),
        ));
    }
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic publications: {error}")))?;
    let mut changes = Vec::with_capacity(job.profiles.len().saturating_mul(2));
    let mut selected = Vec::with_capacity(job.profiles.len());
    for (profile, compiled) in job.profiles.into_iter().zip(compiled) {
        let DeferredProfile {
            key,
            expected_artifacts,
            attempt,
            ..
        } = profile;
        let (staged, publication_coverage) = admit_local_compile(compiled, expected_artifacts)?;
        let (claim, _selected) = publish_local_compile(
            semantic_authority,
            &key,
            attempt,
            &staged,
            &job.revision_fence,
        )?;
        record_semantic_publication(
            &relation,
            key.clone(),
            publication_coverage,
            claim,
            &mut changes,
        )?;
        selected.push((key, claim));
    }
    let intent = if job.source_changes.is_empty() && changes.is_empty() {
        None
    } else {
        Some(BuiltinIntent::index_with_semantics(
            job.package,
            &job.label,
            job.source_changes,
            changes,
        )?)
    };
    Ok(PreparedProductSelection { intent, selected })
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
            OwnedPackageSource::new(&source.relative_path, &source.source)
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
        let (claim, selected, publication_coverage) = if let Some((claim, selected)) =
            remote_publication
        {
            // Remote result envelopes carry semantic artifacts but no typed
            // source-scope gaps. Remote admission therefore requires the
            // exact complete source-identity multiset before this branch can
            // select a generation; partial worker output is rejected there.
            (claim, selected, SemanticPublicationCoverage::Complete)
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
            if let (Some(owner), Some((capture, work))) = (owner_cluster, captured_work.as_ref()) {
                let elapsed_ms =
                    u64::try_from(local_compile_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                match super::super::cluster_dispatch::LocalCompilerCostObservation::new(
                    *work.recipe().as_ref(),
                    capture.payload_bytes(),
                    expected_artifacts,
                    elapsed_ms.max(1),
                ) {
                    Ok(observation) => {
                        if let Err(error) = owner.record_local_observation(observation) {
                            eprintln!("locald compiler cost observation was not saved: {error}");
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
            (publication.0, publication.1, publication_coverage)
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
        let _ = selected;
    }
    Ok((changes, selected_claims))
}

/// Admits one local compile's output: every expected source is accounted
/// for (an artifact or a typed scope gap), and a gap makes the publication
/// partial. The words are the owner's refusal when it is not.
fn admit_local_compile(
    compiled: Result<StagedSemanticPackage, impl std::fmt::Display>,
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
            return Err(BuiltinModelError(format!(
                "local semantic compilation failed; prior selected semantic generation was preserved: {error}"
            )));
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
            != backend_semantic::vocabulary::LanguageProfile::try_from(identity.trust.profile)
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
    hasher.update(b"backend.local-service.semantic-input.v1\0");
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
        hasher.update(blake3::hash(source.source.as_bytes()).as_bytes());
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
    let files = record
        .project_fields()
        .map(|fields| fields.files)
        .ok_or_else(|| BuiltinModelError("project key contains a source file record".to_owned()))?;
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
    BuiltinIntent::remove_project(package, label, files, semantic_changes).map(Some)
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
    }
}

pub(super) fn semantic_versions(
    daemon: &crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    package: &backend_engine::PackageReference,
    workspace: Option<&Path>,
    semantic_authority: &super::super::semantic_authority::SemanticAuthority,
) -> Result<Box<[backend_engine::SemanticVersionRecord]>, BuiltinModelError> {
    let relation = daemon
        .engine()
        .daemon()
        .owner()
        .snapshot()
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open semantic version history: {error}")))?;
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
                    unavailable.get_or_insert(*reason);
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
                    generations.push((
                        target,
                        selected_key,
                        claim,
                        semantic_version_record(
                            key,
                            coverage,
                            claim,
                            false,
                            semantic_authority.freshness(key, claim),
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
        let mut ignored_lockfile = fs::File::create(root.join(".next/cache/package-lock.json"))
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
        assert_eq!(dirty, live_profiles, "unproven lanes fail closed on no-op");
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
