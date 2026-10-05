//! The single application state owner.
//!
//! `UiRootEntity` is deliberately boring: one runtime, one immutable
//! snapshot pointer, and typed intents. It renders nothing — the window root
//! is the [`Shell`](crate::shell::Shell), whose regions read the snapshot
//! through the [`DataStore`] mirror. An input queues an
//! [`Intent`](crate::navigation::Intent); it is reduced at the end of the
//! current effect cycle, and engine results arrive through the actor's wake
//! task, so nothing here ever needs a frame.

use super::coordinator::{DesktopRuntime, RequestOutcome, RuntimeEvent};
use super::reads::ReadPool;
use super::store::{DataStore, OwnerAttachment, RouteDependencies, RouteReadLease};
use crate::core::{FaultCode, IntentDispatcher, ProducerAuthority, SnapshotReadModel};
use crate::model::{AppSnapshot, ConnectionStatus, PersistentState};
use crate::navigation::{FolderPickerOutcome, Intent, Route, View};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, PathPromptOptions, Task};
use std::collections::{BTreeMap, BTreeSet};
use super::persistence_writer::{PersistenceWriter, WriteRevision, WriteFailure, WriteAck, WriteReceiver};
use std::path::PathBuf;
use std::sync::Arc;

/// A native view request carries the graph visit it belongs to. The map
/// resolves its visible selection rather than the route's original symbol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphDestination { Page, Code }
impl GraphDestination {
    pub(crate) const fn view(self) -> View { match self { Self::Page => View::Page, Self::Code => View::Code } }
}

#[derive(Default)]
struct ConnectionProbeLatch {
    active: Option<ActiveConnectionProbe>,
}

struct ActiveConnectionProbe {
    request: crate::navigation::RequestId,
    previous: ConnectionStatus,
    basis: crate::core::VersionedRoot,
    attachment: OwnerAttachment,
}

impl ConnectionProbeLatch {
    fn begin(
        &mut self,
        request: crate::navigation::RequestId,
        previous: ConnectionStatus,
        basis: crate::core::VersionedRoot,
        attachment: OwnerAttachment,
    ) -> bool {
        if self.active.is_some() {
            return false;
        }
        self.active = Some(ActiveConnectionProbe { request, previous, basis, attachment });
        true
    }

    fn finish(
        &mut self,
        request: crate::navigation::RequestId,
        outcome: RequestOutcome,
        current_basis: crate::core::VersionedRoot,
        current_attachment: Option<&OwnerAttachment>,
    ) -> Option<Intent> {
        let active = self.active.as_ref()?;
        if active.request != request {
            return None;
        }
        let previous = active.previous;
        let same_basis = active.basis.same_authority(current_basis);
        let same_attachment = current_attachment == Some(&active.attachment);
        self.active.take()?;
        if !same_attachment {
            // The former owner cannot lend even its previous Connected check
            // to a replacement or withdrawn attachment. The watcher will
            // publish Ready/Failed for the new generation independently.
            return Some(Intent::ConnectionProbeAborted { previous: ConnectionStatus::Unknown });
        }
        if !same_basis {
            return Some(Intent::ConnectionProbeAborted { previous });
        }
        Some(match outcome {
            RequestOutcome::Succeeded => Intent::ConnectionResult { result: ConnectionStatus::Connected },
            RequestOutcome::Failed(FaultCode::Transport) => Intent::ConnectionResult { result: ConnectionStatus::TransportFailed },
            RequestOutcome::Failed(FaultCode::Unsupported) => Intent::ConnectionResult { result: ConnectionStatus::CheckUnavailable },
            RequestOutcome::Failed(_) => Intent::ConnectionResult { result: ConnectionStatus::CheckFailed },
            RequestOutcome::Cancelled
            | RequestOutcome::Superseded
            | RequestOutcome::Refused(_) => Intent::ConnectionProbeAborted {
                previous,
            },
        })
    }

    fn is_active(&self) -> bool {
        self.active.is_some()
    }

