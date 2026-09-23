//! The single GPUI application entity.
//!
//! `UiRootEntity` is deliberately boring: one runtime, one immutable
//! snapshot pointer, and typed shell navigation. There is no second store,
//! reducer entity, transport entity, or view-local copy of engine state. A
//! frame reads the snapshot, emits elements, and returns; an input queues an
//! [`Intent`](crate::navigation::Intent), which is reduced at the next frame.

use super::coordinator::{DesktopRuntime, RuntimeEvent};
use super::{AnimationTimeline, LiveFrameClock};
use crate::core::{IntentDispatcher, SnapshotReadModel, VersionedRoot};
use crate::model::{AppSnapshot, PersistentState};
use crate::navigation::{
    ActionId, CommandPaletteState, EscapeResult, FocusId, FocusTree, FolderPickerOutcome, Intent,
    ModalId, ModalStack, OrbitRoute, Overlay, PackageLane, PackageRoute, Route,
};
use crate::ui::components::ActionTree;
use backend_library::{CommandId, SurfaceCommand};
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, PathPromptOptions, Render, Subscription,
    Task, Window,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The complete UI-thread state owner for one desktop window.
pub struct UiRootEntity {
    runtime: DesktopRuntime,
    pending: Vec<Intent>,
    timeline: AnimationTimeline<LiveFrameClock>,
    focus: FocusTree,
    modals: ModalStack,
    palette: CommandPaletteState,
    /// Last app overlay handed to gpui_component::Root. This is only an
    /// idempotence marker; Root owns the actual dialog/sheet lifetime.
    component_overlay: Option<Overlay>,
    persistence: Option<PersistentState>,
    requested_surface: Option<(CommandId, String, VersionedRoot)>,
    requested_local_package: Option<(crate::core::LocalProjectId, VersionedRoot)>,
    first_catalog_route_admitted: bool,
    window_activation_subscription: Option<Subscription>,
    capture_time: Option<Duration>,
    folder_picker_task: Option<Task<()>>,
    connection_probe: Option<crate::navigation::RequestId>,
}

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
            timeline: AnimationTimeline::new(LiveFrameClock::default()),
            focus: FocusTree::default(),
            modals: ModalStack::default(),
            palette: CommandPaletteState::default(),
            component_overlay: None,
            persistence,
            requested_surface: None,
            requested_local_package: None,
            first_catalog_route_admitted: false,
            window_activation_subscription: None,
            capture_time: None,
            folder_picker_task: None,
            connection_probe: None,
        }
    }

    /// Connects the semantic focus owner to GPUI's real window activation
    /// notifications. The subscription is installed once by the visible and
    /// capture roots; every later activation only changes focus-ring policy on
    /// the UI thread and preserves the exact focused route.
    pub(crate) fn observe_window_activation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.window_activation_subscription.is_some() {
            return;
        }
        let subscription = cx.observe_window_activation(window, |this, window, cx| {
            this.set_window_focused(window.is_window_active(), cx);
        });
        self.window_activation_subscription = Some(subscription);
    }

    /// Returns the one immutable read model used by every visual surface.
    #[must_use]
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        self.runtime.snapshot()
    }

    /// Returns the typed focus model for accessibility probes.
    #[must_use]
    pub const fn focus(&self) -> &FocusTree {
        &self.focus
    }

    /// Returns the typed overlay model.
    #[must_use]
    pub const fn modals(&self) -> &ModalStack {
        &self.modals
    }

    /// Returns the command palette projection.
    #[must_use]
    pub const fn palette(&self) -> &CommandPaletteState {
        &self.palette
    }

    /// Returns the overlay route most recently admitted to CE Root.
    pub(crate) const fn component_overlay(&self) -> Option<Overlay> {
        self.component_overlay
    }

    /// Records the route handed to CE Root without owning a second overlay
    /// stack. The snapshot remains the source of truth for the desired route.
    pub(crate) fn set_component_overlay(&mut self, overlay: Option<Overlay>) {
        self.component_overlay = overlay;
    }

    /// Moves semantic focus to the next control in the active scope.
    pub fn focus_next(&mut self, cx: &mut Context<Self>) -> Option<FocusId> {
        let focused = self.focus.focus_next();
        if focused.is_some() {
            cx.notify();
        }
        focused
    }

    /// Moves semantic focus to the previous control in the active scope.
    pub fn focus_previous(&mut self, cx: &mut Context<Self>) -> Option<FocusId> {
        let focused = self.focus.focus_previous();
        if focused.is_some() {
            cx.notify();
        }
        focused
    }

    /// Preserves semantic focus while the native window leaves or regains focus.
    pub fn set_window_focused(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focus.set_window_focused(focused);
        cx.notify();
    }

    /// Applies the Escape hierarchy and queues the corresponding typed intent.
    pub fn dismiss_escape(&mut self, cx: &mut Context<Self>) -> EscapeResult {
        let result = self.focus.escape();
        if matches!(result, EscapeResult::Dismissed(_)) {
            self.queue(Intent::DismissOverlay, cx);
        }
        result
    }

    /// Routes a stable keyboard/command action through the active focus scope.
    ///
    /// Actions with payloads (selection movement and activation) remain owned
    /// by the focused GPUI CE component; this boundary still reports them as
    /// consumed so an outer shell cannot also interpret the same key.
    pub fn dispatch_action(&mut self, action: ActionId, cx: &mut Context<Self>) -> bool {
        if !self.focus.accepts(action) {
            return false;
        }
        if action == ActionId::DismissOverlay {
            return !matches!(self.dismiss_escape(cx), EscapeResult::Ignored);
        }
        if let Some(intent) = action.intent() {
            self.queue(intent, cx);
        }
        true
    }

    /// Pins the transient timeline to a deterministic capture timestamp.
    ///
    /// The screenshot harness calls this immediately before each draw. It is
    /// deliberately render-only state: the immutable snapshot and every
    /// product intent continue to use the normal GPUI event path.
    pub(crate) fn set_capture_time(&mut self, now: Duration) {
        self.capture_time = Some(now);
    }

    /// Queues a typed intent and wakes the next frame.
    pub fn queue(&mut self, intent: Intent, cx: &mut Context<Self>) {
        self.pending.push(intent);
        cx.notify();
    }

    /// Applies a typed intent immediately from a harness or startup phase.
    pub fn dispatch(&mut self, intent: Intent, cx: &mut Context<Self>) {
        match intent {
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

    /// Ensures one typed product surface is admitted for the current root.
    ///
    /// Render functions call this at their boundary instead of reaching into
    /// a client.  The identity tuple makes the request idempotent across
    /// animation frames while still allowing a different package, lane, or
    /// producer root to replace it.
    pub(crate) fn ensure_surface(&mut self, command: SurfaceCommand, cx: &mut Context<Self>) {
        let basis = self.snapshot().key();
        let identity = (command.id(), format!("{command:?}"), basis);
        if self.requested_surface.as_ref() == Some(&identity) {
            return;
        }
        self.requested_surface = Some(identity);
        let request = self.runtime.allocate_request();
        self.queue(
            Intent::RefreshSurface {
                command,
                basis,
                request,
            },
            cx,
        );
    }

    /// Ensures one local project's offline package facts are admitted.
    ///
    /// Like [`Self::ensure_surface`], a render boundary calls this instead of
    /// reading files: the read runs on the actor's local lane and its result
    /// arrives as a snapshot. Facts already admitted for `project` are kept;
    /// otherwise one read is issued per project and root.
    pub(crate) fn ensure_local_package(
        &mut self,
        project: &crate::core::LocalProjectId,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot();
        if snapshot
            .local_package()
            .loaded_value()
            .is_some_and(|package| package.project == *project)
        {
            return;
        }
        let basis = snapshot.key();
        let identity = (project.clone(), basis);
        if self.requested_local_package.as_ref() == Some(&identity) {
            return;
        }
        self.requested_local_package = Some(identity);
        let request = self.runtime.allocate_request();
        self.queue(
            Intent::RefreshLocalPackage {
                project: project.clone(),
                basis,
                request,
            },
            cx,
        );
    }

    fn apply_events(&mut self, events: Vec<RuntimeEvent>, cx: &mut Context<Self>) {
        for event in events {
            match event {
                RuntimeEvent::SnapshotChanged(snapshot) => {
                    self.sync_navigation(&snapshot);
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
                RuntimeEvent::RequestCompleted { .. } => {}
                RuntimeEvent::RejectedStale(_) => {}
            }
        }
        cx.notify();
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
            })),
            cx,
        );
    }

    fn sync_navigation(&mut self, snapshot: &AppSnapshot) {
        self.palette.open = matches!(snapshot.overlay(), Some(Overlay::CommandPalette));
    }

    /// Projects the one post-layout action registry into semantic focus IDs.
    ///
    /// GPUI CE remains the owner of native handles and actual Tab dispatch.
    /// This projection only gives the runtime a stable route for modal scope,
    /// Escape restoration, and screenshot assertions. Every key comes from a
    /// visible, enabled action registered while the current frame rendered.
    /// The A11y ActionFrames finalizer must run before this snapshot so the
    /// focused bit and measured bounds belong to this exact post-layout frame.
    fn sync_rendered_actions(&mut self, window: &Window, cx: &Context<Self>) {
        let theme = crate::theme::theme(cx);
        let action_tree = theme.action_tree(window);
        let focus_order = theme.action_focus_order(window);
        let native_focus_owner = theme.action_native_focus_owner(window);
        sync_focus_from_actions(
            &mut self.focus,
            &mut self.modals,
            &mut self.palette,
            &action_tree,
            &focus_order,
            native_focus_owner.as_deref(),
        );
    }

    fn frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Activation can arrive between frames. Sampling it here keeps the
        // focus ring correct even when a platform does not emit a separate
        // observer tick before the first repaint.
        let window_focused = window.is_window_active();
        if window_focused != self.focus.window_focused() {
            self.focus.set_window_focused(window_focused);
        }
        for intent in std::mem::take(&mut self.pending) {
            self.dispatch(intent, cx);
        }
        let events = self.runtime.poll();
        self.apply_events(events, cx);
        // Cold restart restores durable Indexing rows without an ephemeral
        // request. Reattach them through the same typed intent path once per
        // row; an admitted request is recorded before the next frame.
        self.schedule_pending_indexes(cx);
        self.timeline
            .set_reduced_motion(self.snapshot().settings().reduced_motion);
        if let Some(now) = self.capture_time {
            self.timeline.advance_at(now);
        } else {
            self.timeline.advance();
        }
        let rendered = crate::views::render_root(self, window, cx);
        self.sync_rendered_actions(window, cx);
        // A settled window must not keep an ambient animation loop alive. An
        // in-flight engine request still needs a frame to drain its event, and
        // a resize/transition track keeps frames alive only until its terminal
        // value is exact.
        if self.timeline.is_active() || self.runtime.needs_frame() || !self.pending.is_empty() {
            window.request_animation_frame();
        }
        rendered
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

