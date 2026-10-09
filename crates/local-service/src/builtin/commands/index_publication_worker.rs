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
    pub(super) test_hook: Option<TestHook>,
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TestStage {
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
    Writer(ReadHeadWriter<BuiltinModel>),
    Pending(ReadHeadPublicationFailure<BuiltinModel>),
    Published(PublishedReadHead<BuiltinModel>),
    Moving,
}

pub(super) enum Disposition {
    Ready,
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
            test_hook: None,
        }
    }

    fn prepare(&mut self) -> Result<(), BuiltinModelError> {
        if self.view.is_some() {
            return Ok(());
        }
        let original = self.prepared.intent.as_ref().ok_or_else(|| {
            BuiltinModelError("publication worker requires an actual source intent".to_owned())
        })?;
        let intent = match self.operation_key {
            Some(key) => original.clone().with_operation_key(key)?,
            None => original.clone(),
        };
        if !self.prepared.profile_refusals.is_empty() {
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
        let staged = prepare_builtin_intent_at_snapshot(
            &self.base,
            &intent,
            self.prepared
                .revision_fence
                .as_ref()
                .map(|f| f.canonical_root()),
            Arc::clone(&self.cancellation),
        )?;
        let Head::Writer(writer) = &mut self.head else {
            return Err(BuiltinModelError(
                "private publication was already selected".to_owned(),
            ));
        };
        let candidate = writer
            .prepare(staged.intent.clone())
            .map_err(|e| BuiltinModelError(format!("prepare index workspace: {e}")))?;
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
            self.semantic.as_mut().expect("original semantic writer").authority_mut()
                .commit_product_selection_changes(self.prepared.selected.clone(), removals, || {
                    if matches!(head, Head::Writer(_)) {
                        if let Some(fence) = fence.as_ref()
                            && !super::super::super::ingest::compiler_revision_is_current_with_cancellation(
                                fence, Some(cancellation.as_ref()),
                            ).map_err(BuiltinModelError)? {
                            return Err(BuiltinModelError("compiler source or configuration changed before product selection".to_owned()));
                        }
                        prepare_publication_barrier(journal, operation_key, Some(request),
                            base_root, base_sequence, plan).map_err(|e| match e {
                                FinishAddError::PublicationBusy => {
                                    busy = true;
                                    BuiltinModelError("durable publication is temporarily busy".to_owned())
                                }
                                FinishAddError::Failed(e) => e,
                            })?;
                    }
                    select_head(head, candidate, events, grant_accepted, #[cfg(test)] test_hook)
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
                    if events
                        .send(Event::Complete(Completion { work, disposition }))
                        .is_err()
                    {
                        return;
                    }
                    match settlements.recv() {
                        Ok(Settlement::Retry(returned)) => work = returned,
                        Ok(Settlement::Retire(returned, retired)) => {
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
