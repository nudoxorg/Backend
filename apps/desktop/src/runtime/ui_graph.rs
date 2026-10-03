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
use crate::core::{IntentDispatcher, ProducerAuthority, SnapshotReadModel};
use crate::model::{AppSnapshot, ConnectionStatus, PersistentState};
use crate::navigation::{FolderPickerOutcome, Intent, Route, View};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, PathPromptOptions, Task};
use std::collections::BTreeSet;
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
}

impl ConnectionProbeLatch {
    fn begin(
        &mut self,
        request: crate::navigation::RequestId,
        previous: ConnectionStatus,
    ) -> bool {
        if self.active.is_some() {
            return false;
        }
        self.active = Some(ActiveConnectionProbe { request, previous });
        true
    }

    fn finish(
        &mut self,
        request: crate::navigation::RequestId,
        outcome: RequestOutcome,
    ) -> Option<Intent> {
        let active = self.active.as_ref()?;
        if active.request != request {
            return None;
        }
        let previous = active.previous;
        self.active.take()?;
        Some(match outcome {
            RequestOutcome::Succeeded => Intent::ConnectionResult { connected: true },
            RequestOutcome::Failed => Intent::ConnectionResult { connected: false },
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
}

#[derive(Clone, Debug)]
pub(crate) struct GraphViewRequest {
    pub target: GraphDestination,
    pub route: Route,
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
    Index { intent: Intent, attachment: OwnerAttachment },
    Read { intent: Intent, lease: RouteReadLease, sequence: u64 },
}

impl QueuedIntent {
    fn intent(&self) -> &Intent { match self { Self::Plain(intent) | Self::Index { intent, .. } | Self::Read { intent, .. } => intent } }
}

/// The complete UI-thread state owner for one desktop window.
pub struct UiRootEntity {
    runtime: DesktopRuntime,
    pending: Vec<QueuedIntent>,
    persistence: Option<PersistentState>,
    bootstrap: Option<(crate::host::bootstrap::Binding, Arc<AppSnapshot>)>,
    folder_picker_task: Option<Task<()>>,
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
            bootstrap: None,
            folder_picker_task: None,
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
        self.runtime.has_pending_work()
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
                    if self.store.as_ref().is_some_and(|store| store.read(cx).admits_owner_attachment(&attachment))
                        && matches!(&intent, Intent::IndexProject { basis, project, .. } if self.snapshot().key().same_authority(*basis)
                            && self.snapshot().workspace().projects.iter().any(|item| item.id == *project
                                && item.phase == crate::model::ProjectPhase::Indexing && item.request.is_none())) => self.dispatch_index(intent, attachment, cx),
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
    fn dispatch_index(&mut self, intent: Intent, attachment: OwnerAttachment, cx: &mut Context<Self>) {
        if let Some(persistence) = &self.persistence {
            let submitted = crate::navigation::reduce(&self.snapshot(), intent.clone()).snapshot;
            if let Err(error) = persistence.save(&PersistentState::project(&submitted)) {
                if let Intent::IndexProject { project, basis, .. } = intent {
                    let message: Arc<str> = format!("The folder is still on your shelf, but its index request could not be saved: {error}")
                        .chars().filter(|character| !character.is_control()).take(240).collect::<String>().into();
                    // The exact attachment still owns this synchronous preflight.
                    if self.store.as_ref().is_some_and(|store| store.read(cx).admits_owner_attachment(&attachment)) {
                        let events = self.runtime.reject_unsent_index(&project, basis, message);
                        self.apply_events(events, cx);
                    }
                }
                return;
            }
        }
        if self.store.as_ref().is_some_and(|store| store.read(cx).admits_owner_attachment(&attachment)) {
            self.dispatch(intent, cx);
        }
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
        if let Intent::ResolveCargoBrowse { expected, context } = &intent
            && self.cargo_resolution_dependency(expected, context, cx).is_none() { return; }
        self.reduced = self.reduced.saturating_add(1);
        match intent {
            Intent::SetView(view @ (View::Page | View::Code))
                if self.snapshot().overlay().is_none()
                    && matches!(self.snapshot().route(), Route::World | Route::Symbol(crate::navigation::SymbolRoute { view: View::Graph, .. })) => {
                let snapshot = self.snapshot();
                self.graph_view_generation = self.graph_view_generation.wrapping_add(1);
                cx.emit(GraphViewRequest {
                    target: if view == View::Page { GraphDestination::Page } else { GraphDestination::Code },
                    route: snapshot.route().clone(),
                    root: snapshot.key(),
                    authority: snapshot.key().authority(),
                    attachment: self.store.as_ref().and_then(|store| store.read(cx).current_owner_attachment()),
                    sequence: self.graph_view_generation,
                });
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
                crate::host::editor::open(launch.as_ref(), None, &path, line);
            }
            Intent::TestConnection => {
                if !self.connection_probe.is_active() {
                    let request = self.runtime.allocate_request();
                    let previous = self.snapshot().settings().connection;
                    if self.connection_probe.begin(request, previous) {
                        self.dispatch_runtime(Intent::TestConnection, cx);
                        self.dispatch_runtime(
                            Intent::RefreshRoot {
                                basis: self.snapshot().key(),
                                request,
                            },
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
                // Save local actions that occurred while paths were unavailable
                // before deferred admissions can cross the actor boundary.
                if let Some(persistence) = &self.persistence {
                    if let Err(error) = persistence.save(&PersistentState::project(&self.snapshot())) {
                        eprintln!("backend-desktop: save recovered local session: {error}");
                    }
                }
            } else { self.bootstrap = Some((binding, origin)); }
        }
        self.dispatch_runtime(Intent::OwnerReady { key, mode }, cx);
        let request = self.runtime.allocate_request();
        self.dispatch_runtime(Intent::RefreshRoot { basis: key, request }, cx);
        // An index an earlier build wrote was set aside while the owner
        // started: say so, and index the shelf's projects again.
        if let Some(moved) = crate::host::aside::take() {
            self.dispatch(Intent::LibraryRebuilding { kept_at: Arc::from(moved.display().to_string()) }, cx);
        }
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
        {
            return;
        }
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
            let _ = root.update(cx, Self::schedule_pending_indexes);
        });
    }

    fn schedule_pending_indexes(&mut self, cx: &mut Context<Self>) {
        let projects = self
            .snapshot()
            .workspace()
            .projects
            .iter()
            .filter(|project| {
                project.phase == crate::model::ProjectPhase::Indexing
                    && project.request.is_none()
                    && !self.index_intent_pending(&project.id)
            })
            .map(|project| project.id.clone())
            .collect::<Vec<_>>();
        for project in projects {
            self.schedule_index(project, cx);
        }
    }

    fn schedule_index(&mut self, project: crate::core::LocalProjectId, cx: &mut Context<Self>) {
        if self.index_intent_pending(&project)
            || !self.snapshot().workspace().projects.iter().any(|item| item.id == project && item.phase == crate::model::ProjectPhase::Indexing && item.request.is_none())
        {
            return;
        }
        let Some(store) = &self.store else { return; };
        let store = store.read(cx);
        let Some(attachment) = store.current_owner_attachment() else { return; };
        let basis = self.snapshot().key();
        if !store.snapshot().key().same_authority(basis) { return; }
        let request = self.runtime.allocate_request();
        self.pending.push(QueuedIntent::Index {
            intent: Intent::IndexProject { project, basis, request },
            attachment,
        });
        self.schedule_flush(cx);
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
                RuntimeEvent::PersistRequested(snapshot) => {
                    if let Some(persistence) = &self.persistence {
                        let _ = persistence.save(&PersistentState::project(&snapshot));
                    }
                }
                RuntimeEvent::RequestCompleted { request, outcome } => {
                    if let Some(intent) = self.connection_probe.finish(request, outcome) {
                        self.dispatch_runtime(intent, cx);
                    }
                }
                RuntimeEvent::RejectedStale(_) => {}
            }
        }
        self.publish_snapshot(cx);
        // A project the owner just indexed brings the packages it builds with.
        super::acquire::follow_indexed_projects(before.as_deref(), &self.snapshot(), cx.weak_entity(), cx);
        // Cold restart restores durable Indexing rows without an ephemeral
        // request; reattach them once through the typed intent path.
        self.schedule_pending_indexes(cx);
    }


}

#[cfg(test)]
mod cargo_queue_tests;
#[cfg(test)]
mod local_index_queue_tests;

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
        Self { root, store }
    }
}

