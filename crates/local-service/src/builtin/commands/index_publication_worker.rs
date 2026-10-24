//! The final durable publication keeps the original writers on one owned
//! thread. The actor grants one checked claim and later installs its coherent
//! result; it never runs this thread's preparation, fsync or SQL projection.
use super::super::index_operation::PublicationJournalWriter;
use super::*;
use crate::builtin::generation_residence::SemanticGenerationResidence;
use crate::builtin::semantic_authority::{DetachedSemanticAuthority, RetiredSemanticRead};
use crate::builtin::sql_projection::ProjectionWriter;
use crate::builtin::view_build::ImageRowResidence;
use crate::builtin::view_publish::PublishedRoots;
use crate::builtin::{BuiltinViewAdmission, PreparedBuiltinView, prepare_builtin_view};
use backend_engine::{
    PreparedReadHeadRefresh, PreparedWorkspaceCandidate, PublishGrant, PublishedReadHead,
    PublishedReadHeadRefresh, ReadHeadPublicationFailure, ReadHeadWriter, RetiredReadHead,
    UnselectedWorkspaceCandidate, WorkspaceCandidateClaim, WorkspaceError, WorkspaceSnapshot,
};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::thread::JoinHandle;

#[must_use = "settle every original writer and retire prior read payloads"]
pub(super) struct PublicationWork {
    pub(super) semantic: Option<DetachedSemanticAuthority>,
    pub(super) journal: Option<PublicationJournalWriter>,
    pub(super) projection: Option<ProjectionWriter>,
    pub(super) image_rows: ImageRowResidence,
    pub(super) generations: SemanticGenerationResidence,
    pub(super) head: Head,
    pub(super) proof: Option<UnselectedWorkspaceCandidate>,
    pub(super) roots: Option<PublishedRoots>,
    pub(super) outcome: Option<backend_library::IndexJobOutcome>,
    preparation: Preparation,
    // Kept outside consumed phase payloads so unwind cannot lose durable
    // candidate attempts that still require retirement.
    source_attempts: Vec<backend_extension_turso::CandidateAttempt>,
    base: WorkspaceSnapshot,
    current: backend_engine::ViewRoot,
    current_cursor: backend_engine::Cursor,
    compiler: LocalCompilerClient,
    deployment: crate::builtin::SemanticDeployment,
    workspace: std::path::PathBuf,
    prior: Option<PublishedRoots>,
    prepared: PreparedProductSelection,
    requested_reference: backend_library::PackageReference,
    operation_key: Option<backend_library::IndexOperationKey>,
    cancellation: Arc<AtomicBool>,
    source_observation: Option<Arc<crate::builtin::index_work_observation::IndexWorkObservation>>,
    staged: Option<PreparedBuiltinIntent>,
    candidate: Option<PreparedWorkspaceCandidate>,
    candidate_snapshot: Option<WorkspaceSnapshot>,
    view: Option<PreparedBuiltinView>,
    partial_basis: Option<backend_library::IndexSourceCaptureSummary>,
    pub(super) residences_reserved: bool,
    settlement_only: bool,
    unselected_error: Option<BuiltinModelError>,
    grant_accepted: bool,
    semantic_committed: bool,
    projected: bool,
    #[cfg(test)]
    observed: Arc<TestObservation>,
    #[cfg(test)]
    pub(super) test_hook: Option<TestHook>,
    #[cfg(test)]
    pub(super) test_spawn_refusal: bool,
}

/// Exactly one owned preparation or checked continuation. The storage
/// capabilities above remain original and independent of the phase payload.
pub(super) enum Preparation {
    Publication {
        refusal: Option<backend_library::IndexJobOutcome>,
    },
    Source {
        scan: IndexScanResult,
        route: Option<super::super::index::SourceRoute>,
    },
    Cluster(super::super::index::SourceCompilePlan),
    Finalize(DeferredIndex),
    Cleanup {
        plan: super::super::index::CaptureFailurePlan,
        outcome: backend_library::IndexJobOutcome,
    },
    SourcePlan(super::super::index::SourceCompilePlan),
    SourcePrepared(PreparedProductSelection),
    SourceCaptured {
        continuation: PreparedIndex,
        captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    },
    // A consumed payload may unwind, but the original capabilities and begun
    // attempt identities remain retained for failure settlement.
    Consumed,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TestStage {
    BeforeSourcePreparation,
    BeforeSourceMaterialization,
    BeforeFinalization,
    BeforeViewPreparation,
    BeforeClusterCompile,
    BeforeClusterFallback,
    BeforeGrantValidation,
    AfterGrant,
    AfterSelection,
    AfterReceipt,
}
#[cfg(test)]
pub(super) struct TestHook {
    pub(super) stage: TestStage,
    pub(super) run: Box<dyn FnOnce() + Send>,
}

pub(super) enum Head {
    /// Original reservation with no candidate or grant.
    Unprepared(ReadHeadWriter<BuiltinModel>),
    Writer(ReadHeadWriter<BuiltinModel>),
    Pending(ReadHeadPublicationFailure<BuiltinModel>),
    Published(PublishedReadHead<BuiltinModel>),
    Refresh(PreparedReadHeadRefresh<BuiltinModel>),
    Refreshed(PublishedReadHeadRefresh<BuiltinModel>),
    Moving,
}

pub(super) enum Disposition {
    Ready,
    /// A durable derived-view refresh of the unchanged workspace base.
    Refreshed,
    /// A checked source-only plan that needs no intermediate capture.
    SourcePrepared,
    /// The independent source/Pending capture is coherent; no terminal
    /// Published receipt was written and compilation has not begun.
    SourceCaptured,
    /// A verified unchanged physical base permits ordinary failure/cancel.
    Unselected(BuiltinModelError),
    /// The same original capabilities remain available for explicit retry.
    Pending(BuiltinModelError),
}

pub(super) struct Completion {
    pub(super) work: Box<PublicationWork>,
    pub(super) disposition: Disposition,
}

pub(super) enum Event {
    Grant {
        claim: WorkspaceCandidateClaim,
        reply: SyncSender<Result<PublishGrant, WorkspaceError>>,
        cancellation: Arc<AtomicBool>,
    },
    RefreshApproval {
        base_root: [u8; 32],
        base_sequence: u64,
        base_view_root: [u8; 32],
        base_cursor: backend_engine::Cursor,
        cancellation: Arc<AtomicBool>,
        reply: SyncSender<Result<(), BuiltinModelError>>,
    },
    Complete(Completion),
}

/// Every potentially large old owner is returned to the same existing worker
/// after the fixed-commitment installation, including the held SQL read tx.
#[derive(Default)]
pub(super) struct Retired {
    pub(super) head: Option<RetiredReadHead>,
    pub(super) semantic: Option<RetiredSemanticRead>,
    pub(super) journal: Option<IndexOperationJournal>,
    pub(super) projection: Option<backend_extension_turso::TursoProjectionReadSnapshot>,
    pub(super) image_rows: Option<ImageRowResidence>,
    pub(super) generations: Option<SemanticGenerationResidence>,
    pub(super) roots: Option<PublishedRoots>,
}

enum Settlement {
    Retry(Box<PublicationWork>),
    Retire(Box<PublicationWork>, Retired),
}

#[must_use = "close or settle and join this owned publication thread"]
pub(super) struct PublicationWorker {
    events: Receiver<Event>,
    settle: Sender<Settlement>,
    handle: JoinHandle<()>,
    #[cfg(test)]
    observed: Arc<TestObservation>,
}

// Observes the original worker without receiving its grant/completion events
// or admitting a retry. No lock is held across any publication work.
#[cfg(test)]
struct TestObservation {
    started: std::time::Instant,
    phase: std::sync::Mutex<&'static str>,
    granted: AtomicBool,
}

#[cfg(test)]
impl TestObservation {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            phase: std::sync::Mutex::new("reserved"),
            granted: AtomicBool::new(false),
        }
    }

    fn phase(&self, phase: &'static str) {
        *self.phase.lock().unwrap_or_else(|error| error.into_inner()) = phase;
    }

    fn snapshot(&self) -> String {
        let phase = *self.phase.lock().unwrap_or_else(|error| error.into_inner());
        format!(
            "phase={phase}, elapsed={:?}, granted={}",
            self.started.elapsed(),
            self.granted.load(Ordering::Acquire)
        )
    }
}