    fn clear(&mut self) {
        self.active = None;
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GraphViewRequest {
    pub target: GraphDestination,
    pub route: Route,
    /// The selected node, including its typed indexed address when known.
    pub selection: super::graph_focus::GraphViewEligibility,
    /// Full observation metadata retained for diagnostics.
    pub root: crate::core::VersionedRoot,
    pub authority: ProducerAuthority,
    /// The serving owner captured with this graph visit. A later same-root
    /// attachment cannot turn an old native view callback into a new action.
    pub attachment: Option<OwnerAttachment>,
    pub sequence: u64,
}

enum QueuedIntent {
    Plain(Intent),
    Index { intent: Intent, attachment: super::actor::IndexMutationLease },
    IndexStatus { intent: Intent, attachment: OwnerAttachment },
    Read { intent: Intent, lease: RouteReadLease, sequence: u64 },
}

impl QueuedIntent {
    fn intent(&self) -> &Intent { match self { Self::Plain(intent) | Self::Index { intent, .. } | Self::IndexStatus { intent, .. } | Self::Read { intent, .. } => intent } }
}

enum IndexPreflightSave { Writing, Saved }

struct IndexPreflight {
    intent: Intent,
    attachment: super::actor::IndexMutationLease,
    save: IndexPreflightSave,
    revision: WriteRevision,
    _task: Option<Task<()>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexPreflightRefusal {
    ProjectRemoved,
    ProjectChanged,
    ClaimChanged,
    OwnerUnavailable,
    OwnerAttachmentChanged,
    ProducerStreamChanged,
    AuthorityRegressed,
}

impl IndexPreflightRefusal {
    fn message(self) -> &'static str {
        match self {
            Self::ProjectRemoved => "The folder was removed while its index request was being saved. Nothing was sent.",
            Self::ProjectChanged => "This folder's index state changed while its request was being saved. Nothing was sent.",
            Self::ClaimChanged => "This folder now belongs to a different index attempt. Nothing was sent for the saved request.",
            Self::OwnerUnavailable => "The local service is unavailable. Nothing was sent; retry when it is ready.",
            Self::OwnerAttachmentChanged => "The local service attachment changed while this request was being saved. Nothing was sent; retry against its current attachment.",
            Self::ProducerStreamChanged => "The index producer stream changed while this request was being saved. Nothing was sent; retry against the current index.",
            Self::AuthorityRegressed => "The index authority regressed or conflicted while this request was being saved. Nothing was sent.",
        }
    }
}

/// A saved mutation retains its caller operation across ordinary publications.
/// This does not authorize a read at the old root or cross an owner attachment.
fn index_preflight_basis(
    snapshot: &AppSnapshot,
    project: &crate::core::LocalProjectId,
    operation: &crate::model::IndexOperationClaim,
    basis: crate::core::VersionedRoot,
) -> Result<crate::core::VersionedRoot, IndexPreflightRefusal> {
    let row = snapshot.workspace().projects.iter().find(|row| row.id == *project)
        .ok_or(IndexPreflightRefusal::ProjectRemoved)?;
    if row.phase != crate::model::ProjectPhase::Indexing || row.request.is_some() {
        return Err(IndexPreflightRefusal::ProjectChanged);
    }
    if !operation.belongs_to(project) || row.operation.as_ref().is_some_and(|saved|
        !saved.same_request(operation) || saved.observation.is_some()) {
        return Err(IndexPreflightRefusal::ClaimChanged);
    }
    let current = snapshot.key();
    if current.same_authority(basis) { return Ok(current); }
    let (old, next) = (basis.revision(), current.revision());
    if current.producer_epoch() != basis.producer_epoch() || old.recipe() != next.recipe()
        || old.branch() != next.branch() || old.log() != next.log() || old.schema() != next.schema() {
        return Err(IndexPreflightRefusal::ProducerStreamChanged);
    }
    if next.sequence() <= old.sequence() {
        return Err(IndexPreflightRefusal::AuthorityRegressed);
    }
    Ok(current)
}

/// The complete UI-thread state owner for one desktop window.
pub struct UiRootEntity {
    runtime: DesktopRuntime,
    pending: Vec<QueuedIntent>,
    persistence: Option<PersistentState>,
    persistence_writer: Option<PersistenceWriter>,
    persistence_wake: Option<Task<()>>,
    persistence_outcome: Option<WriteRevision>,
    index_preflights: BTreeMap<crate::core::LocalProjectId, IndexPreflight>,
    quitting: bool,
    bootstrap: Option<(crate::host::bootstrap::Binding, Arc<AppSnapshot>)>,
    folder_picker_task: Option<Task<()>>,
    /// One bounded cadence task for accepted/active durable operation reads.
    index_poll: Option<Task<()>>,
    connection_probe: ConnectionProbeLatch,
    /// The data plane: snapshot mirror, keyed page resources, read pool.
    store: Option<Entity<DataStore>>,
    /// The one task that drains engine results when the actor wakes it.
    engine_wake: Option<Task<()>>,
    /// Whether a deferred flush of queued intents is already scheduled.
    flush_scheduled: bool,
    /// Last snapshot handed to the store.
    published: Option<Arc<AppSnapshot>>,
    /// Intents reduced, for tests and diagnostics.
    reduced: u64,
    /// Invalidates graph view intents only for competing content requests.
    graph_view_generation: u64,
}

impl EventEmitter<GraphViewRequest> for UiRootEntity {}

impl UiRootEntity {
    /// Installs one root around an already admitted runtime.
    #[must_use]
    pub fn new(mut runtime: DesktopRuntime, persistence: Option<PersistentState>) -> Self {
        let basis = runtime.snapshot().key();
        let request = runtime.allocate_request();
        Self {
            runtime,
            // Startup is itself a typed intent.  The root is admitted before
            // the first useful frame so a cold window never renders a fake
            // catalog or a second, view-owned bootstrap path.
            pending: vec![QueuedIntent::Plain(Intent::RefreshRoot { basis, request })],
            persistence,
            persistence_writer: None,
            persistence_wake: None,
            persistence_outcome: None,
            index_preflights: BTreeMap::new(),
            quitting: false,
            bootstrap: None,
            folder_picker_task: None,
            index_poll: None,
            connection_probe: ConnectionProbeLatch::default(),
            store: None,
            engine_wake: None,
            flush_scheduled: false,
            published: None,
            reduced: 0,
            graph_view_generation: 0,
        }
    }

    /// Attaches the data-plane store and starts the engine wake task.
    ///
    /// After this, engine results reach the UI only through the actor's wake
    /// signal: one `cx.spawn` task awaits it, drains every queued result, and
    /// notifies. Nothing polls per frame and an idle window with a
    /// long-running request schedules no frame.
    pub fn attach(&mut self, store: Option<Entity<DataStore>>, cx: &mut Context<Self>) {
        self.store = store;
        if self.engine_wake.is_none()
            && let Some(mut receiver) = self.runtime.take_wake()
        {
            self.engine_wake = Some(cx.spawn(async move |this, cx| {
                while receiver.wait().await.is_some() {
                    if this.update(cx, Self::drain_engine).is_err() {
                        break;
                    }
                }
            }));
        }
        self.publish_snapshot(cx);
        if !self.pending.is_empty() {
            self.schedule_flush(cx);
        }
    }

    /// Returns the data-plane store, when attached.
    #[must_use]
    pub const fn store(&self) -> Option<&Entity<DataStore>> {
        self.store.as_ref()
    }

    /// Returns how many intents this root has reduced.
    #[must_use]
    pub const fn reduced(&self) -> u64 {
        self.reduced
    }

    pub(crate) const fn graph_view_generation(&self) -> u64 { self.graph_view_generation }

    /// Returns whether engine work is in flight or waiting to be drained.
    #[must_use]
    pub fn has_pending_work(&self) -> bool {
        self.runtime.has_pending_work() || !self.index_preflights.is_empty()
    }

    /// [`Self::has_pending_work`] apart from indexing (journey harness).
    #[must_use]
    pub fn has_pending_work_besides_indexing(&self) -> bool {
        self.runtime.has_pending_work_besides_indexing()
    }

    /// Takes every engine result the actor has delivered now, without
    /// waiting for its wake task (a harness that holds one input instant
    /// while the owner works).
    #[cfg(feature = "visual-harness")]
    pub(crate) fn drain_now(&mut self, cx: &mut Context<Self>) {
        self.drain_engine(cx);
    }

    /// Drains every engine result the actor has delivered.
    fn drain_engine(&mut self, cx: &mut Context<Self>) {
        let events = self.runtime.poll();
        if !events.is_empty() {
            self.apply_events(events, cx);
        }
    }

    /// Runs queued intents at the end of the current effect cycle, so an
    /// intent queued during a render never needs a follow-up frame request.
    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush_scheduled {
            return;
        }
        self.flush_scheduled = true;
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = this.update(cx, Self::flush_pending);
        });
    }

    fn flush_pending(&mut self, cx: &mut Context<Self>) {
        self.flush_scheduled = false;
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        for queued in pending {
            match queued {
                QueuedIntent::Plain(intent) => self.dispatch(intent, cx),
                QueuedIntent::Index { intent, attachment }
                    if matches!(&intent, Intent::IndexProject { project, operation, basis, .. }
                        if index_preflight_basis(&self.snapshot(), project, operation, *basis).is_ok()
                            && matches!(attachment.admission(*basis), super::owner::MutationAdmission::Ready(_) | super::owner::MutationAdmission::Observing)) => {
                        self.dispatch_index(intent, attachment, cx);
                    }
                QueuedIntent::IndexStatus { intent, attachment }
                    if self.store.as_ref().is_some_and(|store| store.read(cx).admits_owner_attachment(&attachment))
                        && matches!(&intent, Intent::ReconcileIndexProject { project, operation, basis, .. }
                            if self.snapshot().key().same_authority(*basis) && self.snapshot().workspace().projects.iter().any(|row|
                                row.id == *project && row.request.is_none() && row.operation.as_ref() == Some(operation))) => self.dispatch_runtime(intent, cx),
                QueuedIntent::IndexStatus { .. } => {},
                // An unsent local admission remains on the shelf. A replacement
                // owner will schedule it against its own attachment and root.
                QueuedIntent::Index { .. } => {}
                QueuedIntent::Read { intent, lease, sequence } if sequence == self.graph_view_generation
                    && self.store.as_ref().is_some_and(|store| lease.admits(store.read(cx))) => self.dispatch(intent, cx),
                QueuedIntent::Read { .. } => {}
            }
        }
    }

    /// Close the durable queued/submitted boundary before crossing the actor.
    /// A crash after this save is conservatively Unconfirmed on restart; a
    /// failed save leaves the folder unsent. Persistence already belongs to
    /// this root, and the exact owner lease is checked again after publication.
    fn dispatch_index(&mut self, intent: Intent, attachment: super::actor::IndexMutationLease, cx: &mut Context<Self>) {
        let Intent::IndexProject { project, .. } = &intent else { return; };
        let project = project.clone();
        if self.quitting || self.index_preflights.contains_key(&project) { return; }
        if self.index_preflights.len() >= super::persistence_writer::MAX_INDEX_PREFLIGHTS {
            self.reject_index_preflight(&intent, "Several index requests are already waiting to be saved. Nothing was sent for this folder; try again after they settle.".into(), cx);
            return;
        }
        if let Err(error) = self.ensure_persistence_writer(cx) {
            self.reject_index_preflight(&intent, error.message, cx);
            return;
        }
        let submitted = crate::navigation::reduce(&self.snapshot(), intent.clone()).snapshot;
        let claims = self.pending_claims(&submitted);
        let writer = self.persistence_writer.as_ref().expect("writer admitted");
        let saved = writer.barrier(move || overlay_claims(PersistentState::project(&submitted), claims));
        let (revision, acknowledgment) = match saved {
            Ok(saved) => saved,
            Err(error) => { self.reject_index_preflight(&intent, error.message, cx); return; }
        };
        self.index_preflights.insert(project.clone(), IndexPreflight { intent, attachment, save: IndexPreflightSave::Writing, revision, _task: None });
        let completed_project = project.clone();
        let task = cx.spawn(async move |root, cx| {
            let saved = acknowledgment.recv().await.unwrap_or_else(|_| Err(WriteFailure { message: "The local state writer closed before confirming this index request. Nothing was sent.".into() }));
            let _ = root.update(cx, |root, cx| root.complete_index_preflight(completed_project, revision, saved, cx));
        });
        if let Some(preflight) = self.index_preflights.get_mut(&project) { preflight._task = Some(task); }
    }

    fn complete_index_preflight(&mut self, project: crate::core::LocalProjectId, revision: WriteRevision, result: Result<WriteAck, WriteFailure>, cx: &mut Context<Self>) {
        if self.quitting || !self.index_preflights.get(&project).is_some_and(|pending| pending.revision == revision
            && matches!(pending.save, IndexPreflightSave::Writing))
            || !self.persistence_writer.as_ref().is_some_and(|writer| writer.accepts(revision)) { return; }
        let Some(pending) = self.index_preflights.get(&project) else { return; };
        let Intent::IndexProject { operation, .. } = &pending.intent else { return; };
        let result = result.and_then(|ack| {
            if ack.revision != revision || !ack.state.shelf.iter().any(|row| row.local_path == project.as_str()
                && row.phase == crate::model::PersistedProjectPhase::Indexing
                && row.native_path.as_ref() == project.native_wire().ok().as_ref()
                && row.operation.as_ref().is_some_and(|saved| saved == operation && saved.belongs_to(&project))) {
                return Err(WriteFailure { message: "The saved state did not confirm this exact index operation. Nothing was sent.".into() });
            }
            Ok(()) // retain the validated claim state, never the full acknowledgment
        });
        self.record_persistence_outcome(revision, result.clone(), cx);
        match result {
            Ok(()) => {
                if let Some(pending) = self.index_preflights.get_mut(&project) {
                    pending.save = IndexPreflightSave::Saved;
                    pending._task = None;
                }
                self.resume_saved_index(project, cx);
            }
            Err(error) => {
                if let Some(pending) = self.index_preflights.remove(&project) {
                    self.reject_index_preflight(&pending.intent, format!("This index request could not be saved: {}", error.message).into(), cx);
                }
            }
        }
    }

    fn resume_saved_indexes(&mut self, cx: &mut Context<Self>) {
        let saved = self.index_preflights.iter().filter(|(_, pending)| matches!(pending.save, IndexPreflightSave::Saved))
            .map(|(project, _)| project.clone()).collect::<Vec<_>>();
        for project in saved { self.resume_saved_index(project, cx); }
    }

    fn resume_saved_index(&mut self, project: crate::core::LocalProjectId, cx: &mut Context<Self>) {
        if self.quitting { return; }
        let Some(pending) = self.index_preflights.get(&project) else { return; };
        if !matches!(pending.save, IndexPreflightSave::Saved) { return; }
        let Intent::IndexProject { basis, operation, request, .. } = &pending.intent else { return; };
        let live = index_preflight_basis(&self.snapshot(), &project, operation, *basis);
        let admitted = live.and_then(|current| match pending.attachment.admission(*basis) {
            super::owner::MutationAdmission::Ready(certified) if certified.same_authority(current) => Ok(Some(current)),
            super::owner::MutationAdmission::Ready(_) | super::owner::MutationAdmission::Observing => Ok(None),
            super::owner::MutationAdmission::Replaced => Err(IndexPreflightRefusal::OwnerAttachmentChanged),
            super::owner::MutationAdmission::Unavailable(_) => Err(IndexPreflightRefusal::OwnerUnavailable),
        });
        match admitted {
            Ok(None) => {
                super::trace::mark("index.preflight.waiting", format_args!("saved request {request:?}; waiting for certified owner observation"));
            }
            Ok(Some(current)) => {
                if !current.same_authority(*basis) {
                    super::trace::mark("index.preflight.rebased", format_args!("captured {basis}; current {current}; request {request:?}"));
                }
                let intent = Intent::IndexProject { project: project.clone(), operation: operation.clone(), basis: current, request: *request };
                let owner = pending.attachment.clone();
                self.index_preflights.remove(&project);
                let events = self.runtime.dispatch_saved_index(intent, owner);
                self.apply_events(events, cx);
            }
            Err(reason) => {
                super::trace::mark("index.preflight.refused", format_args!("{reason:?}; captured {basis}; current {}; request {request:?}", self.snapshot().key()));
                let intent = pending.intent.clone();
                self.index_preflights.remove(&project);
                self.reject_index_preflight(&intent, reason.message().into(), cx);
            }
        }
    }

    fn reject_index_preflight(&mut self, intent: &Intent, message: Arc<str>, cx: &mut Context<Self>) {
        if let Intent::IndexProject { project, operation, .. } = intent {
            let events = self.runtime.reject_unsent_index(project, self.snapshot().key(), Some(operation), message);
            self.apply_events(events, cx);
        }
    }

    fn pending_claims(&self, snapshot: &AppSnapshot) -> Vec<(crate::core::LocalProjectId, crate::model::IndexOperationClaim)> {
        self.index_preflights.values().filter_map(|pending| {
            let Intent::IndexProject { project, operation, .. } = &pending.intent else { return None; };
            snapshot.workspace().projects.iter().any(|row| row.id == *project && row.phase == crate::model::ProjectPhase::Indexing
                && row.request.is_none() && row.operation.is_none()).then(|| (project.clone(), operation.clone()))
        }).collect()
    }

    fn ensure_persistence_writer(&mut self, cx: &mut Context<Self>) -> Result<(), WriteFailure> {
        let persistence = self.persistence.as_ref().ok_or_else(|| WriteFailure { message: "The index request cannot start because its durable operation key cannot be saved. The folder remains on your shelf.".into() })?;
        if let Some(writer) = &self.persistence_writer {
            if writer.path() != persistence.path() { return Err(WriteFailure { message: "The local state path changed while this window was open. Nothing was sent.".into() }); }
        } else { self.persistence_writer = Some(PersistenceWriter::start(persistence.clone())?); }
        self.attach_persistence_wake(cx);
        Ok(())
    }

    fn attach_persistence_wake(&mut self, cx: &mut Context<Self>) {
        if self.persistence_wake.is_some() { return; }
        let Some(mut wake) = self.persistence_writer.as_mut().and_then(PersistenceWriter::take_wake) else { return; };
        self.persistence_wake = Some(cx.spawn(async move |root, cx| {
            while wake.wait().await.is_some() {
                if root.update(cx, |root, cx| {
                    if let Some((revision, result)) = root.persistence_writer.as_ref().and_then(PersistenceWriter::take_outcome) {
                        root.record_persistence_outcome(revision, result, cx);
                    }
                }).is_err() { break; }
            }
        }));
    }

    fn persist_snapshot(&mut self, snapshot: &AppSnapshot, cx: &mut Context<Self>) {
        if self.persistence.is_none() || self.quitting { return; }
        if let Err(error) = self.ensure_persistence_writer(cx) {
            self.runtime.set_persistence_fault(Some(error.message));
            self.publish_snapshot(cx);
            return;
        }
        let state = overlay_claims(PersistentState::project(snapshot), self.pending_claims(snapshot));
        if let Err(error) = self.persistence_writer.as_ref().expect("writer admitted").ordinary(state) {
            self.runtime.set_persistence_fault(Some(error.message));
            self.publish_snapshot(cx);
        }
    }

    fn record_persistence_outcome(&mut self, revision: WriteRevision, result: Result<(), WriteFailure>, cx: &mut Context<Self>) {
        if !self.persistence_writer.as_ref().is_some_and(|writer| writer.accepts(revision))
            || self.persistence_outcome.is_some_and(|last| last > revision) { return; }
        self.persistence_outcome = Some(revision);
        self.runtime.set_persistence_fault(result.err().map(|error| error.message));
        self.publish_snapshot(cx);
    }

    fn finish_persistence(&mut self, cx: &mut Context<Self>) -> Option<WriteReceiver> {
        self.quitting = true;
        self.pending.clear();
        self.index_poll = None;
        let events = self.runtime.poll();
        self.apply_events(events, cx);
        if self.persistence.is_none() { return None; }
        if let Err(error) = self.ensure_persistence_writer(cx) {
            self.runtime.set_persistence_fault(Some(error.message));
            return None;
        }
        let state = overlay_claims(PersistentState::project(&self.snapshot()), self.pending_claims(&self.snapshot()));
        let writer = self.persistence_writer.as_ref().expect("writer admitted");
        let _ = writer.ordinary(state);
        Some(writer.finish())
    }

    /// Hands the current snapshot to the store, which emits one typed event
    /// per branch that actually changed.
    fn publish_snapshot(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.runtime.snapshot();
        if self
            .published
            .as_ref()
            .is_some_and(|published| Arc::ptr_eq(published, &snapshot))
        {
            return;
        }
        if self.published.as_ref().is_some_and(|previous| previous.route() != snapshot.route()
            || !previous.key().same_authority(snapshot.key()) || previous.overlay() != snapshot.overlay()) {
            self.graph_view_generation = self.graph_view_generation.wrapping_add(1);
        }
        self.published = Some(Arc::clone(&snapshot));
        if let Some(store) = &self.store {
            store.update(cx, |store, cx| store.admit_snapshot(snapshot, cx));
        }
    }

    /// Returns the one immutable read model used by every visual surface.
    #[must_use]
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.runtime.snapshot()
    }

    /// Queues a typed intent; it is reduced at the end of the current effect
    /// cycle, so a burst of intents from one input is one reduction pass.
    pub fn queue(&mut self, intent: Intent, cx: &mut Context<Self>) {
        if let Intent::ResolveCargoBrowse { expected, context } = &intent {
            let Some(dependency) = self.cargo_resolution_dependency(expected, context, cx) else { return; };
            self.queue_read(intent, dependency, cx);
            return;
        }
        self.pending.push(QueuedIntent::Plain(intent));
        self.schedule_flush(cx);
    }

    /// A selected current resource must remain admitted when this intent is
    /// reduced. A competing visit invalidates the existing UI generation.
    pub(crate) fn queue_read(&mut self, intent: Intent, dependency: (crate::model::pages::PageKey, crate::model::pages::Stamp), cx: &mut Context<Self>) {
        let Some(store) = &self.store else { return; };
        let store = store.read(cx);
        let snapshot = self.snapshot();
        if snapshot.route() != store.snapshot().route() || snapshot.overlay() != store.snapshot().overlay()
            || !snapshot.key().same_authority(store.snapshot().key()) { return; }
        let Some(lease) = RouteReadLease::capture(store, dependency) else { return; };
        self.pending.push(QueuedIntent::Read { intent, lease, sequence: self.graph_view_generation });
        self.schedule_flush(cx);
    }

    fn cargo_resolution_dependency(&self, expected: &crate::navigation::CargoSourceRoute, context: &crate::navigation::CargoBrowseContext, cx: &App) -> Option<(crate::model::pages::PageKey, crate::model::pages::Stamp)> {
        let snapshot = self.snapshot();
        if snapshot.route() != &Route::CargoSource(expected.clone()) || expected.resolve_context(context.clone()).is_none() { return None; }
        let store = self.store.as_ref()?.read(cx);
        if store.snapshot().route() != snapshot.route() || store.snapshot().overlay() != snapshot.overlay()
            || !store.snapshot().key().same_authority(snapshot.key()) { return None; }
        let package = crate::model::pages::PackageRef::parse(expected.package.as_str()).ok()?;
        let receipt = RouteDependencies::new(snapshot.route(), snapshot.overlay()).current_cargo_package(store, &package)?;
        (receipt.context() == context).then(|| receipt.native_dependency())
    }

    /// Applies a typed intent immediately from a harness or startup phase.
    pub fn dispatch(&mut self, intent: Intent, cx: &mut Context<Self>) {
        if matches!(&intent, Intent::IndexProject { .. }) {
            if let Some(attachment) = self.store.as_ref().and_then(|store| store.read(cx).current_index_owner()) {
                self.dispatch_index(intent, attachment, cx);
            }
            return;
        }
        if let Intent::ResolveCargoBrowse { expected, context } = &intent
            && self.cargo_resolution_dependency(expected, context, cx).is_none() { return; }
        match &intent {
            Intent::RemoveProject(project) | Intent::CancelIndex(project) | Intent::RetryIndex(project) => {
                if let Some(pending) = self.index_preflights.remove(project) {
                    self.runtime.release_unsent_claim(&pending.intent);
                }
            }
            _ => {}
        }
        self.reduced = self.reduced.saturating_add(1);
        match intent {
            Intent::SetView(view @ (View::Page | View::Code))
                if self.snapshot().overlay().is_none()
                    && matches!(self.snapshot().route(), Route::World | Route::Symbol(crate::navigation::SymbolRoute { view: View::Graph, .. })) => {
                let snapshot = self.snapshot();
                self.graph_view_generation = self.graph_view_generation.wrapping_add(1);
                if let Some(store) = &self.store {
                    let (selection, attachment) = store.read_with(cx, |store, _| {
                        (store.graph_view_eligibility(), store.current_owner_attachment())
                    });
                    if selection.can_open() {
                        cx.emit(GraphViewRequest {
                            target: if view == View::Page { GraphDestination::Page } else { GraphDestination::Code },
                            route: snapshot.route().clone(),
                            selection,
                            root: snapshot.key(),
                            authority: snapshot.key().authority(),
                            attachment,
                            sequence: self.graph_view_generation,
                        });
                    } else if let Some(message) = selection.guidance() {
                        let notice = super::graph_focus::Notice {
                            visit: snapshot.route().clone(),
                            root: snapshot.key(),
                            message: message.into(),
                            retry: None,
                        };
                        store.update(cx, |store, cx| store.set_notice(Some(notice), cx));
                    }
                }
            }
            // The world, then the ask the graph flies once it shows.
            Intent::Tour(package) => {
                self.dispatch_runtime(Intent::Tour(package.clone()), cx);
                if let Some(store) = &self.store
                    && let Ok(package) = crate::model::pages::PackageRef::parse(package.as_str())
                {
                    store.update(cx, |store, cx| store.ask_tour(package, cx));
                }
            }
            Intent::OpenFolderPicker => self.start_folder_picker(cx),
            Intent::RevealProject(project) => cx.reveal_path(&project.path()),
            Intent::OpenSource { path, line } => {
                let launch = crate::host::editor::launcher(cx);
                let visit = self.snapshot().route().clone();
                let root = self.snapshot().key();
                let outcome = crate::host::editor::open(launch.as_ref(), None, &path, line);
                if let Some(store) = &self.store {
                    let message = outcome.message(&path, line);
                    store.update(cx, |store, cx| store.set_notice(Some(super::graph_focus::Notice {
                        visit, root, message: message.into(), retry: None,
                    }), cx));
                }
            }
            Intent::TestConnection => {
                if !self.connection_probe.is_active() {
                    let Some(attachment) = self.store.as_ref()
                        .and_then(|store| store.read(cx).current_owner_attachment()) else {
                        return;
                    };
                    let request = self.runtime.allocate_request();
                    let previous = self.snapshot().settings().connection;
                    let basis = self.snapshot().key();
                    if self.connection_probe.begin(request, previous, basis, attachment) {
                        self.dispatch_runtime(Intent::TestConnection, cx);
                        self.dispatch_runtime(
                            Intent::CheckConnection { basis, request },
                            cx,
                        );
                    }
                }
            }
            Intent::FolderPickerResult { outcome } => {
                let selected = match &outcome {
                    FolderPickerOutcome::Selected(paths) => Some(Arc::clone(paths)),
                    FolderPickerOutcome::Cancelled
                    | FolderPickerOutcome::Unavailable(_)
                    | FolderPickerOutcome::Failed(_) => None,
                };
                self.dispatch_runtime(Intent::FolderPickerResult { outcome }, cx);
                if let Some(paths) = selected {
                    for path in paths.iter() {
                        if let Ok(project) = crate::core::LocalProjectId::from_path(path) {
                            self.schedule_index(project, cx);
                        }
                    }
                }
            }
            Intent::AddProject { project } => {
                self.dispatch_runtime(
                    Intent::AddProject {
                        project: project.clone(),
                    },
                    cx,
                );
                self.schedule_index(project, cx);
                self.schedule_pending_indexes(cx);
            }
            Intent::ActivateProject(project) => {
                self.dispatch_runtime(Intent::ActivateProject(project), cx);
                self.schedule_pending_indexes(cx);
            }
            Intent::RetryIndex(project) => {
                self.dispatch_runtime(Intent::RetryIndex(project), cx);
                self.schedule_pending_indexes(cx);
            }
            Intent::CheckIndexOutcome(project) => self.schedule_index_check(project, cx),
            Intent::AddRelease(release) => super::acquire::add(release, cx.weak_entity(), cx),
            other => self.dispatch_runtime(other, cx),
        }
    }

    /// The owner answered (W-Open I1): its root replaces the unserved one,
    /// and the root read (project, catalog) is asked again at it — the one
    /// asked at startup waited behind the owner at the unserved basis.
    pub(crate) fn admit_owner(
        &mut self,
        key: crate::core::VersionedRoot,
        mode: crate::model::ServiceMode,
        cx: &mut Context<Self>,
    ) {
        if let Some((binding, origin)) = self.bootstrap.take() {
            if let Some(bound) = binding.get() {
                self.runtime.admit_bootstrap(bound, &origin);
                self.persistence = bound.persistence.clone();
                // Preserve local decisions through the same ordered writer.
                self.persist_snapshot(&self.snapshot(), cx);
            } else { self.bootstrap = Some((binding, origin)); }
        }
        self.connection_probe.clear();
        self.dispatch_runtime(Intent::OwnerReady { key, mode }, cx);
        let request = self.runtime.allocate_request();
        self.dispatch_runtime(Intent::RefreshRoot { basis: key, request }, cx);
        // An index an earlier build wrote was set aside while the owner
        // started: say so, and index the shelf's projects again.
        if let Some(moved) = crate::host::aside::take() {
            self.dispatch(Intent::LibraryRebuilding { kept_at: Arc::from(moved.display().to_string()) }, cx);
        }
        // One fresh owner observation may reconcile previously Unknown or
        // Unresolved durable evidence. Their same-owner idle state does not poll.
        let uncertain = self.snapshot().workspace().projects.iter().filter(|row|
            row.phase == crate::model::ProjectPhase::Unconfirmed && row.operation.is_some() && row.request.is_none())
            .map(|row| row.id.clone()).collect::<Vec<_>>();
        for project in uncertain { self.schedule_index_check(project, cx); }
        self.schedule_operation_observation(cx);
        // Packages an earlier launch was still adding are added now.
        super::acquire::resume(&self.snapshot(), cx.weak_entity(), cx);
        self.resume_indexes_after_owner(cx);
    }

    /// A certified publication for the existing attachment. This advances
    /// the ordinary current-root path without replaying startup/acquisition.
    pub(crate) fn renew_owner(
        &mut self,
        key: crate::core::VersionedRoot,
        mode: crate::model::ServiceMode,
        attachment_changed: bool,
        cx: &mut Context<Self>,
    ) {
        if !attachment_changed
            && self.snapshot().key().same_authority(key)
            && self.snapshot().settings().service_mode == mode
            && self.snapshot().settings().confirmed_service_mode == Some(mode)
        {
            return;
        }
        self.connection_probe.clear();
        self.dispatch_runtime(Intent::OwnerReady { key, mode }, cx);
        self.refresh_root(cx);
        self.resume_indexes_after_owner(cx);
    }

    /// Reads the owner's root again: something outside the project lane
    /// changed what it serves (a release added to the library).
    pub(crate) fn refresh_root(&mut self, cx: &mut Context<Self>) {
        let request = self.runtime.allocate_request();
        self.dispatch_runtime(Intent::RefreshRoot { basis: self.snapshot().key(), request }, cx);
    }

    /// A publication changes package observations independently of the root
    /// read's catalog. Refresh open resources in place; retain route/history.
    pub(crate) fn packages_published(
        &mut self,
        packages: &BTreeSet<crate::model::pages::PackageRef>,
        cx: &mut Context<Self>,
    ) {
        if packages.is_empty() { return; }
        if let Some(store) = &self.store {
            store.update(cx, |store, cx| store.packages_published(packages, cx));
        }
        self.refresh_root(cx);
    }

    /// The owner watcher alone withdraws a previously confirmed generation.
    pub(crate) fn owner_starting(&mut self, cx: &mut Context<Self>) {
        self.connection_probe.clear();
        self.dispatch_runtime(Intent::OwnerStarting, cx);
        self.resume_saved_indexes(cx);
    }

    pub(crate) fn owner_unavailable(&mut self, cx: &mut Context<Self>) {
        self.connection_probe.clear();
        self.dispatch_runtime(Intent::OwnerUnavailable, cx);
        self.resume_saved_indexes(cx);
    }

    fn dispatch_runtime(&mut self, intent: Intent, cx: &mut Context<Self>) {
        let events = self.runtime.dispatch(intent);
        self.apply_events(events, cx);
    }

    fn start_folder_picker(&mut self, cx: &mut Context<Self>) {
        if self.folder_picker_task.is_some() {
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Choose local project folders".into()),
        });
        let task = cx.spawn(async move |weak: gpui::WeakEntity<Self>, cx| {
            let outcome = match receiver.await {
                Ok(Ok(Some(paths))) => folder_picker_outcome(paths),
                Ok(Ok(None)) => FolderPickerOutcome::Cancelled,
                Err(_) => FolderPickerOutcome::Failed(Arc::from(
                    "The native folder picker closed without returning a result.",
                )),
                Ok(Err(error)) => FolderPickerOutcome::Unavailable(bound_picker_error(error)),
            };
            let _ = weak.update(cx, |this, cx| {
                this.folder_picker_task = None;
                this.queue(Intent::FolderPickerResult { outcome }, cx);
            });
        });
        self.folder_picker_task = Some(task);
    }

    /// Answers the native folder panel this root opened, as a person would
    /// (`Some(folders)` chosen, `None` cancelled), when nothing can click it:
    /// the journey harness's one allowed substitution. The answer takes the
    /// task's own path (`folder_picker_outcome`, then the same queued intent).
    /// `false`: no panel is open.
    pub(crate) fn answer_folder_picker(&mut self, chosen: Option<Vec<PathBuf>>, cx: &mut Context<Self>) -> bool {
        if self.folder_picker_task.take().is_none() {
            return false;
        }
        let outcome = chosen.map_or(FolderPickerOutcome::Cancelled, folder_picker_outcome);
        self.queue(Intent::FolderPickerResult { outcome }, cx);
        true
    }

    /// The owner watcher admits its root before renewing the store. Defer
    /// scheduling until both name the same live attachment; unsent projects
    /// stay local while the service starts or is unavailable.
    fn resume_indexes_after_owner(&self, cx: &mut Context<Self>) {
        let root = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = root.update(cx, |root, cx| { root.resume_saved_indexes(cx); root.schedule_pending_indexes(cx); });
        });
    }

    fn schedule_pending_indexes(&mut self, cx: &mut Context<Self>) {
        let queued = self.pending.iter().filter(|intent| matches!(intent, QueuedIntent::Index { .. })).count();
        let available = super::persistence_writer::MAX_INDEX_PREFLIGHTS.saturating_sub(self.index_preflights.len().saturating_add(queued));
        let projects = self
            .snapshot()
            .workspace()
            .projects
            .iter()
            .filter(|project| {
                project.phase == crate::model::ProjectPhase::Indexing
                    && project.request.is_none()
                    && project.operation.is_none()
                    && !self.index_intent_pending(&project.id)
                    && !self.index_preflights.contains_key(&project.id)
            })
            .take(available)
            .map(|project| project.id.clone())
            .collect::<Vec<_>>();
        for project in projects {
            self.schedule_index(project, cx);
        }
    }

    fn schedule_index(&mut self, project: crate::core::LocalProjectId, cx: &mut Context<Self>) {
        if self.quitting || self.index_preflights.contains_key(&project) || self.index_intent_pending(&project)
            || !self.snapshot().workspace().projects.iter().any(|item| item.id == project && item.phase == crate::model::ProjectPhase::Indexing && item.request.is_none() && item.operation.is_none())
        {
            return;
        }
        let Some(store) = &self.store else { return; };
        let store = store.read(cx);
        let Some(attachment) = store.current_index_owner() else { return; };
        let basis = self.snapshot().key();
        if !store.snapshot().key().same_authority(basis) { return; }
        let operation = match crate::model::IndexOperationClaim::fresh(&project) {
            Ok(operation) => operation,
            Err(message) => {
                let events = self.runtime.reject_unsent_index(&project, basis, None, message.into());
                self.apply_events(events, cx);
                return;
            }
        };
        let request = self.runtime.allocate_request();
        self.pending.push(QueuedIntent::Index {
            intent: Intent::IndexProject { project, operation, basis, request },
            attachment,
        });
        self.schedule_flush(cx);
    }

    fn schedule_index_check(&mut self, project: crate::core::LocalProjectId, cx: &mut Context<Self>) {
        if self.quitting || self.index_preflights.contains_key(&project) { return; }
        if self.pending.iter().any(|queued| matches!(queued.intent(), Intent::ReconcileIndexProject { project: candidate, .. } if candidate == &project)) { return; }
        let snapshot = self.snapshot();
        let Some(row) = snapshot.workspace().projects.iter().find(|row| row.id == project && row.request.is_none()) else { return; };
        let Some(operation) = row.operation.as_ref().filter(|operation| operation.belongs_to(&project)).cloned() else { return; };
        let Some(store) = self.store.as_ref() else { return; };
        let Some(attachment) = store.read(cx).current_owner_attachment() else { return; };
        let request = self.runtime.allocate_request();
        self.pending.push(QueuedIntent::IndexStatus {
            intent: Intent::ReconcileIndexProject { project, operation, basis: snapshot.key(), request }, attachment,
        });
        self.schedule_flush(cx);
    }

    fn schedule_operation_observation(&mut self, cx: &mut Context<Self>) {
        if self.quitting || self.index_poll.is_some() || !self.store.as_ref().is_some_and(|store| store.read(cx).owner_serving()) { return; }
        if !self.snapshot().workspace().projects.iter().any(|row| row.request.is_none()
            && row.operation.as_ref().is_some_and(crate::model::IndexOperationClaim::needs_observation)) { return; }
        self.index_poll = Some(cx.spawn(async move |root, cx| {
            // A cadence bounds read traffic; elapsed time never determines an
            // operation's phase, receipt, failure or retry permission.
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            let _ = root.update(cx, |root, cx| {
                root.index_poll = None;
                let projects = root.snapshot().workspace().projects.iter().filter(|row| row.request.is_none()
                    && row.operation.as_ref().is_some_and(crate::model::IndexOperationClaim::needs_observation))
                    .map(|row| row.id.clone()).collect::<Vec<_>>();
                for project in projects { root.schedule_index_check(project, cx); }
            });
        }));
    }

    fn index_intent_pending(&self, project: &crate::core::LocalProjectId) -> bool {
        self.pending.iter().any(|queued| {
            matches!(queued.intent(), Intent::IndexProject { project: candidate, .. } if candidate == project)
        })
    }

    fn apply_events(&mut self, events: Vec<RuntimeEvent>, cx: &mut Context<Self>) {
        let before = self.published.clone();
        for event in events {
            match event {
                // A publication updates data, never chooses the person's
                // next route. Late startup/index replies must not move Home
                // into an arbitrary first package or add navigation history.
                RuntimeEvent::SnapshotChanged(_) => {}
                RuntimeEvent::PersistRequested(_) => {
                    // Ordinary events mark the current root dirty; an older
                    // event snapshot cannot overwrite newer durable evidence.
                    self.persist_snapshot(&self.snapshot(), cx);
                }
                RuntimeEvent::RequestCompleted { request, outcome } => {
                    let attachment = self.store.as_ref()
                        .and_then(|store| store.read(cx).current_owner_attachment());
                    let basis = self.snapshot().key();
                    if let Some(intent) = self.connection_probe.finish(
                        request, outcome, basis, attachment.as_ref(),
                    ) {
                        self.dispatch_runtime(intent, cx);
                    }
                }
                RuntimeEvent::RejectedStale(_) => {}
            }
        }
        self.publish_snapshot(cx);
        if before.as_ref().is_some_and(|before| self.snapshot().workspace().projects.iter().any(|row|
            row.phase == crate::model::ProjectPhase::Ready && row.operation.as_ref().is_some_and(|operation|
                matches!(operation.observation.as_ref(), Some(backend_library::IndexOperationObservation::Known(status))
                    if matches!(status.state, backend_library::IndexOperationState::Published(_))))
            && !before.workspace().projects.iter().any(|old| old.id == row.id && old.phase == crate::model::ProjectPhase::Ready)))
        {
            // Hydration has its own read authority. Failure here cannot undo an
            // already admitted durable publication receipt.
            let basis = self.snapshot().key();
            let request = self.runtime.allocate_request();
            self.queue(Intent::RefreshRoot { basis, request }, cx);
        }
        // A project the owner just indexed brings the packages it builds with.
        super::acquire::follow_indexed_projects(before.as_deref(), &self.snapshot(), cx.weak_entity(), cx);
        // Cold restart restores durable Indexing rows without an ephemeral
        // request; reattach them once through the typed intent path.
        self.resume_saved_indexes(cx);
        self.schedule_pending_indexes(cx);
        self.schedule_operation_observation(cx);
    }


}

