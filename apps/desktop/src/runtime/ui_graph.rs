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
    ActionId, CommandPaletteState, EscapeResult, FocusId, FocusTree, Intent, ModalId, ModalStack,
    OrbitRoute, Overlay, PackageLane, PackageRoute, Route,
};
use crate::ui::components::ActionTree;
use backend_library::{CommandId, SurfaceCommand};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, Subscription, Window};
use std::collections::HashMap;
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
    persistence: Option<PersistentState>,
    requested_surface: Option<(CommandId, String, VersionedRoot)>,
    first_catalog_route_admitted: bool,
    window_activation_subscription: Option<Subscription>,
    capture_time: Option<Duration>,
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
            persistence,
            requested_surface: None,
            first_catalog_route_admitted: false,
            window_activation_subscription: None,
            capture_time: None,
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
        let events = self.runtime.dispatch(intent);
        self.apply_events(events, cx);
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
                RuntimeEvent::RejectedStale(_) => {}
            }
        }
        cx.notify();
    }

    fn admit_first_catalog_route(&mut self, snapshot: &AppSnapshot, cx: &mut Context<Self>) {
        if self.first_catalog_route_admitted {
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
        Self { root }
    }
}