impl PublicationWork {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        semantic: Option<DetachedSemanticAuthority>,
        journal: Option<PublicationJournalWriter>,
        projection: Option<ProjectionWriter>,
        read_head: Option<ReadHeadWriter<BuiltinModel>>,
        image_rows: ImageRowResidence,
        generations: SemanticGenerationResidence,
        base: WorkspaceSnapshot,
        current: backend_engine::ViewRoot,
        current_cursor: backend_engine::Cursor,
        compiler: LocalCompilerClient,
        deployment: crate::builtin::SemanticDeployment,
        workspace: std::path::PathBuf,
        prior: Option<PublishedRoots>,
        prepared: PreparedProductSelection,
        preparation: Preparation,
        requested_reference: backend_library::PackageReference,
        operation_key: Option<backend_library::IndexOperationKey>,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        let source_observation = match &preparation {
            Preparation::Source { scan, .. } => scan.observation(),
            _ => None,
        };
        let source_attempts = match &preparation {
            Preparation::Finalize(job) => job.pending_attempts(),
            _ => Vec::new(),
        };
        Self {
            semantic,
            journal,
            projection,
            image_rows,
            generations,
            head: read_head.map_or(Head::Moving, Head::Unprepared),
            proof: None,
            roots: None,
            outcome: None,
            preparation,
            source_attempts,
            base,
            current,
            current_cursor,
            compiler,
            deployment,
            workspace,
            prior,
            prepared,
            requested_reference,
            operation_key,
            cancellation,
            source_observation,
            staged: None,
            candidate: None,
            candidate_snapshot: None,
            view: None,
            partial_basis: None,
            residences_reserved: false,
            settlement_only: false,
            unselected_error: None,
            grant_accepted: false,
            semantic_committed: false,
            projected: false,
            #[cfg(test)]
            observed: Arc::new(TestObservation::new()),
            #[cfg(test)]
            test_hook: None,
            #[cfg(test)]
            test_spawn_refusal: false,
        }
    }

    #[cfg(test)]
    pub(super) fn cleanup_inventory(
        &self,
    ) -> Option<(
        Vec<backend_extension_turso::CandidateAttempt>,
        backend_library::IndexJobOutcome,
    )> {
        match &self.preparation {
            Preparation::Cleanup { plan, outcome } => {
                Some((plan.attempts.clone(), outcome.clone()))
            }
            _ => None,
        }
    }

    pub(super) fn base_root(&self) -> backend_version::WorkspaceRoot {
        self.base.root()
    }

    pub(super) fn admit_workspace(
        &mut self,
        workspace: Result<std::path::PathBuf, BuiltinModelError>,
    ) -> Result<(), BuiltinModelError> {
        self.workspace = workspace?;
        Ok(())
    }

    pub(super) fn failure_outcome(&self) -> Option<&backend_library::IndexJobOutcome> {
        match &self.preparation {
            Preparation::Publication { refusal } => refusal.as_ref(),
            Preparation::Cleanup { outcome, .. } => Some(outcome),
            _ => None,
        }
    }

    pub(super) fn is_cleanup(&self) -> bool {
        matches!(self.preparation, Preparation::Cleanup { .. })
    }

    pub(super) fn take_source_continuation(
        &mut self,
    ) -> (
        PreparedIndex,
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    ) {
        match std::mem::replace(&mut self.preparation, Preparation::Consumed) {
            Preparation::SourcePrepared(prepared) => {
                (PreparedIndex::Ready(prepared), BTreeMap::new())
            }
            Preparation::SourceCaptured {
                continuation,
                captures,
            } => (continuation, captures),
            _ => unreachable!("checked source completion"),
        }
    }

    fn prepare(&mut self) -> Result<(), BuiltinModelError> {
        if self.view.is_some() {
            return Ok(());
        }
        // Cleanup stays borrowed in the phase: a panic must retain both its
        // frozen outcome and its unresolved attempt retirement plan.
        if let Preparation::Cleanup { plan, .. } = &mut self.preparation {
            self.prepared = plan.prepare(
                &self.base,
                self.semantic
                    .as_mut()
                    .expect("original cleanup semantic writer")
                    .authority_mut(),
            )?;
        } else if matches!(
            self.preparation,
            Preparation::Finalize(_) | Preparation::Cluster(_) | Preparation::Source { .. }
        ) {
            #[cfg(test)]
            self.run_test_hook(match &self.preparation {
                Preparation::Finalize(_) => TestStage::BeforeFinalization,
                Preparation::Cluster(_) => TestStage::BeforeClusterCompile,
                Preparation::Source { .. } => TestStage::BeforeSourcePreparation,
                _ => unreachable!("checked owned preparation"),
            });
            let preparation = std::mem::replace(&mut self.preparation, Preparation::Consumed);
            match preparation {
                Preparation::Finalize(job) => {
                    let prepared =
                        super::super::index::finish_deferred_index_for_snapshot(&self.base, job)?;
                    self.preparation = Preparation::SourcePrepared(prepared);
                    return Ok(());
                }
                Preparation::Cluster(plan) => {
                    #[cfg(test)]
                    let fallback_hook = &mut self.test_hook;
                    let prepared = plan.compile_cluster(
                        &self.base,
                        &self.compiler,
                        self.semantic
                            .as_mut()
                            .expect("original cluster semantic writer")
                            .authority_mut(),
                        &mut self.source_attempts,
                        #[cfg(test)]
                        &mut || {
                            if fallback_hook
                                .as_ref()
                                .is_some_and(|hook| hook.stage == TestStage::BeforeClusterFallback)
                            {
                                let hook = fallback_hook
                                    .take()
                                    .expect("one-shot real configured fallback");
                                (hook.run)();
                            }
                        },
                    )?;
                    self.preparation = Preparation::SourcePrepared(prepared);
                    return Ok(());
                }
                Preparation::Source { scan, route } => {
                    if let Some(progress) = &self.source_observation {
                        progress.phase("source-records-and-profile-admission");
                    }
                    #[cfg(test)]
                    let source_hook = &mut self.test_hook;
                    match super::super::index::prepare_source_scan(
                        &self.base,
                        scan,
                        &self.compiler,
                        self.semantic
                            .as_mut()
                            .expect("original source semantic writer")
                            .authority_mut(),
                        self.operation_key,
                        route,
                        #[cfg(test)]
                        &mut || {
                            if source_hook.as_ref().is_some_and(|hook| {
                                hook.stage == TestStage::BeforeSourceMaterialization
                            }) {
                                let hook = source_hook
                                    .take()
                                    .expect("one-shot real source materialization");
                                (hook.run)();
                            }
                        },
                    )? {
                        super::super::index::PreparedSourceScan::Ready(prepared) => {
                            self.preparation = Preparation::SourcePrepared(prepared);
                            return Ok(());
                        }
                        super::super::index::PreparedSourceScan::Capture {
                            selection,
                            continuation,
                        } => {
                            self.prepared = selection;
                            self.preparation = Preparation::SourcePlan(continuation);
                        }
                    }
                }
                _ => unreachable!("checked owned preparation"),
            }
        }
        if self.prepared.intent.is_none() {
            if !matches!(self.preparation, Preparation::Publication { refusal: None })
                || !self.prepared.selected.is_empty()
                || !self.prepared.profile_refusals.is_empty()
            {
                return Err(BuiltinModelError(
                    "view-only refresh cannot select compiler outcomes".to_owned(),
                ));
            }
            let loader = self
                .semantic
                .as_ref()
                .expect("original semantic writer")
                .authority()
                .capture_image_loader()?;
            self.generations.install_selected_loader(loader);
            #[cfg(test)]
            self.run_test_hook(TestStage::BeforeViewPreparation);
            let view = prepare_builtin_view(
                self.base.clone(),
                self.current.clone(),
                &self.compiler,
                self.deployment,
                &self.workspace,
                self.prior.as_ref(),
                None,
                &mut self.image_rows,
                &mut self.generations,
            )?;
            let admission = BuiltinViewAdmission {
                workspace_root: self.base.root(),
                source_root: view.view.basis().root,
            };
            let Head::Unprepared(writer) = std::mem::replace(&mut self.head, Head::Moving) else {
                return Err(BuiltinModelError(
                    "view refresh lost its original writer".to_owned(),
                ));
            };
            self.head = match writer.prepare_refresh_view(
                view.view.clone(),
                backend_engine::Cursor::for_view_root(&view.view),
                &admission,
                view.event.clone(),
            ) {
                Ok(refresh) => Head::Refresh(refresh),
                Err((writer, error)) => {
                    self.head = Head::Unprepared(writer);
                    return Err(BuiltinModelError(format!("prepare view refresh: {error}")));
                }
            };
            self.view = Some(view);
            return Ok(());
        }
        let original = self.prepared.intent.as_ref().ok_or_else(|| {
            BuiltinModelError("publication worker requires an actual source intent".to_owned())
        })?;
        let intent = match self.operation_key {
            Some(key) => original.clone().with_operation_key(key)?,
            None => original.clone(),
        };
        if !self.prepared.profile_refusals.is_empty() && !self.prepared.selected.is_empty() {
            let producer = self
                .prepared
                .selected
                .first()
                .ok_or_else(|| {
                    BuiltinModelError("partial publication lacks an admitted profile".to_owned())
                })?
                .0
                .package();
            if self
                .prepared
                .selected
                .iter()
                .any(|(key, _)| key.package() != producer)
            {
                return Err(BuiltinModelError(
                    "partial candidates span foreign producer namespaces".to_owned(),
                ));
            }
            self.partial_basis = super::super::index::source_capture_summary_for_snapshot(
                &self.base,
                producer,
                self.operation_key,
                None,
            )?;
            if self.partial_basis.is_none() {
                return Err(BuiltinModelError(
                    "partial publication lost its selected source basis".to_owned(),
                ));
            }
        }
        if let Some(progress) = &self.source_observation {
            progress.phase("stage-intent");
        }
        #[cfg(test)]
        self.observed.phase("stage-intent");
        let staged = prepare_builtin_intent_at_snapshot(
            &self.base,
            &intent,
            self.prepared
                .revision_fence
                .as_ref()
                .map(|f| f.canonical_root()),
            Arc::clone(&self.cancellation),
        )?;
        if let Some(progress) = &self.source_observation {
            progress.phase("prepare-workspace");
        }
        #[cfg(test)]
        self.observed.phase("prepare-workspace");
        let candidate = {
            let Head::Unprepared(writer) = &mut self.head else {
                return Err(BuiltinModelError(
                    "private publication already prepared or selected".to_owned(),
                ));
            };
            writer
                .prepare(staged.intent.clone())
                .map_err(|e| BuiltinModelError(format!("prepare index workspace: {e}")))?
        };
        // Enter prepared custody before subsequent fallible snapshot work.
        let Head::Unprepared(writer) = std::mem::replace(&mut self.head, Head::Moving) else {
            unreachable!("original unprepared reservation")
        };
        self.head = Head::Writer(writer);
        let Head::Writer(writer) = &mut self.head else {
            unreachable!("prepared original writer")
        };
        if let Some(progress) = &self.source_observation {
            progress.phase("open-candidate");
        }
        #[cfg(test)]
        self.observed.phase("open-candidate");
        let snapshot = writer
            .candidate_snapshot(&candidate)
            .map_err(|e| BuiltinModelError(format!("open index candidate: {e}")))?;
        // Retain the prepared candidate before any projection can unwind.
        self.candidate = Some(candidate);
        self.staged = Some(staged);
        self.candidate_snapshot = Some(snapshot.clone());
        if matches!(self.preparation, Preparation::SourcePlan(_)) {
            let Preparation::SourcePlan(plan) =
                std::mem::replace(&mut self.preparation, Preparation::Consumed)
            else {
                unreachable!("checked source plan")
            };
            let captures = plan.captures.clone();
            let continuation = plan.finish(
                &snapshot,
                &self.compiler,
                self.semantic
                    .as_mut()
                    .expect("original source semantic writer")
                    .authority_mut(),
                &mut self.source_attempts,
            )?;
            self.preparation = Preparation::SourceCaptured {
                continuation,
                captures,
            };
        }
        let loader = self
            .semantic
            .as_ref()
            .expect("original semantic writer")
            .authority()
            .capture_image_loader()?;
        self.generations.install_selected_loader(loader);
        if let Some(progress) = &self.source_observation {
            progress.phase("prepare-view");
        }
        #[cfg(test)]
        self.observed.phase("prepare-view");
        #[cfg(test)]
        if self
            .test_hook
            .as_ref()
            .is_some_and(|hook| hook.stage == TestStage::BeforeViewPreparation)
        {
            let hook = self
                .test_hook
                .take()
                .expect("one-shot source view preparation");
            (hook.run)();
        }
        let view = prepare_builtin_view(
            snapshot.clone(),
            self.current.clone(),
            &self.compiler,
            self.deployment,
            &self.workspace,
            self.prior.as_ref(),
            Some(&intent),
            &mut self.image_rows,
            &mut self.generations,
        )?;
        let admission = BuiltinViewAdmission {
            workspace_root: snapshot.root(),
            source_root: view.view.basis().root,
        };
        if let Some(progress) = &self.source_observation {
            progress.phase("prepare-read-head");
        }
        #[cfg(test)]
        self.observed.phase("prepare-read-head");
        writer
            .prepare_view(
                self.candidate.as_ref().expect("retained private candidate"),
                view.view.clone(),
                backend_engine::Cursor::for_view_root(&view.view),
                &admission,
                view.event.clone(),
            )
            .map_err(|e| BuiltinModelError(format!("prepare coherent index read head: {e}")))?;
        self.view = Some(view);
        Ok(())
    }

    fn attempt(&mut self, events: &Sender<Event>) -> Result<(), FinishAddError> {
        self.prepare()?;
        if matches!(self.head, Head::Refresh(_) | Head::Refreshed(_)) {
            return self.attempt_refresh(events);
        }
        if matches!(self.preparation, Preparation::SourcePrepared(_)) {
            return Ok(());
        }
        if !self.semantic_committed {
            let source_capture = matches!(self.preparation, Preparation::SourceCaptured { .. });
            let cleanup_phase = self.is_cleanup();
            let staged = self.staged.as_ref().expect("prepared request");
            let request = staged.request_identity();
            let base_root = staged.base_workspace_root();
            let base_sequence = staged.base_workspace_sequence();
            let removals = selected_semantic_removals(&staged.intent);
            let head = &mut self.head;
            let candidate = &mut self.candidate;
            let grant_accepted = &mut self.grant_accepted;
            #[cfg(test)]
            let test_hook = &mut self.test_hook;
            #[cfg(test)]
            let observed = &self.observed;
            let source_observation = self.source_observation.clone();
            let journal = self
                .journal
                .as_mut()
                .expect("original receipt writer")
                .journal_mut();
            let cancellation = &self.cancellation;
            let fence = &self.prepared.revision_fence;
            let operation_key = self.operation_key;
            let plan = self.prepared.partial_plan.clone();
            let mut busy = false;
            #[cfg(test)]
            observed.phase("prepare-selector");
            self.semantic.as_mut().expect("original semantic writer").authority_mut()
                .commit_product_selection_changes(self.prepared.selected.clone(), removals, || {
                    if matches!(head, Head::Writer(_)) {
                        #[cfg(test)]
                        observed.phase("publication-barrier");
                        if !source_capture && !cleanup_phase {
                        prepare_publication_barrier(journal, operation_key, Some(request),
                            base_root, base_sequence, plan).map_err(|e| match e {
                                FinishAddError::PublicationBusy => {
                                    busy = true;
                                    BuiltinModelError("durable publication is temporarily busy".to_owned())
                                }
                                FinishAddError::Failed(e) => e,
                            })?;
                        }
                    }
                    if matches!(head, Head::Writer(_)) {
                        #[cfg(test)]
                        if test_hook.as_ref().is_some_and(|hook| hook.stage == TestStage::BeforeGrantValidation) {
                            let hook = test_hook.take().expect("one-shot post-barrier validation seam");
                            (hook.run)();
                        }
                        // The durable Prepared barrier may block. Revalidate
                        // after it, immediately before asking for the grant.
                        // This proves freshness at this validation instant,
                        // not atomic exclusion of external filesystem writes
                        // during the asynchronous owner-grant handoff. The
                        // selected candidate still owns exact captured bytes.
                        if let Some(fence) = fence.as_ref()
                            && !super::super::super::ingest::compiler_revision_is_current_with_cancellation(
                                fence, Some(cancellation.as_ref()),
                            ).map_err(BuiltinModelError)? {
                            return Err(BuiltinModelError("compiler source or configuration changed before product selection".to_owned()));
                        }
                    }
                    select_head(head, candidate, events, grant_accepted,
                        #[cfg(test)] test_hook, cancellation, source_observation.as_deref(), #[cfg(test)] observed)
                }).map_err(|e| if busy { FinishAddError::PublicationBusy } else { e.into() })?;
            self.semantic_committed = true;
            #[cfg(test)]
            self.run_test_hook(TestStage::AfterSelection);
        }
        let snapshot = self
            .candidate_snapshot
            .as_ref()
            .expect("exact candidate snapshot");
        let view = self.view.as_ref().expect("exact prepared view");
        if !self.projected {
            if let Some(progress) = &self.source_observation {
                progress.phase("project-sql");
            }
            #[cfg(test)]
            self.observed.phase("project-sql");
            let projection = self
                .projection
                .as_mut()
                .expect("original SQL writer")
                .projection_mut();
            project_snapshot_view_deltas(projection, &view.view, &view.outcome.deltas)?;
            self.semantic
                .as_mut()
                .expect("original semantic writer")
                .authority_mut()
                .mark_projections_current()?;
            self.projected = true;
        }
        let staged = self.staged.as_ref().expect("actual request");
        if let Preparation::SourceCaptured { captures, .. } = &self.preparation {
            if let Some(key) = self.operation_key {
                let receipt = super::super::index::source_capture_receipt_for_snapshot(
                    snapshot,
                    captures
                        .keys()
                        .next()
                        .expect("captured source scope")
                        .package(),
                    key,
                    Some(staged.request_identity()),
                )?
                .ok_or_else(|| {
                    BuiltinModelError("source candidate lost its exact capture marker".to_owned())
                })?;
                self.journal
                    .as_mut()
                    .expect("original source receipt writer")
                    .journal_mut()
                    .source_captured(key, receipt)
                    .map_err(|e| BuiltinModelError(format!("record source capture: {e}")))?;
            }
            self.roots = Some(view.outcome.roots.clone());
            return Ok(());
        }
        if let Some(outcome) = self.failure_outcome().cloned() {
            if let Some(key) = self.operation_key {
                let journal = self
                    .journal
                    .as_mut()
                    .expect("original refusal receipt writer")
                    .journal_mut();
                let Some(JournalEntry::Retained(entry)) = journal
                    .entry(key)
                    .map_err(|e| BuiltinModelError(format!("read refusal receipt: {e}")))?
                else {
                    return Err(
                        BuiltinModelError("refusal lost its exact operation".to_owned()).into(),
                    );
                };
                if let Some(source) = entry.source_capture.as_ref() {
                    refresh_index_operation_source_capture_for_snapshot(
                        journal,
                        snapshot,
                        key,
                        entry.source_package(),
                        source,
                    )
                    .map_err(|e| BuiltinModelError(format!("record refusal capture: {e}")))?;
                }
                let (reason, detail, compiler_failure) = index_operation_failure(Some(&outcome));
                match &entry.state {
                    StoredOperationState::Failed {
                        reason: stored_reason,
                        detail: stored_detail,
                        compiler_failure: stored_failure,
                    } if *stored_reason == reason
                        && *stored_detail == detail
                        && *stored_failure == compiler_failure => {}
                    StoredOperationState::Accepted | StoredOperationState::Prepared { .. } => {
                        journal
                            .failed_with_compiler_failure(key, reason, detail, compiler_failure)
                            .map_err(|e| {
                                BuiltinModelError(format!("record capture-only refusal: {e}"))
                            })?;
                    }
                    _ => {
                        return Err(BuiltinModelError(
                            "refusal receipt contradicts its retained terminal".to_owned(),
                        )
                        .into());
                    }
                }
            }
            self.roots = Some(view.outcome.roots.clone());
            self.outcome = Some(outcome.clone());
            return Ok(());
        }
        if let Some(progress) = &self.source_observation {
            progress.phase("publish-receipt");
        }
        #[cfg(test)]
        self.observed.phase("publish-receipt");
        let receipt = backend_library::IndexOperationPublicationReceipt::from_published_view(
            Some(staged.request_identity()),
            *snapshot.commit().id().as_bytes(),
            *snapshot.root().as_bytes(),
            snapshot.sequence(),
            &view.view,
            backend_engine::Cursor::for_view_root(&view.view),
        )
        .map_err(|e| BuiltinModelError(format!("exact index publication receipt: {e}")))?;
        let partial = partial_publication_for_snapshot(
            snapshot,
            self.partial_basis.as_ref(),
            &self.prepared.selected,
            self.requested_reference.clone(),
            self.prepared.profile_refusals.clone(),
            self.operation_key,
            &receipt,
        )?;
        if let Some(key) = self.operation_key {
            let journal = self
                .journal
                .as_mut()
                .expect("original receipt writer")
                .journal_mut();
            let entry = match journal
                .entry(key)
                .map_err(|e| BuiltinModelError(format!("read publication receipt: {e}")))?
            {
                Some(JournalEntry::Retained(entry)) => entry,
                _ => {
                    return Err(BuiltinModelError(
                        "retained publication receipt disappeared".to_owned(),
                    )
                    .into());
                }
            };
            let already_published = match &entry.state {
                StoredOperationState::Published {
                    receipt: stored, ..
                } => {
                    if stored != &receipt || partial.is_some() || entry.planned_partial.is_some() {
                        return Err(BuiltinModelError(
                            "durable publication receipt differs from the retained coherent result"
                                .to_owned(),
                        )
                        .into());
                    }
                    true
                }
                StoredOperationState::PartiallyPublished {
                    receipt: stored, ..
                } => {
                    if stored != &receipt
                        || partial.is_none()
                        || entry.planned_partial != self.prepared.partial_plan
                    {
                        return Err(BuiltinModelError(
                            "durable partial receipt differs from the retained profile partition"
                                .to_owned(),
                        )
                        .into());
                    }
                    true
                }
                _ => false,
            };
            if !already_published {
                if let Some(source) = entry.source_capture.as_ref() {
                    refresh_index_operation_source_capture_for_snapshot(
                        journal,
                        snapshot,
                        key,
                        entry.source_package(),
                        source,
                    )
                    .map_err(|e| BuiltinModelError(format!("record semantic capture: {e}")))?;
                }
                // A failed terminal replacement remains Pending, with the
                // same original writer and selected capabilities to retry.
                journal
                    .published(key, receipt)
                    .map_err(|e| BuiltinModelError(format!("record index publication: {e}")))?;
            }
        }
        #[cfg(test)]
        self.run_test_hook(TestStage::AfterReceipt);
        self.roots = Some(
            self.view
                .as_ref()
                .expect("prepared view")
                .outcome
                .roots
                .clone(),
        );
        self.outcome = Some(match partial {
            Some(p) => backend_library::IndexJobOutcome::PartiallyPublished(p),
            None => backend_library::IndexJobOutcome::Published,
        });
        Ok(())
    }

    fn attempt_refresh(&mut self, events: &Sender<Event>) -> Result<(), FinishAddError> {
        if !self.grant_accepted {
            prepare_publication_barrier(
                self.journal
                    .as_mut()
                    .expect("original receipt writer")
                    .journal_mut(),
                self.operation_key,
                None,
                *self.base.root().as_bytes(),
                self.base.sequence(),
                None,
            )?;
            #[cfg(test)]
            self.run_test_hook(TestStage::BeforeGrantValidation);
            if let Some(fence) = self.prepared.revision_fence.as_ref()
                && !super::super::super::ingest::compiler_revision_is_current_with_cancellation(
                    fence,
                    Some(self.cancellation.as_ref()),
                )
                .map_err(BuiltinModelError)?
            {
                return Err(BuiltinModelError(
                    "source changed before view refresh approval".to_owned(),
                )
                .into());
            }
            let (reply, received) = std::sync::mpsc::sync_channel(1);
            events
                .send(Event::RefreshApproval {
                    base_root: *self.base.root().as_bytes(),
                    base_sequence: self.base.sequence(),
                    base_view_root: *self.current.root().as_bytes(),
                    base_cursor: self.current_cursor,
                    cancellation: Arc::clone(&self.cancellation),
                    reply,
                })
                .map_err(|_| {
                    BuiltinModelError("owner retired before view refresh approval".to_owned())
                })?;
            received.recv().map_err(|_| {
                BuiltinModelError("owner retired without view refresh approval".to_owned())
            })??;
            // Cancellation loses at the actor's approval load. This approves
            // only the derived view for an already-selected immutable base.
            self.grant_accepted = true;
        }
        if matches!(self.head, Head::Refresh(_)) {
            let Head::Refresh(refresh) = std::mem::replace(&mut self.head, Head::Moving) else {
                unreachable!("checked refresh")
            };
            match refresh.persist() {
                Ok(published) => self.head = Head::Refreshed(published),
                Err((refresh, error)) => {
                    self.head = Head::Refresh(refresh);
                    return Err(BuiltinModelError(format!("persist view refresh: {error}")).into());
                }
            }
        }
        let view = self.view.as_ref().expect("exact refreshed view");
        if !self.projected {
            project_snapshot_view_deltas(
                self.projection
                    .as_mut()
                    .expect("original SQL writer")
                    .projection_mut(),
                &view.view,
                &view.outcome.deltas,
            )?;
            self.semantic
                .as_mut()
                .expect("original semantic writer")
                .authority_mut()
                .mark_projections_current()?;
            self.projected = true;
        }
        if let Some(key) = self.operation_key {
            let receipt = backend_library::IndexOperationPublicationReceipt::from_published_view(
                None,
                *self.base.commit().id().as_bytes(),
                *self.base.root().as_bytes(),
                self.base.sequence(),
                &view.view,
                backend_engine::Cursor::for_view_root(&view.view),
            )
            .map_err(|e| BuiltinModelError(format!("exact refreshed-view receipt: {e}")))?;
            let journal = self
                .journal
                .as_mut()
                .expect("original receipt writer")
                .journal_mut();
            match journal
                .entry(key)
                .map_err(|e| BuiltinModelError(format!("read refreshed-view receipt: {e}")))?
            {
                Some(JournalEntry::Retained(StoredOperation {
                    state:
                        StoredOperationState::Published {
                            receipt: stored, ..
                        },
                    ..
                })) if stored == receipt => {}
                Some(JournalEntry::Retained(StoredOperation {
                    state: StoredOperationState::Prepared { .. },
                    ..
                })) => journal.published(key, receipt).map_err(|e| {
                    BuiltinModelError(format!("record refreshed-view receipt: {e}"))
                })?,
                _ => {
                    return Err(BuiltinModelError(
                        "view refresh lost its exact Prepared receipt".to_owned(),
                    )
                    .into());
                }
            }
        }
        self.roots = Some(view.outcome.roots.clone());
        self.outcome = Some(backend_library::IndexJobOutcome::Published);
        Ok(())
    }

    #[cfg(test)]
    fn run_test_hook(&mut self, stage: TestStage) {
        if self
            .test_hook
            .as_ref()
            .is_some_and(|hook| hook.stage == stage)
        {
            let hook = self.test_hook.take().expect("one-shot exact stage hook");
            (hook.run)();
        }
    }

    // A private last observation, never a publication receipt or a capability.
    // In particular, a selected workspace cannot be rolled back or reported as
    // cancelled merely because its view sink or later projection is pending.
    fn observe_pending(&self, outer: &BuiltinModelError) {
        if !cfg!(unix) || !self.grant_accepted {
            return;
        }
        let (phase, workspace_selected, detail) = match &self.head {
            Head::Pending(ReadHeadPublicationFailure::SelectedPending { error, .. }) => {
                ("selected-view-persistence", Some(true), error.to_string())
            }
            Head::Pending(ReadHeadPublicationFailure::Unsettled { error, .. }) => {
                ("physical-selection-unsettled", None, error.to_string())
            }
            Head::Pending(ReadHeadPublicationFailure::Rejected { error, .. }) => {
                ("physical-selection-rejected", Some(false), error.to_string())
            }
            Head::Published(_) => {
                let phase = if !self.semantic_committed {
                    "semantic-selection"
                } else if !self.projected {
                    "post-head-projection"
                } else {
                    "terminal-receipt"
                };
                (phase, Some(true), outer.to_string())
            }
            _ => return,
        };
        let mut boundary = detail.len().min(1024);
        while !detail.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let snapshot = self.candidate_snapshot.as_ref();
        let receipt = serde_json::json!({
            "schema": "nudox.index-publication-pending-observation.v1",
            "process_id": std::process::id(),
            "observed_unix_millis": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).ok()
                .and_then(|value| u64::try_from(value.as_millis()).ok()),
            "operation_key": self.operation_key,
            "candidate_workspace_root": snapshot.map(|value| value.root().to_bytes()),
            "candidate_workspace_sequence": snapshot.map(WorkspaceSnapshot::sequence),
            "phase": phase,
            "workspace_selected": workspace_selected,
            "detail": &detail[..boundary],
            "detail_truncated": boundary < detail.len(),
            "terminal_receipt": false,
            "readiness_authority": false,
        });
        if let Ok(bytes) = serde_json::to_vec(&receipt)
            && bytes.len() <= 8192
        {
            let _ = backend_platform::durable::write_private_atomic_unflushed(
                &self.workspace.join("index-publication-pending-v1.json"),
                &bytes,
            );
        }
    }

    fn step(&mut self, events: &Sender<Event>) -> Disposition {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.settlement_only {
                Err(self
                    .unselected_error
                    .clone()
                    .unwrap_or_else(|| {
                        BuiltinModelError("retrying retained writer settlement".to_owned())
                    })
                    .into())
            } else {
                self.attempt(events)
            }
        }))
        .unwrap_or_else(|_| {
            Err(BuiltinModelError(
                "index publication unwound; original writers remain retained".to_owned(),
            )
            .into())
        });
        let error = match result {
            Ok(()) => {
                return if matches!(self.head, Head::Refreshed(_)) {
                    Disposition::Refreshed
                } else if matches!(self.preparation, Preparation::SourceCaptured { .. }) {
                    Disposition::SourceCaptured
                } else if matches!(self.preparation, Preparation::SourcePrepared(_)) {
                    Disposition::SourcePrepared
                } else {
                    Disposition::Ready
                };
            }
            Err(FinishAddError::PublicationBusy) => {
                return Disposition::Pending(BuiltinModelError(
                    "durable publication is temporarily busy".to_owned(),
                ));
            }
            Err(FinishAddError::Failed(e)) => e,
        };
        self.observe_pending(&error);
        if self.is_cleanup() {
            // Its terminal decision is already frozen. Keep the original
            // cleanup plan/writers for explicit retry instead of dropping
            // unresolved attempt retirement or synthesizing another terminal.
            return Disposition::Pending(error);
        }
        if !self.grant_accepted && matches!(self.head, Head::Refresh(_)) {
            let Head::Refresh(refresh) = std::mem::replace(&mut self.head, Head::Moving) else {
                unreachable!("checked unapproved refresh")
            };
            self.head = Head::Unprepared(refresh.into_writer());
        }
        if matches!(self.head, Head::Unprepared(_)) {
            // No candidate/grant was ever prepared. Existing owner return
            // validates the same ungranted reservation without manufacturing
            // a physical candidate proof for the writer's Ready state.
            self.settlement_only = true;
            self.unselected_error.get_or_insert_with(|| error.clone());
            return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.record_unselected_outcome(&error)
            })) {
                Ok(Ok(())) => Disposition::Unselected(error),
                Ok(Err(error)) => Disposition::Pending(error),
                Err(_) => Disposition::Pending(BuiltinModelError(
                    "unprepared failure settlement unwound; original writers retained".to_owned(),
                )),
            };
        }
        // Physical selection, not cancellation or a best-effort notification,
        // decides whether this attempt can become an ordinary failed terminal.
        let writer = match &mut self.head {
            Head::Writer(writer) => Some(writer),
            Head::Pending(
                ReadHeadPublicationFailure::Rejected { writer, .. }
                | ReadHeadPublicationFailure::Unsettled { writer, .. },
            ) => Some(writer),
            Head::Pending(ReadHeadPublicationFailure::SelectedPending { .. })
            | Head::Published(_)
            | Head::Unprepared(_)
            | Head::Refresh(_)
            | Head::Refreshed(_)
            | Head::Moving => None,
        };
        if let Some(writer) = writer {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                writer.prove_unselected()
            })) {
                Ok(Ok(proof)) => {
                    self.proof = Some(proof);
                    // Seal the original attempt before any capture-only
                    // terminal commit can advance the selected workspace.
                    // A failed journal write retains all original writers and
                    // retries this settlement, never the prepared publication.
                    self.settlement_only = true;
                    self.unselected_error.get_or_insert_with(|| error.clone());
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.record_unselected_outcome(&error)
                    })) {
                        Ok(Ok(())) => Disposition::Unselected(error),
                        Ok(Err(error)) => Disposition::Pending(error),
                        Err(_) => Disposition::Pending(BuiltinModelError(
                            "unselected receipt write unwound; original writers remain retained"
                                .to_owned(),
                        )),
                    }
                }
                Ok(Err(_)) | Err(_) => Disposition::Pending(error),
            }
        } else {
            Disposition::Pending(error)
        }
    }

    fn record_unselected_outcome(
        &mut self,
        error: &BuiltinModelError,
    ) -> Result<(), BuiltinModelError> {
        let outcome = self
            .outcome
            .clone()
            .or_else(|| self.failure_outcome().cloned())
            .unwrap_or_else(|| {
                if self.cancellation_won() {
                    backend_library::IndexJobOutcome::Cancelled
                } else {
                    backend_library::IndexJobOutcome::Failed(bounded_index_detail(error.clone()))
                }
            });
        let reason = if self.cancellation_won() {
            backend_extension_turso::CandidateAttemptRetirementReason::Cancelled
        } else {
            backend_extension_turso::CandidateAttemptRetirementReason::Failed
        };
        for attempt in &self.source_attempts {
            self.semantic
                .as_mut()
                .expect("original source semantic writer")
                .authority_mut()
                .retire_candidate_attempt(attempt, reason)?;
        }
        // Freeze arbitration across an unavailable receipt writer. A retry
        // cannot change an already chosen failure into a later cancellation.
        self.outcome = Some(outcome.clone());
        if let Some(key) = self.operation_key {
            let (reason, detail, compiler_failure) = index_operation_failure(Some(&outcome));
            let journal = self
                .journal
                .as_mut()
                .ok_or_else(|| {
                    BuiltinModelError(
                        "unselected publication lost its original receipt writer".to_owned(),
                    )
                })?
                .journal_mut();
            let retained = journal
                .entry(key)
                .map_err(|e| BuiltinModelError(format!("read unselected index receipt: {e}")))?;
            match retained {
                Some(JournalEntry::Retained(entry)) => match entry.state {
                    StoredOperationState::Accepted | StoredOperationState::Prepared { .. } => {
                        journal
                            .failed_with_compiler_failure(key, reason, detail, compiler_failure)
                            .map_err(|e| {
                                BuiltinModelError(format!("record unselected index receipt: {e}"))
                            })?;
                    }
                    StoredOperationState::Failed {
                        reason: stored_reason,
                        detail: stored_detail,
                        compiler_failure: stored_failure,
                    } if stored_reason == reason
                        && stored_detail == detail
                        && stored_failure == compiler_failure => {}
                    _ => {
                        return Err(BuiltinModelError(
                            "unselected publication contradicts its retained terminal receipt"
                                .to_owned(),
                        ));
                    }
                },
                _ => {
                    return Err(BuiltinModelError(
                        "unselected publication lost its admitted operation receipt".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn settlement_retry(&mut self) {
        self.settlement_only = true;
    }

    pub(super) fn can_cancel(&self) -> bool {
        !self.is_cleanup()
            && !self.settlement_only
            && !self.grant_accepted
            && matches!(
                self.head,
                Head::Unprepared(_) | Head::Writer(_) | Head::Refresh(_)
            )
    }

    pub(super) fn cancellation_won(&self) -> bool {
        !self.grant_accepted && self.cancellation.load(Ordering::Acquire)
    }

    pub(super) fn take_unselected_writer(&mut self) -> Option<ReadHeadWriter<BuiltinModel>> {
        match std::mem::replace(&mut self.head, Head::Moving) {
            Head::Unprepared(writer) | Head::Writer(writer) => Some(writer),
            Head::Pending(ReadHeadPublicationFailure::Rejected {
                writer,
                candidate,
                grant,
                ..
            }) => {
                // The returned proof seals this writer against future publish;
                // the no-longer-usable private tokens retire on this worker.
                self.candidate = Some(candidate);
                drop(grant);
                Some(writer)
            }
            Head::Pending(ReadHeadPublicationFailure::Unsettled { writer, .. }) => Some(writer),
            other => {
                self.head = other;
                None
            }
        }
    }
}

fn select_head(
    head: &mut Head,
    candidate: &mut Option<PreparedWorkspaceCandidate>,
    events: &Sender<Event>,
    grant_accepted: &mut bool,
    #[cfg(test)] test_hook: &mut Option<TestHook>,
    cancellation: &Arc<AtomicBool>,
    source_observation: Option<&crate::builtin::index_work_observation::IndexWorkObservation>,
    #[cfg(test)] observed: &TestObservation,
) -> Result<(), BuiltinModelError> {
    if matches!(head, Head::Published(_)) {
        return Ok(());
    }
    let result = match head {
        Head::Writer(_) => {
            let private = candidate.take().ok_or_else(|| {
                BuiltinModelError("publication lost its private prepared candidate".to_owned())
            })?;
            let claim = private.claim();
            let (reply, receive) = std::sync::mpsc::sync_channel(1);
            if let Some(progress) = source_observation {
                progress.phase("await-owner-grant");
            }
            #[cfg(test)]
            observed.phase("await-owner-grant");
            events
                .send(Event::Grant {
                    claim,
                    reply,
                    cancellation: Arc::clone(cancellation),
                })
                .map_err(|_| {
                    BuiltinModelError("owner retired before publication grant".to_owned())
                })?;
            // The actor alone arbitrates cancellation at grant_candidate's
            // AtomicBool load. Cancellation after that load loses even if the
            // worker has not entered its physical publish call yet.
            let grant = receive
                .recv()
                .map_err(|_| {
                    BuiltinModelError("owner retired without granting publication".to_owned())
                })?
                .map_err(|e| BuiltinModelError(format!("index publication grant refused: {e}")))?;
            *grant_accepted = true;
            #[cfg(test)]
            {
                observed.granted.store(true, Ordering::Release);
                observed.phase("accepted-owner-grant");
            }
            #[cfg(test)]
            if test_hook
                .as_ref()
                .is_some_and(|hook| hook.stage == TestStage::AfterGrant)
            {
                let hook = test_hook.take().expect("one-shot accepted grant hook");
                (hook.run)();
            }
            let Head::Writer(writer) = std::mem::replace(head, Head::Moving) else {
                unreachable!()
            };
            if let Some(progress) = source_observation {
                progress.phase("physical-publish");
            }
            #[cfg(test)]
            observed.phase("physical-publish");
            writer.publish(private, grant)
        }
        Head::Pending(_) => {
            let Head::Pending(pending) = std::mem::replace(head, Head::Moving) else {
                unreachable!()
            };
            match pending {
                ReadHeadPublicationFailure::Unsettled { writer, .. } => {
                    writer.reconcile_publication()
                }
                ReadHeadPublicationFailure::SelectedPending { selected, .. } => selected.persist(),
                rejected @ ReadHeadPublicationFailure::Rejected { .. } => Err(rejected),
            }
        }
        Head::Unprepared(_) | Head::Published(_) | Head::Refresh(_) | Head::Refreshed(_) => {
            unreachable!()
        }
        Head::Moving => {
            return Err(BuiltinModelError(
                "publication capability transfer incomplete".to_owned(),
            ));
        }
    };
    match result {
        Ok(published) => {
            *head = Head::Published(published);
            Ok(())
        }
        Err(pending) => {
            *head = Head::Pending(pending);
            Err(BuiltinModelError(
                "physical index publication or durable view acknowledgement remains pending"
                    .to_owned(),
            ))
        }
    }
}

impl PublicationWorker {
    pub(super) fn spawn(
        #[allow(unused_mut)] mut work: Box<PublicationWork>,
    ) -> Result<Self, (Box<PublicationWork>, std::io::Error)> {
        #[cfg(test)]
        if std::mem::take(&mut work.test_spawn_refusal) {
            return Err((
                work,
                std::io::Error::other("injected original worker thread startup refusal"),
            ));
        }
        #[cfg(test)]
        let observed = Arc::clone(&work.observed);
        let (startup, receive) = std::sync::mpsc::sync_channel::<Box<PublicationWork>>(1);
        let (events, observations) = std::sync::mpsc::channel();
        let (settle, settlements) = std::sync::mpsc::channel();
        let handle = match std::thread::Builder::new()
            .name("locald-index-publish".to_owned())
            .spawn(move || {
                let Ok(mut work) = receive.recv() else {
                    return;
                };
                loop {
                    let disposition = work.step(&events);
                    if let Some(progress) = &work.source_observation {
                        progress.phase("await-owner-settlement-or-explicit-retry");
                    }
                    if events
                        .send(Event::Complete(Completion { work, disposition }))
                        .is_err()
                    {
                        return;
                    }
                    match settlements.recv() {
                        Ok(Settlement::Retry(returned)) => work = returned,
                        Ok(Settlement::Retire(returned, retired)) => {
                            if let Some(progress) = &returned.source_observation {
                                progress.phase("retire-original-payloads");
                            }
                            #[cfg(test)]
                            returned.observed.phase("retire-original-payloads");
                            if let Some(progress) = returned.source_observation.clone() {
                                // Keep the tuple's original left-to-right destruction
                                // order. The opt-in observer owns no publication
                                // capability and distinguishes destruction from the
                                // owner's subsequent join/poll interval.
                                progress.phase("retire-returned-work");
                                drop(returned);
                                progress.phase("retire-prior-read-payloads");
                                drop(retired);
                                progress.phase("retirement-complete");
                            } else {
                                drop((returned, retired));
                            }
                            return;
                        }
                        Err(_) => return,
                    }
                }
            }) {
            Ok(handle) => handle,
            Err(error) => return Err((work, error)),
        };
        if let Err(error) = startup.send(work) {
            let _ = handle.join();
            return Err((
                error.0,
                std::io::Error::other("publication startup transfer refused"),
            ));
        }
        Ok(Self {
            events: observations,
            settle,
            handle,
            #[cfg(test)]
            observed,
        })
    }

    pub(super) fn spawn_retirement(
        work: Box<PublicationWork>,
        retired: Retired,
    ) -> Result<Self, (Box<PublicationWork>, Retired, std::io::Error)> {
        #[cfg(test)]
        let observed = Arc::clone(&work.observed);
        let (start, receive) = std::sync::mpsc::sync_channel::<(Box<PublicationWork>, Retired)>(1);
        let (_events, observations) = std::sync::mpsc::channel();
        let (settle, _settlements) = std::sync::mpsc::channel();
        let handle = match std::thread::Builder::new()
            .name("locald-index-retire".to_owned())
            .spawn(move || {
                if let Ok(payload) = receive.recv() {
                    drop(payload);
                }
            }) {
            Ok(handle) => handle,
            Err(error) => return Err((work, retired, error)),
        };
        if let Err(error) = start.send((work, retired)) {
            let _ = handle.join();
            return Err((
                error.0.0,
                error.0.1,
                std::io::Error::other("retirement startup transfer refused"),
            ));
        }
        Ok(Self {
            events: observations,
            settle,
            handle,
            #[cfg(test)]
            observed,
        })
    }

    pub(super) fn poll(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    pub(super) fn retry(&self, work: Box<PublicationWork>) -> Result<(), Box<PublicationWork>> {
        self.settle
            .send(Settlement::Retry(work))
            .map_err(|e| match e.0 {
                Settlement::Retry(work) => work,
                Settlement::Retire(..) => unreachable!(),
            })
    }

    pub(super) fn retire(&self, work: Box<PublicationWork>, retired: Retired) {
        // This owned receiver stays alive until retirement is acknowledged;
        // no fallible product work occurs between export and this send.
        self.settle
            .send(Settlement::Retire(work, retired))
            .unwrap_or_else(|_| panic!("owned publication retirement receiver disappeared"));
    }

    pub(super) fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    #[cfg(test)]
    pub(super) fn observed_state(&self) -> String {
        self.observed.snapshot()
    }

    pub(super) fn join(self) {
        self.handle
            .join()
            .expect("publication work catches owned-writer unwinds");
    }

    pub(super) fn close(self, completion: Option<Completion>) {
        if let Some(completion) = completion {
            self.retire(completion.work, Retired::default());
        } else {
            // Receiving until completion drops every unissued grant sender,
            // so a worker cannot stay blocked behind owner retirement. Any
            // exported unique writer is sent back for worker-side destruction.
            while let Ok(event) = self.events.recv() {
                match event {
                    Event::Grant { reply, .. } => drop(reply),
                    Event::RefreshApproval { reply, .. } => drop(reply),
                    Event::Complete(completion) => {
                        self.retire(completion.work, Retired::default());
                        break;
                    }
                }
            }
        }
        self.join();
    }
}