fn overlay_claims(mut state: crate::model::PersistedDesktopState, claims: Vec<(crate::core::LocalProjectId, crate::model::IndexOperationClaim)>) -> crate::model::PersistedDesktopState {
    for (project, operation) in claims {
        if let Some(row) = state.shelf.iter_mut().find(|row| row.local_path == project.as_str()) {
            row.operation = Some(operation);
            row.phase = crate::model::PersistedProjectPhase::Indexing;
        }
    }
    state
}


#[cfg(test)]
mod cargo_queue_tests;
#[cfg(test)]
mod local_index_queue_tests;
#[cfg(test)]
mod durable_writer_tests;

fn folder_picker_outcome(paths: Vec<PathBuf>) -> FolderPickerOutcome {
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for path in paths {
        if !path.is_dir() {
            return FolderPickerOutcome::Failed(Arc::from(format!(
                "The selected path is not a folder: {}",
                path.display()
            )));
        }
        if path.as_os_str().is_empty() {
            return FolderPickerOutcome::Failed(Arc::from("The selected folder path is empty."));
        }
        if seen.insert(path.clone()) {
            selected.push(path);
        }
    }
    if selected.is_empty() {
        FolderPickerOutcome::Cancelled
    } else {
        FolderPickerOutcome::Selected(selected.into())
    }
}

