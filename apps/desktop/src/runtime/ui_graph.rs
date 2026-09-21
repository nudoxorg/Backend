//! GPUI-thread entity graph. Widgets consume narrow read models from here.

use super::coordinator::DesktopRuntime;
use super::{AnimationTimeline, LiveFrameClock};
use crate::core::{IntentDispatcher, SnapshotReadModel};
use crate::model::AppSnapshot;
use crate::navigation::{CommandPaletteState, FocusTree, Intent, ModalStack};
use gpui::{
    App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    Styled, div,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Entity containing the one immutable snapshot pointer seen by views.
pub struct SnapshotEntity {
    snapshot: Arc<AppSnapshot>,
}

impl SnapshotEntity {
    /// Returns the shared snapshot pointer.
    #[must_use]
    pub fn snapshot(&self) -> Arc<AppSnapshot> {
        Arc::clone(&self.snapshot)
    }

    fn replace(&mut self, snapshot: Arc<AppSnapshot>) {
        self.snapshot = snapshot;
    }
}

impl SnapshotReadModel for SnapshotEntity {
    fn snapshot(&self) -> Arc<AppSnapshot> {
        SnapshotEntity::snapshot(self)
    }
}

/// Entity owning the typed focus tree.
pub struct FocusEntity {
    /// Focus tree data.
    pub tree: FocusTree,
}

/// Entity owning the modal stack.
pub struct ModalEntity {
    /// Modal stack data.
    pub stack: ModalStack,
}

/// Entity owning command palette state.
pub struct PaletteEntity {
    /// Palette data.
    pub state: CommandPaletteState,
}

/// Root UI entity. It owns handles to the other UI-thread entities and has no
/// engine DTO or persistence dependency.
pub struct UiRootEntity {
    runtime: Rc<RefCell<DesktopRuntime>>,
    pending: Vec<Intent>,
    timeline: AnimationTimeline<LiveFrameClock>,
    /// Snapshot entity consumed by views.
    pub snapshot: Entity<SnapshotEntity>,
    /// Focus entity.
    pub focus: Entity<FocusEntity>,
    /// Modal entity.
    pub modals: Entity<ModalEntity>,
    /// Command palette entity.
    pub palette: Entity<PaletteEntity>,
}

impl UiRootEntity {
    /// Dispatches a typed intent through the pure reducer/runtime boundary.
    pub fn dispatch(&mut self, intent: Intent, cx: &mut Context<Self>) {
        let events = self.runtime.borrow_mut().dispatch(intent);
        self.apply_events(events, cx);
    }

    /// Polls background events without blocking the UI thread.
    pub fn poll(&mut self, cx: &mut Context<Self>) {
        let events = self.runtime.borrow_mut().poll();
        self.apply_events(events, cx);
    }

    fn apply_events(
        &mut self,
        events: Vec<super::coordinator::RuntimeEvent>,
        cx: &mut Context<Self>,
    ) {
        for event in events {
            if let super::coordinator::RuntimeEvent::SnapshotChanged(snapshot) = event {
                self.snapshot
                    .update(cx, |entity, _| entity.replace(snapshot));
            }
        }
        cx.notify();
    }
}

impl Render for UiRootEntity {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = std::mem::take(&mut self.pending);
        for intent in pending {
            self.dispatch(intent, cx);
        }
        self.poll(cx);
        let reduced_motion = self.snapshot.read(cx).snapshot().settings().reduced_motion;
        self.timeline.set_reduced_motion(reduced_motion);
        self.timeline.advance();
        window.request_animation_frame();
        let route = shell_route_label(self.snapshot.read(cx));
        div().id("nudox-shell-v3").size_full().child(route)
    }
}

fn shell_route_label(read_model: &impl SnapshotReadModel) -> String {
    format!("{:?}", read_model.snapshot().route().key())
}

/// The complete UI-thread entity graph installed once per window.
pub struct UiEntityGraph {
    /// Root shell entity.
    pub root: Entity<UiRootEntity>,
    /// Immutable snapshot entity.
    pub snapshot: Entity<SnapshotEntity>,
    /// Focus tree entity.
    pub focus: Entity<FocusEntity>,
    /// Modal stack entity.
    pub modals: Entity<ModalEntity>,
    /// Command palette entity.
    pub palette: Entity<PaletteEntity>,
}

impl UiEntityGraph {
    /// Installs the graph on the GPUI/UI thread.
    pub fn install(cx: &mut App, runtime: DesktopRuntime) -> Self {
        let runtime = Rc::new(RefCell::new(runtime));
        let snapshot = cx.new(|_| SnapshotEntity {
            snapshot: runtime.borrow().snapshot(),
        });
        let focus = cx.new(|_| FocusEntity {
            tree: FocusTree::default(),
        });
        let modals = cx.new(|_| ModalEntity {
            stack: ModalStack::default(),
        });
        let palette = cx.new(|_| PaletteEntity {
            state: CommandPaletteState::default(),
        });
        let root = cx.new(|_| UiRootEntity {
            runtime,
            pending: Vec::new(),
            timeline: AnimationTimeline::new(LiveFrameClock::default()),
            snapshot: snapshot.clone(),
            focus: focus.clone(),
            modals: modals.clone(),
            palette: palette.clone(),
        });
        Self {
            root,
            snapshot,
            focus,
            modals,
            palette,
        }
    }
}

impl IntentDispatcher for UiRootEntity {
    fn dispatch(&mut self, intent: Intent) {
        self.pending.push(intent);
    }
}
