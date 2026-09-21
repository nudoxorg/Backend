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
    CommandPaletteState, FocusId, FocusTree, Intent, ModalId, ModalStack, OrbitRoute, Overlay,
    PackageLane, PackageRoute, Route,
};
use backend_library::{CommandId, SurfaceCommand};
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, Window};
use std::sync::Arc;

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
        }
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
        self.modals = match snapshot.overlay() {
            Some(Overlay::Settings(page)) => {
                let mut stack = ModalStack::default();
                let _ = page;
                stack.push(ModalId::Settings, FocusId::Shell);
                stack
            }
            Some(Overlay::CommandPalette) | None => ModalStack::default(),
        };
    }

    fn frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for intent in std::mem::take(&mut self.pending) {
            self.dispatch(intent, cx);
        }
        let events = self.runtime.poll();
        self.apply_events(events, cx);
        self.timeline
            .set_reduced_motion(self.snapshot().settings().reduced_motion);
        self.timeline.advance();
        window.request_animation_frame();
        crate::views::render_root(self, window, cx)
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