#[cfg(test)]
mod connection_probe_tests {
    use super::*;

    #[test]
    fn superseded_probe_retires_once_and_allows_a_later_reconnect() {
        let mut probe = ConnectionProbeLatch::default();
        let first = crate::navigation::RequestId::new(701);
        let second = crate::navigation::RequestId::new(702);

        assert!(probe.begin(first, ConnectionStatus::Disconnected));
        assert_eq!(
            probe.finish(crate::navigation::RequestId::new(799), RequestOutcome::Succeeded),
            None,
            "an unrelated request terminal cannot release this probe"
        );
        assert!(!probe.begin(second, ConnectionStatus::Unknown));
        assert_eq!(
            probe.finish(first, RequestOutcome::Superseded),
            Some(Intent::ConnectionProbeAborted {
                previous: ConnectionStatus::Disconnected,
            })
        );
        assert_eq!(probe.finish(first, RequestOutcome::Succeeded), None);
        assert!(!probe.is_active());

        assert!(probe.begin(second, ConnectionStatus::Connected));
        assert_eq!(
            probe.finish(second, RequestOutcome::Succeeded),
            Some(Intent::ConnectionResult { connected: true })
        );
        assert!(!probe.is_active());
    }

    #[test]
    fn refusal_and_actor_failure_are_not_reported_as_success() {
        let mut probe = ConnectionProbeLatch::default();
        let refused = crate::navigation::RequestId::new(703);
        assert!(probe.begin(refused, ConnectionStatus::Unknown));
        assert_eq!(
            probe.finish(
                refused,
                RequestOutcome::Refused(super::super::coordinator::RequestRefusalReason::Closed),
            ),
            Some(Intent::ConnectionProbeAborted {
                previous: ConnectionStatus::Unknown,
            })
        );

        let failed = crate::navigation::RequestId::new(704);
        assert!(probe.begin(failed, ConnectionStatus::Unknown));
        assert_eq!(
            probe.finish(failed, RequestOutcome::Failed),
            Some(Intent::ConnectionResult { connected: false })
        );
    }
}