fn action_descends_from(
    action_id: &str,
    modal_root: &str,
    parents: &HashMap<String, Option<String>>,
) -> bool {
    let mut parent = parents.get(action_id).and_then(Option::as_deref);
    let mut steps = 0;
    while let Some(current) = parent {
        if current == modal_root {
            return true;
        }
        parent = parents.get(current).and_then(Option::as_deref);
        steps += 1;
        if steps > parents.len() {
            return false;
        }
    }
    false
}

fn modal_id_for_root(root: &str) -> Option<ModalId> {
    match root {
        "settings-dialog" => Some(ModalId::Settings),
        "command-palette-dialog" => Some(ModalId::CommandPalette),
        _ => None,
    }
}

/// Projects the finalized U1 ActionFrames evidence into U2 semantic focus.
///
/// The measured order map is the only source of rendered Tab order and the
/// native owner is the only source of rendered focus ownership. ActionTree
/// contributes modal root, inert/visibility state, parent relationships, and
/// restore ID; metadata `focused` is intentionally never treated as native
/// evidence here.
fn sync_focus_from_actions(
    focus: &mut FocusTree,
    modals: &mut ModalStack,
    palette: &mut CommandPaletteState,
    action_tree: &ActionTree,
    measured_order: &HashMap<String, u32>,
    native_focus_owner: Option<&str>,
) {
    let parents = action_tree
        .iter()
        .map(|action| {
            (
                action.id().to_string(),
                action.parent_value().map(ToString::to_string),
            )
        })
        .collect::<HashMap<_, _>>();
    let modal_root = action_tree.modal_root().map(ToString::to_string);
    let mut entries = action_tree
        .iter()
        .filter(|action| action.is_focusable() && measured_order.contains_key(action.id().as_ref()))
        .collect::<Vec<_>>();
    entries.sort_by_key(|action| {
        measured_order
            .get(action.id().as_ref())
            .copied()
            .unwrap_or(u32::MAX)
    });
    let is_modal_action = |action_id: &str| {
        modal_root
            .as_deref()
            .is_some_and(|root| action_descends_from(action_id, root, &parents))
    };
    let mut shell_ids = entries
        .iter()
        .filter(|action| !is_modal_action(action.id().as_ref()))
        .map(|action| action.id().clone())
        .collect::<Vec<_>>();
    let modal_ids = entries
        .iter()
        .filter(|action| is_modal_action(action.id().as_ref()))
        .map(|action| action.id().clone())
        .collect::<Vec<_>>();
    // U1 marks the launch control inert while the modal owns focus, so it is
    // absent from measured Tab order. Keep its semantic node registered solely
    // for exact Escape restoration; `sync_action_order` still exposes only
    // modal keys as the active Tab order.
    if let Some(restore) = action_tree.restore_focus() {
        if !shell_ids.contains(restore) {
            shell_ids.push(restore.clone());
        }
    }
    let modal_id = modal_root.as_deref().and_then(modal_id_for_root);
    let modal = modal_id.map(|id| (id.focus(), modal_ids.as_slice()));
    focus.sync_action_order(&shell_ids, modal);

    let current_modal = modals.top();
    let current_focus_modal = focus.active_modal();
    let expected_focus_modal = modal_id.map(ModalId::focus);
    if current_modal != modal_id || current_focus_modal != expected_focus_modal {
        if current_modal.is_some() || current_focus_modal.is_some() {
            let _ = modals.pop();
            let _ = focus.pop_modal();
        }
        if let Some(modal_id) = modal_id {
            let restore = action_tree
                .restore_focus()
                .map(|id| FocusId::Action(focus.action_key(id.as_ref())));
            let restore_route = restore.or_else(|| Some(focus.route().active()));
            let restore_id = restore_route.unwrap_or(FocusId::Shell);
            let _ = modals.push(modal_id, restore_id);
            let _ = focus.push_modal_with_restore(modal_id.focus(), Some(restore_id));
        }
    }
    palette.open = matches!(modal_id, Some(ModalId::CommandPalette));

    let Some(owner) = native_focus_owner else {
        return;
    };
    let owner_is_modal = modal_root
        .as_deref()
        .is_some_and(|root| action_descends_from(owner, root, &parents));
    if owner_is_modal == modal_id.is_some() {
        let native_modal = modal_id.map(ModalId::focus);
        let _ = focus.sync_native_action(owner, native_modal);
    }
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

impl Render for UiRootEntity {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.frame(window, cx)
    }
}

/// One installed GPUI entity graph. The root is the only application state
/// owner; this wrapper exists solely so native startup and harness code can
/// retain a stable installation handle.
pub struct UiEntityGraph {
    /// The single root entity.
    pub root: Entity<UiRootEntity>,
}

impl UiEntityGraph {
    /// Installs the one root entity on the GPUI thread.
    pub fn install(
        cx: &mut App,
        runtime: DesktopRuntime,
        persistence: Option<PersistentState>,
    ) -> Self {
        let root = cx.new(|_| UiRootEntity::new(runtime, persistence));
        crate::views::install_shell_keymap(cx, root.downgrade());
        Self { root }
    }
}