fn bound_picker_error(error: impl std::fmt::Display) -> Arc<str> {
    let message = error
        .to_string()
        .chars()
        .filter(|character| !character.is_control())
        .take(240)
        .collect::<String>();
    Arc::from(format!(
        "The native folder picker is unavailable: {message}"
    ))
}

impl SnapshotReadModel for UiRootEntity {
    fn snapshot(&self) -> Arc<AppSnapshot> {
        self.snapshot()
    }
}

impl IntentDispatcher for UiRootEntity {
    fn dispatch(&mut self, intent: Intent) {
        // This context-free adapter cannot capture a current read receipt.
        // Read-backed legacy resolution goes through queue/queue_read.
        if !matches!(intent, Intent::ResolveCargoBrowse { .. }) { self.pending.push(QueuedIntent::Plain(intent)); }
    }
}

/// One installed GPUI entity graph. The root is the only application state
/// owner; the store is its data plane (snapshot mirror, keyed page resources,
/// read pool). This wrapper exists so native startup and harness code can
/// retain a stable installation handle.
pub struct UiEntityGraph {
    /// The single root entity.
    pub root: Entity<UiRootEntity>,
    /// The data-plane store region views subscribe to.
    pub store: Entity<DataStore>,
}

impl UiEntityGraph {
    /// Installs the root entity and a store without a read lane (pages are
    /// reported unavailable). Production uses [`Self::install_with_reads`].
    pub fn install(
        cx: &mut App,
        runtime: DesktopRuntime,
        persistence: Option<PersistentState>,
    ) -> Self {
        Self::install_with_reads(cx, runtime, persistence, None)
    }

