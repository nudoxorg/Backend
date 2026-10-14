//! Durable workspace publications keep the original writers on one owned
//! thread. The actor grants one checked claim and later installs its coherent
//! result; it never runs this thread's preparation, fsync or SQL projection.
//!
//! Two transitions use this worker. The final publication installs the
//! compiled product. The source capture runs first: it turns a finished scan
//! into the source rows and Pending markers, stages and commits them, installs
//! their structural view, and prepares the compiler attempts, all here rather
//! than on the actor, so a large package never stops the owner from serving
//! reads.
use super::super::index::{
    ScanCapture, ScanContinuation, ScanPlan, prepare_scan_plan, source_capture_receipt_for_snapshot,
};
use super::super::index_operation::PublicationJournalWriter;
use super::*;
use crate::builtin::generation_residence::SemanticGenerationResidence;
use crate::builtin::semantic_authority::{DetachedSemanticAuthority, RetiredSemanticRead};
use crate::builtin::sql_projection::ProjectionWriter;
use crate::builtin::view_build::ImageRowResidence;
use crate::builtin::view_publish::PublishedRoots;
use crate::builtin::{BuiltinViewAdmission, PreparedBuiltinView, prepare_builtin_view};
use backend_engine::{
    PreparedWorkspaceCandidate, PublishGrant, PublishedReadHead, ReadHeadPublicationFailure,
    ReadHeadWriter, RetiredReadHead, UnselectedWorkspaceCandidate, WorkspaceCandidateClaim,
    WorkspaceError, WorkspaceSnapshot,
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
    base: WorkspaceSnapshot,
    current: backend_engine::ViewRoot,
    compiler: LocalCompilerClient,
    deployment: crate::builtin::SemanticDeployment,
    workspace: std::path::PathBuf,
    prior: Option<PublishedRoots>,
    prepared: PreparedProductSelection,
    requested_reference: backend_library::PackageReference,
    operation_key: Option<backend_library::IndexOperationKey>,
    cancellation: Arc<AtomicBool>,
    /// A finished scan this work still has to turn into its transition. Present
    /// only for a source capture, until [`Self::prepare`] consumes it.
    scan: Option<IndexScanResult>,
    /// Whether this is the source capture rather than the final publication.
    capture: bool,
    /// Whether this publishes a source transition whose job outcome is already
    /// decided (a refusal, or the closure of a failed job's captures): it ends
    /// once the transition is selected and projected, with no publication receipt.
    terminal: bool,
    /// Whether that transition only closes a failed job's Pending captures. It
    /// is neither announced by a Prepared marker nor keyed to an operation.
    closure: bool,
    /// The decided outcome, reported once the transition is selected.
    decided: Option<backend_library::IndexJobOutcome>,
    /// What the capture continues into once its transition is selected.
    continuation: Option<ScanContinuation>,
    /// Pending markers the capture installs; the owner records them so a
    /// later terminal can close them.
    pub(super) captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    /// The capture's hand-off to the owner, set when the work is complete.
    pub(super) next: Option<CaptureNext>,
    source_captured: bool,
    /// Ingest counts of the root this work commits, computed here so the owner
    /// can serve status from the new root without counting it itself.
    pub(super) progress: Option<ProgressMemo>,
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
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TestStage {
    BeforeScan,
    BeforeStage,
    AfterGrant,
    AfterSelection,
    AfterReceipt,
}
#[cfg(test)]
pub(super) struct TestHook {
    pub(super) stage: TestStage,
    pub(super) run: Box<dyn FnOnce() + Send>,
}

#[cfg(test)]
impl TestHook {
    /// Whether the hook is aimed at the source capture rather than at a later
    /// publication.
    pub(super) fn targets_capture(&self) -> bool {
        self.stage == TestStage::BeforeScan
    }
}

pub(super) enum Head {
    Writer(ReadHeadWriter<BuiltinModel>),
    Pending(ReadHeadPublicationFailure<BuiltinModel>),
    Published(PublishedReadHead<BuiltinModel>),
    Moving,
}

pub(super) enum Disposition {
    Ready,
    /// A source capture found nothing to commit. No candidate was staged or
    /// selected, so the writers return exactly as an unselected attempt's do,
    /// but the outcome is the prepared product selection, not a failure.
    Unchanged,
    /// A verified unchanged physical base permits ordinary failure/cancel.
    Unselected(BuiltinModelError),
    /// The same original capabilities remain available for explicit retry.
    Pending(BuiltinModelError),
}

/// What a source capture hands the owner once its work is complete.
pub(super) enum CaptureNext {
    /// The committed capture continues into compilation, or was refused while
    /// admitting its compiler sources.
    Compile(Result<DeferredIndex, BuiltinModelError>),
    /// No profile needed compiling: this is the whole product selection.
    Ready(PreparedProductSelection),
}

pub(super) struct Completion {
    pub(super) work: Box<PublicationWork>,
    pub(super) disposition: Disposition,
}

pub(super) enum Event {
    Grant {
        claim: WorkspaceCandidateClaim,
        reply: SyncSender<Result<PublishGrant, WorkspaceError>>,
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
        compiler: LocalCompilerClient,
        deployment: crate::builtin::SemanticDeployment,
        workspace: std::path::PathBuf,
        prior: Option<PublishedRoots>,
        prepared: PreparedProductSelection,
        requested_reference: backend_library::PackageReference,
        operation_key: Option<backend_library::IndexOperationKey>,
        cancellation: Arc<AtomicBool>,
    ) -> Self {
        Self {
            semantic,
            journal,
            projection,
            image_rows,
            generations,
            head: read_head.map_or(Head::Moving, Head::Writer),
            proof: None,
            roots: None,
            outcome: None,
            base,
            current,
            compiler,
            deployment,
            workspace,
            prior,
            prepared,
            requested_reference,
            operation_key,
            cancellation,
            scan: None,
            capture: false,
            terminal: false,
            closure: false,
            decided: None,
            continuation: None,
            captures: BTreeMap::new(),
            next: None,
            source_captured: false,
            progress: None,
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
        }
    }

    /// Turns this work into a source capture: it prepares the transition from
    /// the finished scan itself, then commits it ahead of compilation.
    pub(super) fn set_scan(&mut self, scan: IndexScanResult) {
        self.scan = Some(scan);
        self.capture = true;
    }

    /// Makes this work close a failed job's Pending captures. The prepared
    /// intent is the whole transition; `outcome` is the job's already decided
    /// terminal result, carried unchanged to the owner.
    pub(super) fn set_closure(&mut self, outcome: backend_library::IndexJobOutcome) {
        self.terminal = true;
        self.closure = true;
        self.decided = Some(outcome);
    }

    /// Makes this work publish a refusal: every profile was refused, so the
    /// prepared intent only records their terminal captures. It is announced
    /// and keyed like the final publication, but its outcome is `outcome`.
    pub(super) fn set_refusal(&mut self, outcome: backend_library::IndexJobOutcome) {
        self.terminal = true;
        self.decided = Some(outcome);
    }

    /// Whether this work only closes a failed job's captures.
    pub(super) const fn is_closure(&self) -> bool {
        self.closure
    }

    /// Whether this work publishes a transition whose job outcome was decided.
    pub(super) const fn is_decided(&self) -> bool {
        self.terminal
    }

    /// The decided outcome, while this work has not yet been selected.
    pub(super) fn take_decided(&mut self) -> Option<backend_library::IndexJobOutcome> {
        self.decided.take()
    }

    /// Whether this is the source capture rather than the final publication.
    pub(super) const fn is_capture(&self) -> bool {
        self.capture
    }

    /// A capture whose scan needed no transition: nothing was staged, so the
    /// writers return untouched.
    fn nothing_to_commit(&self) -> bool {
        matches!(self.next, Some(CaptureNext::Ready(_)))
    }

    /// Prepares the capture transition from the scan on this worker: source
    /// and semantic admission, candidate observations, row changes and Pending
    /// markers. It runs against the base snapshot the head writer was
    /// reserved at, so the grant refuses a transition whose base moved.
    fn prepare_scan(&mut self, scan: IndexScanResult) -> Result<(), BuiltinModelError> {
        #[cfg(test)]
        self.run_test_hook(TestStage::BeforeScan);
        let authority = self
            .semantic
            .as_mut()
            .expect("original semantic writer")
            .authority_mut();
        let plan = match prepare_scan_plan(
            &self.base,
            scan,
            &self.compiler,
            authority,
            self.operation_key,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                // A scan the owner admits but cannot use is a typed refusal,
                // as it was when the owner prepared scans itself; only an
                // explicit cancellation stays one.
                if !self.cancellation.load(Ordering::Acquire) {
                    self.outcome = Some(backend_library::IndexJobOutcome::Refused(
                        bounded_index_detail(error.clone()),
                    ));
                }
                return Err(error);
            }
        };
        match plan {
            ScanPlan::Ready(prepared) => self.next = Some(CaptureNext::Ready(prepared)),
            ScanPlan::Capture(capture) => {
                let ScanCapture {
                    intent,
                    continuation,
                } = *capture;
                self.captures = continuation.captures.clone();
                self.prepared.intent = Some(intent);
                self.prepared.revision_fence = Some(continuation.revision_fence.clone());
                self.continuation = Some(continuation);
            }
        }
        Ok(())
    }

    /// Projects the selected capture's structural view into SQL, then runs the
    /// capture's remaining steps.
    fn finish_published_capture(&mut self) -> Result<(), BuiltinModelError> {
        if !self.projected {
            #[cfg(test)]
            self.observed.phase("project-sql");
            let view = self.view.as_ref().expect("exact prepared view");
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
        self.finish_capture()
    }

    /// The steps after a capture is selected. The keyed operation's structural
    /// receipt is a retryable write; compiler admission is not retried, its
    /// refusal is the capture's hand-off to the owner.
    fn finish_capture(&mut self) -> Result<(), BuiltinModelError> {
        if self.next.is_some() {
            return Ok(());
        }
        let snapshot = self
            .candidate_snapshot
            .as_ref()
            .expect("exact candidate snapshot");
        if !self.source_captured {
            if let Some(key) = self.operation_key {
                let identity = self
                    .staged
                    .as_ref()
                    .expect("prepared request")
                    .request_identity();
                let package = self
                    .continuation
                    .as_ref()
                    .expect("capture continuation")
                    .package_reference();
                let receipt =
                    source_capture_receipt_for_snapshot(snapshot, package, key, Some(identity))?
                        .ok_or_else(|| {
                            BuiltinModelError(
                        "source capture root does not retain its exact keyed operation marker"
                            .to_owned(),
                    )
                        })?;
                self.journal
                    .as_mut()
                    .expect("original receipt writer")
                    .journal_mut()
                    .source_captured(key, receipt)
                    .map_err(|error| {
                        BuiltinModelError(format!("persist source-capture receipt: {error}"))
                    })?;
            }
            self.source_captured = true;
        }
        let continuation = self.continuation.take().expect("capture continuation");
        let authority = self
            .semantic
            .as_mut()
            .expect("original semantic writer")
            .authority_mut();
        self.next = Some(CaptureNext::Compile(continuation.into_deferred(
            snapshot,
            &self.compiler,
            authority,
        )));
        Ok(())
    }

    fn prepare(&mut self) -> Result<(), BuiltinModelError> {
        if self.view.is_some() {
            return Ok(());
        }
        let mut lp = crate::owner_trace::laps();
        if let Some(scan) = self.scan.take() {
            self.prepare_scan(scan)?;
        }
        lp.lap("worker.prepare_scan");
        if self.nothing_to_commit() {
            return Ok(());
        }
        // A capture's intent carries every source row of the scan. It is staged
        // by value and nothing reads it afterwards, so it is moved rather than
        // copied twice.
        let original = if self.capture {
            self.prepared.intent.take()
        } else {
            self.prepared.intent.clone()
        }
        .ok_or_else(|| {
            BuiltinModelError("publication worker requires an actual source intent".to_owned())
        })?;
        let intent = match self.operation_key {
            Some(key) => original.with_operation_key(key)?,
            None => original,
        };
        // A refusal publishes no generation, so there is no partial basis to bind.
        if !self.terminal && !self.prepared.profile_refusals.is_empty() {
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
        #[cfg(test)]
        self.run_test_hook(TestStage::BeforeStage);
        #[cfg(test)]
        self.observed.phase("stage-intent");
        // A capture stages its source rows only; the final publication also
        // stages the raw bytes of any changed source file under the scan root.
        let source_root = if self.capture {
            None
        } else {
            self.prepared
                .revision_fence
                .as_ref()
                .map(|f| f.canonical_root())
        };
        let (staged, edit) = if self.capture {
            let staged = prepare_owned_builtin_intent_at_snapshot(
                &self.base,
                intent,
                source_root,
                Arc::clone(&self.cancellation),
            )?;
            (staged, None)
        } else {
            let staged = prepare_builtin_intent_at_snapshot(
                &self.base,
                &intent,
                source_root,
                Arc::clone(&self.cancellation),
            )?;
            (staged, Some(intent))
        };
        lp.lap("worker.stage_intent");
        let Head::Writer(writer) = &mut self.head else {
            return Err(BuiltinModelError(
                "private publication was already selected".to_owned(),
            ));
        };
        #[cfg(test)]
        self.observed.phase("prepare-workspace");
        let candidate = writer
            .prepare(staged.intent.clone())
            .map_err(|e| BuiltinModelError(format!("prepare index workspace: {e}")))?;
        lp.lap("worker.prepare_workspace");
        #[cfg(test)]
        self.observed.phase("open-candidate");
        let snapshot = writer
            .candidate_snapshot(&candidate)
            .map_err(|e| BuiltinModelError(format!("open index candidate: {e}")))?;
        // Retain the prepared candidate before any projection can unwind.
        self.candidate = Some(candidate);
        self.staged = Some(staged);
        self.candidate_snapshot = Some(snapshot.clone());
        let loader = self
            .semantic
            .as_ref()
            .expect("original semantic writer")
            .authority()
            .capture_image_loader()?;
        self.generations.install_selected_loader(loader);
        #[cfg(test)]
        self.observed.phase("prepare-view");
        let view = prepare_builtin_view(
            snapshot.clone(),
            self.current.clone(),
            &self.compiler,
            self.deployment,
            &self.workspace,
            self.prior.as_ref(),
            // A capture changes the companion capture root, which always
            // rehydrates; the planner needs no edit to know that.
            edit.as_ref(),
            &mut self.image_rows,
            &mut self.generations,
        )?;
        lp.lap("worker.prepare_view");
        let admission = BuiltinViewAdmission {
            workspace_root: snapshot.root(),
            source_root: view.view.basis().root,
        };
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
        lp.lap("worker.prepare_read_head");
        Ok(())
    }

    fn attempt(&mut self, events: &Sender<Event>) -> Result<(), FinishAddError> {
        let mut lp = crate::owner_trace::laps();
        self.prepare()?;
        lp.lap("worker.prepare_total");
        if self.nothing_to_commit() {
            return Ok(());
        }
        let capture = self.capture;
        // Only the final publication is announced by a durable Prepared marker;
        // a capture or a terminal closure is recoverable from its Pending markers.
        let announced = !capture && !self.closure;
        if !self.semantic_committed {
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
                        if let Some(fence) = fence.as_ref()
                            && !super::super::super::ingest::compiler_revision_is_current_with_cancellation(
                                fence, Some(cancellation.as_ref()),
                            ).map_err(BuiltinModelError)? {
                            return Err(BuiltinModelError("compiler source or configuration changed before product selection".to_owned()));
                        }
                        if announced {
                            #[cfg(test)]
                            observed.phase("publication-barrier");
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
                    select_head(head, candidate, events, grant_accepted,
                        #[cfg(test)] test_hook, #[cfg(test)] observed)
                }).map_err(|e| if busy { FinishAddError::PublicationBusy } else { e.into() })?;
            self.semantic_committed = true;
            #[cfg(test)]
            self.run_test_hook(TestStage::AfterSelection);
        }
        lp.lap("worker.select_head");
        if self.progress.is_none()
            && let Some(snapshot) = self.candidate_snapshot.as_ref()
        {
            // Best effort: without it the owner counts on its first status
            // request instead.
            self.progress = ProgressMemo::of(snapshot).ok();
        }
        lp.lap("worker.ingest_progress");
        if capture {
            // The capture's head is selected, so the owner must install it
            // whatever happens next; a later failure is the job's to refuse,
            // not a pending retry: nothing about it can heal by waiting.
            self.roots = Some(
                self.view
                    .as_ref()
                    .expect("prepared view")
                    .outcome
                    .roots
                    .clone(),
            );
            if let Err(error) = self.finish_published_capture() {
                self.next = Some(CaptureNext::Compile(Err(error)));
            }
            return Ok(());
        }
        let snapshot = self
            .candidate_snapshot
            .as_ref()
            .expect("exact candidate snapshot");
        let view = self.view.as_ref().expect("exact prepared view");
        if !self.projected {
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
        if self.terminal {
            self.outcome = self.decided.take();
            self.roots = Some(
                self.view
                    .as_ref()
                    .expect("prepared view")
                    .outcome
                    .roots
                    .clone(),
            );
            return Ok(());
        }
        let staged = self.staged.as_ref().expect("actual request");
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
            Ok(()) if self.nothing_to_commit() => return Disposition::Unchanged,
            Ok(()) => return Disposition::Ready,
            Err(FinishAddError::PublicationBusy) => {
                return Disposition::Pending(BuiltinModelError(
                    "durable publication is temporarily busy".to_owned(),
                ));
            }
            Err(FinishAddError::Failed(e)) => e,
        };
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
        let outcome = self.outcome.clone().unwrap_or_else(|| {
            if self.cancellation_won() {
                backend_library::IndexJobOutcome::Cancelled
            } else {
                backend_library::IndexJobOutcome::Failed(bounded_index_detail(error.clone()))
            }
        });
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
        !self.settlement_only && !self.grant_accepted && matches!(self.head, Head::Writer(_))
    }

    pub(super) fn cancellation_won(&self) -> bool {
        !self.grant_accepted && self.cancellation.load(Ordering::Acquire)
    }

    pub(super) fn take_unselected_writer(&mut self) -> Option<ReadHeadWriter<BuiltinModel>> {
        match std::mem::replace(&mut self.head, Head::Moving) {
            Head::Writer(writer) => Some(writer),
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
            #[cfg(test)]
            observed.phase("await-owner-grant");
            events.send(Event::Grant { claim, reply }).map_err(|_| {
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
        Head::Published(_) => unreachable!(),
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
        work: Box<PublicationWork>,
    ) -> Result<Self, (Box<PublicationWork>, std::io::Error)> {
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
                    let trace = crate::owner_trace::span("worker.publish_step");
                    let disposition = work.step(&events);
                    drop(trace);
                    if events
                        .send(Event::Complete(Completion { work, disposition }))
                        .is_err()
                    {
                        return;
                    }
                    match settlements.recv() {
                        Ok(Settlement::Retry(returned)) => work = returned,
                        Ok(Settlement::Retire(returned, retired)) => {
                            #[cfg(test)]
                            returned.observed.phase("retire-original-payloads");
                            drop((returned, retired));
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
