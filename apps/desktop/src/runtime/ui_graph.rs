//! The single application state owner.
//!
//! `UiRootEntity` is deliberately boring: one runtime, one immutable
//! snapshot pointer, and typed intents. It renders nothing — the window root
//! is the [`Shell`](crate::shell::Shell), whose regions read the snapshot
//! through the [`DataStore`] mirror. An input queues an
//! [`Intent`](crate::navigation::Intent); it is reduced at the end of the
//! current effect cycle, and engine results arrive through the actor's wake
//! task, so nothing here ever needs a frame.

use super::coordinator::{DesktopRuntime, RuntimeEvent};
use super::reads::ReadPool;
use super::store::DataStore;
use crate::core::{IntentDispatcher, SnapshotReadModel};
use crate::model::{AppSnapshot, PersistentState};
use crate::navigation::{FolderPickerOutcome, Intent, OrbitRoute, PackageLane, PackageRoute, Route, View};
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
#[derive(Clone, Debug)]
pub(crate) struct GraphViewRequest {
    pub target: GraphDestination,
    pub route: Route,
    pub root: crate::core::VersionedRoot,
    pub sequence: u64,
}

/// The complete UI-thread state owner for one desktop window.
pub struct UiRootEntity {
    runtime: DesktopRuntime,
    pending: Vec<Intent>,
    persistence: Option<PersistentState>,
    first_catalog_route_admitted: bool,
    folder_picker_task: Option<Task<()>>,
    connection_probe: Option<crate::navigation::RequestId>,
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
            pending: vec![Intent::RefreshRoot { basis, request }],
            persistence,
            first_catalog_route_admitted: false,
            folder_picker_task: None,
            connection_probe: None,
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
        for intent in pending {
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
            || previous.key() != snapshot.key() || previous.overlay() != snapshot.overlay()) {
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
        self.pending.push(intent);
        self.schedule_flush(cx);
    }

    /// Applies a typed intent immediately from a harness or startup phase.
    pub fn dispatch(&mut self, intent: Intent, cx: &mut Context<Self>) {
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
                    sequence: self.graph_view_generation,
                });
            }
            Intent::OpenFolderPicker => self.start_folder_picker(cx),
            Intent::RevealProject(project) => cx.reveal_path(&project.path()),
            Intent::TestConnection => {
                self.dispatch_runtime(Intent::TestConnection, cx);
                if self.connection_probe.is_none() {
                    let request = self.runtime.allocate_request();
                    self.connection_probe = Some(request);
                    self.dispatch_runtime(
                        Intent::RefreshRoot {
                            basis: self.snapshot().key(),
                            request,
                        },
                        cx,
                    );
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
            other => self.dispatch_runtime(other, cx),
        }
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
        if self.index_intent_pending(&project) {
            return;
        }
        let basis = self.snapshot().key();
        let request = self.runtime.allocate_request();
        self.queue(
            Intent::IndexProject {
                project,
                basis,
                request,
            },
            cx,
        );
    }

    fn index_intent_pending(&self, project: &crate::core::LocalProjectId) -> bool {
        self.pending.iter().any(|intent| {
            matches!(intent, Intent::IndexProject { project: candidate, .. } if candidate == project)
        })
    }

    fn apply_events(&mut self, events: Vec<RuntimeEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                RuntimeEvent::SnapshotChanged(snapshot) => {
                    self.admit_first_catalog_route(&snapshot, cx);
                }
                RuntimeEvent::PersistRequested(snapshot) => {
                    if let Some(persistence) = &self.persistence {
                        let _ = persistence.save(&PersistentState::project(&snapshot));
                    }
                }
                RuntimeEvent::RequestCompleted { request, succeeded }
                    if self.connection_probe == Some(request) =>
                {
                    self.connection_probe = None;
                    self.dispatch_runtime(
                        Intent::ConnectionResult {
                            connected: succeeded,
                        },
                        cx,
                    );
                }
                RuntimeEvent::RequestCompleted { .. } | RuntimeEvent::RejectedStale(_) => {}
            }
        }
        self.publish_snapshot(cx);
        // Cold restart restores durable Indexing rows without an ephemeral
        // request; reattach them once through the typed intent path.
        self.schedule_pending_indexes(cx);
    }

    fn admit_first_catalog_route(&mut self, snapshot: &AppSnapshot, cx: &mut Context<Self>) {
        if self.first_catalog_route_admitted {
            return;
        }
        // A service catalog can be live before the first folder has been
        // admitted to the shelf. Keep cold first launch on the onboarding
        // surface until the user chooses a source; otherwise a registry row
        // silently replaces the empty-project affordance.
        if snapshot.workspace().projects.is_empty() {
            return;
        }
        let Some(catalog) = snapshot.catalog().loaded_value() else {
            return;
        };
        let Some(package) = catalog.packages.first() else {
            return;
        };
        if !matches!(snapshot.route(), Route::Orbit(OrbitRoute::Home)) {
            self.first_catalog_route_admitted = true;
            return;
        }
        self.first_catalog_route_admitted = true;
        self.queue(
            Intent::Navigate(Route::Package(PackageRoute {
                project: None,
                package: package.coordinate.clone(),
                lane: PackageLane::Overview,
                selected: Some(package.object),
                at: None,
            })),
            cx,
        );
    }

}

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
        self.pending.push(intent);
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
        let store = DataStore::install(cx, runtime.snapshot(), reads);
        let attached = store.clone();
        let root = cx.new(|cx| {
            let mut root = UiRootEntity::new(runtime, persistence);
            root.attach(Some(attached), cx);
            root
        });
        Self { root, store }
    }
}