    /// Installs the root entity, the store, and the store's read pool.
    pub fn install_with_reads(
        cx: &mut App,
        runtime: DesktopRuntime,
        persistence: Option<PersistentState>,
        reads: Option<ReadPool>,
    ) -> Self {
        Self::install_with_owner(cx, runtime, persistence, reads, None, None)
    }

    /// [`Self::install_with_reads`] for a window that opens before its owner
    /// answered (W-Open I1): the store holds reads until the gate says the
    /// owner is ready, and every owner state arrives as a data event
    /// ([`super::owner::watch`]). `None`: the owner already answered.
    /// `keep` paints the launch snapshot's pages meanwhile and saves the
    /// route's pages for the next launch (W-Open I2).
    pub(crate) fn install_with_owner(
        cx: &mut App,
        runtime: DesktopRuntime,
        persistence: Option<PersistentState>,
        reads: Option<ReadPool>,
        gate: Option<super::owner::OwnerGate>,
        keep: Option<super::snapshot::Keep>,
    ) -> Self {
        Self::install_with_bootstrap(cx, runtime, persistence, reads, gate, keep, None)
    }

    pub(crate) fn install_with_bootstrap(
        cx: &mut App,
        runtime: DesktopRuntime,
        persistence: Option<PersistentState>,
        reads: Option<ReadPool>,
        gate: Option<super::owner::OwnerGate>,
        keep: Option<super::snapshot::Keep>,
        binding: Option<crate::host::bootstrap::Binding>,
    ) -> Self {
        let store = DataStore::install_with_owner(cx, runtime.snapshot(), reads, gate.clone(), keep);
        let attached = store.clone();
        let root = cx.new(|cx| {
            let origin = runtime.snapshot();
            let mut root = UiRootEntity::new(runtime, persistence);
            // Keep the launch origin even if discovery completed while the
            // platform was starting: local edits always win the cold merge.
            root.bootstrap = binding.map(|binding| (binding, origin));
            root.attach(Some(attached), cx);
            root
        });
        if let Some(gate) = gate {
            super::owner::watch(gate, &root, &store, cx);
        }
        let quitting = root.clone();
        cx.on_app_quit(move |cx| {
            let saved = quitting.update(cx, |root, cx| root.finish_persistence(cx));
            let deadline = cx.background_executor().timer(std::time::Duration::from_secs(5));
            async move {
                use std::future::Future as _;
                let Some(saved) = saved else { return; };
                let mut saved = std::pin::pin!(saved.recv());
                let mut deadline = std::pin::pin!(deadline);
                let result = std::future::poll_fn(|cx| {
                    if let std::task::Poll::Ready(result) = saved.as_mut().poll(cx) {
                        return std::task::Poll::Ready(match result {
                            Ok(Ok(_)) => None,
                            Ok(Err(error)) => Some(error),
                            Err(_) => Some(WriteFailure { message: "The local writer closed before confirming the latest changes. Previously saved state remains available.".into() }),
                        });
                    }
                    if deadline.as_mut().poll(cx).is_ready() { return std::task::Poll::Ready(Some(WriteFailure { message: "Saving the latest changes did not finish before close. Previously saved operation claims remain available for recovery.".into() })); }
                    std::task::Poll::Pending
                }).await;
                if let Some(error) = result { eprintln!("backend-desktop: {}", error.message); }
            }
        }).detach();
        Self { root, store }
    }
}

#[cfg(test)]
mod connection_probe_tests {
    use super::*;

    fn basis() -> crate::core::VersionedRoot {
        crate::core::VersionedRoot::synthetic(
            backend_library::view_state_root(&[("probe".to_owned(), "owner".to_owned())]), 1,
        )
    }

    fn attachment() -> OwnerAttachment {
        DataStore::new(Arc::new(AppSnapshot::empty(basis())), None)
            .current_owner_attachment().expect("synthetic serving attachment")
    }

    #[test]
    fn superseded_probe_retires_once_and_allows_a_later_reconnect() {
        let mut probe = ConnectionProbeLatch::default();
        let first = crate::navigation::RequestId::new(701);
        let second = crate::navigation::RequestId::new(702);
        let attachment = attachment();

        assert!(probe.begin(first, ConnectionStatus::TransportFailed, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(crate::navigation::RequestId::new(799), RequestOutcome::Succeeded, basis(), Some(&attachment)),
            None,
            "an unrelated request terminal cannot release this probe"
        );
        assert!(!probe.begin(second, ConnectionStatus::Unknown, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(first, RequestOutcome::Superseded, basis(), Some(&attachment)),
            Some(Intent::ConnectionProbeAborted {
                previous: ConnectionStatus::TransportFailed,
            })
        );
        assert_eq!(probe.finish(first, RequestOutcome::Succeeded, basis(), Some(&attachment)), None);
        assert!(!probe.is_active());

        assert!(probe.begin(second, ConnectionStatus::Connected, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(second, RequestOutcome::Succeeded, basis(), Some(&attachment)),
            Some(Intent::ConnectionResult { result: ConnectionStatus::Connected })
        );
        assert!(!probe.is_active());
    }

    #[test]
    fn refusal_and_actor_failure_are_not_reported_as_success() {
        let mut probe = ConnectionProbeLatch::default();
        let refused = crate::navigation::RequestId::new(703);
        let attachment = attachment();
        assert!(probe.begin(refused, ConnectionStatus::Unknown, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(
                refused,
                RequestOutcome::Refused(super::super::coordinator::RequestRefusalReason::Closed),
                basis(), Some(&attachment),
            ),
            Some(Intent::ConnectionProbeAborted {
                previous: ConnectionStatus::Unknown,
            })
        );

        let failed = crate::navigation::RequestId::new(704);
        assert!(probe.begin(failed, ConnectionStatus::Unknown, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(failed, RequestOutcome::Failed(FaultCode::Transport), basis(), Some(&attachment)),
            Some(Intent::ConnectionResult { result: ConnectionStatus::TransportFailed })
        );
        let unsupported = crate::navigation::RequestId::new(707);
        assert!(probe.begin(unsupported, ConnectionStatus::Connected, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(unsupported, RequestOutcome::Failed(FaultCode::Unsupported), basis(), Some(&attachment)),
            Some(Intent::ConnectionResult { result: ConnectionStatus::CheckUnavailable }),
        );
        let protocol = crate::navigation::RequestId::new(708);
        assert!(probe.begin(protocol, ConnectionStatus::Connected, basis(), attachment.clone()));
        assert_eq!(
            probe.finish(protocol, RequestOutcome::Failed(FaultCode::Protocol), basis(), Some(&attachment)),
            Some(Intent::ConnectionResult { result: ConnectionStatus::CheckFailed }),
        );
    }

    #[gpui::test]
    fn an_old_same_root_owner_reply_cannot_complete_the_new_attachments_check(cx: &mut gpui::TestAppContext) {
        let gate = super::super::owner::OwnerGate::ready(basis(), crate::model::ServiceMode::Attached);
        let store = cx.update(|cx| DataStore::install_with_owner(
            cx, Arc::new(AppSnapshot::empty(basis())), None, Some(gate.clone()), None,
        ));
        let old = store.read_with(cx, |store, _| store.current_owner_attachment())
            .expect("first serving attachment");
        let mut probe = ConnectionProbeLatch::default();
        let request = crate::navigation::RequestId::new(705);
        assert!(probe.begin(request, ConnectionStatus::Connected, basis(), old.clone()));
        gate.publish(super::super::owner::OwnerState::Ready {
            key: basis(), mode: crate::model::ServiceMode::Attached,
        });
        store.update(cx, |store, cx| store.owner_ready(cx));
        let current = store.read_with(cx, |store, _| store.current_owner_attachment())
            .expect("replacement serving attachment");
        assert_ne!(old, current);
        assert_eq!(
            probe.finish(request, RequestOutcome::Succeeded, basis(), Some(&current)),
            Some(Intent::ConnectionProbeAborted { previous: ConnectionStatus::Unknown }),
        );
        assert!(!probe.is_active());

        let request = crate::navigation::RequestId::new(706);
        assert!(probe.begin(request, ConnectionStatus::Connected, basis(), current.clone()));
        let newer_basis = crate::core::VersionedRoot::synthetic(basis().root(), 2);
        assert_eq!(
            probe.finish(request, RequestOutcome::Succeeded, newer_basis, Some(&current)),
            Some(Intent::ConnectionProbeAborted { previous: ConnectionStatus::Connected }),
            "a current attachment cannot promote a result asked at an old root",
        );
    }
}
