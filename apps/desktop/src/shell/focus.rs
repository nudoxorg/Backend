//! The keyboard's model: zones (Tab), targets inside a zone (J/K, ↑↓), and
//! the travelling focus bevel.
//!
//! A region registers the things the keyboard can stand on while it renders
//! ([`Targets::begin`] / [`Targets::push`]) and wraps each one in
//! [`Targets::track`], which records the element's bounds at prepaint. The
//! focused target is drawn by [`FocusGlow`], the region's last child: one
//! doubled periwinkle bevel that springs from the previous target to the next
//! (the bevel light travels), so moving focus re-renders only the region it
//! moves in.

use crate::model::pages::{PageKey, SymbolRef};
use crate::navigation::Route;
use facet::motion::{Motion, spec};
use facet::paint::{Bevel, CutPaint, Edge, paint_cut};
use facet::{ActiveFacet as _, Measure};
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, MouseExitEvent, MouseMoveEvent, Pixels, SharedString, Style,
    Window, point, px, size,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// The keyboard zones, in Tab order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Zone {
    /// The titlebar: shelf toggle, altimeter, thread, here capsule.
    Titlebar,
    /// The shelf (or the spine).
    Shelf,
    /// The page.
    #[default]
    Reader,
    /// The pinned peeks column.
    Pins,
}

impl Zone {
    /// Every zone in Tab order.
    pub const ALL: [Self; 4] = [Self::Titlebar, Self::Shelf, Self::Reader, Self::Pins];
}

/// What a target does when it is activated (Enter, a click, a hint).
pub(crate) type Act = Rc<dyn Fn(&mut Window, &mut App)>;

/// One thing the keyboard can stand on.
#[derive(Clone)]
pub(crate) struct Target {
    /// Stable id within its region.
    pub id: SharedString,
    /// What it reads as (hint mode's announcement, the peek's title).
    pub label: SharedString,
    /// Enter / click / hint.
    pub act: Act,
    /// The page Space peeks, when the target is a link.
    pub peek: Option<PageKey>,
    /// The declaration S peels to source, when the target is one.
    pub source: Option<SymbolRef>,
}

/// What a click leaves behind, and what the keyboard stands on: the focused
/// target and which target left which route. The one owner of that state:
/// [`Targets`] holds a `Recall` and delegates to it, and an action that has to
/// write it captures a clone of it (see [`Targets::recall`]).
///
/// Shared between every clone (not per clone) so a target focused while
/// building one frame's `Ctx` is still focused once that clone is dropped: a
/// body only ever borrows `&Targets`, never `&mut`, so setting focus from a
/// click needs interior mutability here.
#[derive(Clone, Default)]
pub(crate) struct Recall {
    focused: Rc<RefCell<Option<SharedString>>>,
    /// Which target a route was left by (a click, not a key walk): keyed on
    /// the exact route, so Back landing on it again can put the keyboard
    /// back on the row that led away from it. `Reader::arrive` clears
    /// `focused` on every arrival ("a new page starts unfocused"), so this
    /// lives apart from it and survives that clear.
    left_by: Rc<RefCell<HashMap<Route, SharedString>>>,
}

impl Recall {
    /// The focused target's id.
    pub(crate) fn focused(&self) -> Option<SharedString> {
        self.focused.borrow().clone()
    }

    /// Focuses `id` (as [`Targets::focus`]).
    pub(crate) fn focus(&self, id: impl Into<SharedString>) {
        *self.focused.borrow_mut() = Some(id.into());
    }

    /// Forgets the focused target (a new page starts unfocused).
    pub(crate) fn clear_focus(&self) {
        *self.focused.borrow_mut() = None;
    }

    /// Remembers that activating `id` left `route` (as [`Targets::remember_leave`]).
    pub(crate) fn remember_leave(&self, route: Route, id: impl Into<SharedString>) {
        self.left_by.borrow_mut().insert(route, id.into());
    }

    /// The target `route` was left by, when a click (not a key walk) is what
    /// left it.
    pub(crate) fn left_by(&self, route: &Route) -> Option<SharedString> {
        self.left_by.borrow().get(route).cloned()
    }
}

/// The list of a region's targets, in walk order. The region that builds its
/// `Targets` (`named`, `default`) owns the list; every `clone()` of it holds
/// the list WEAKLY. An action stored in the list that captured a clone of the
/// `Targets` would otherwise hold the list that holds the action: a cycle
/// that keeps everything the action captured (the shell's links, the data
/// store) alive past the window (the Library's project tile did exactly that:
/// `fit_tests::the_librarys_project_tile_does_not_keep_its_own_target_list_alive`).
/// With a weak clone that cycle cannot be written.
enum List {
    Owner(Rc<RefCell<Vec<Target>>>),
    Clone(Weak<RefCell<Vec<Target>>>),
}

impl List {
    fn strong(&self) -> Option<Rc<RefCell<Vec<Target>>>> {
        match self {
            Self::Owner(list) => Some(Rc::clone(list)),
            Self::Clone(list) => list.upgrade(),
        }
    }

    fn weak(&self) -> Weak<RefCell<Vec<Target>>> {
        match self {
            Self::Owner(list) => Rc::downgrade(list),
            Self::Clone(list) => list.clone(),
        }
    }

    /// Runs `f` on the list; `None` when its owner is gone (nothing is left
    /// to walk).
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Target>) -> R) -> Option<R> {
        self.strong().map(|list| f(&mut list.borrow_mut()))
    }
}

impl Default for List {
    fn default() -> Self {
        Self::Owner(Rc::default())
    }
}

impl Clone for List {
    fn clone(&self) -> Self {
        Self::Clone(self.weak())
    }
}

/// A region's targets, rebuilt every render, and its focused one.
#[derive(Clone, Default)]
pub(crate) struct Targets {
    /// The region's name: its bevel's tracks are `{name}.glow-x` and so on,
    /// one continuous track per region.
    name: &'static str,
    list: List,
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    /// The focused target and the routes clicks left ([`Recall`]).
    recall: Recall,
    /// The zone is the active one: the glow shows only there.
    active: bool,
    /// Where the bevel was last heading, kept while it comes to rest unseen.
    heading: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// This frame's layout of each target (known before any prepaint, so a
    /// scroll container can bring one into view in the same frame).
    layouts: Rc<RefCell<HashMap<SharedString, LayoutId>>>,
    /// The bevel's own motion store: its liveness is the bevel's alone.
    motion: Motion,
    /// What Esc does on this page while it has something to fold (an open
    /// module): rebuilt every render like the list, and never a clone of
    /// this `Targets` (the same cycle the list guards against).
    escape: Rc<RefCell<Option<Act>>>,
    /// The declaration each target that is one stands for (twins: what a
    /// hovered target lights elsewhere), rebuilt every render.
    sources: Rc<RefCell<HashMap<SharedString, SymbolRef>>>,
}

impl Targets {
    /// The targets of the region called `name`.
    pub(crate) fn named(name: &'static str) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// Starts a render: forgets last frame's list (bounds are kept until the
    /// same ids record again, so a reused prepaint keeps them valid).
    pub(crate) fn begin(&self) {
        let _ = self.list.with(Vec::clear);
        self.layouts.borrow_mut().clear();
        self.escape.borrow_mut().take();
        self.sources.borrow_mut().clear();
    }

    /// The page has something Esc folds (an open module): pressing it does
    /// this, before anything of the shell's own that is not a transient.
    pub(crate) fn on_escape(&self, act: Act) {
        *self.escape.borrow_mut() = Some(act);
    }

    /// What Esc folds on this page, when something is open.
    pub(crate) fn escape(&self) -> Option<Act> {
        self.escape.borrow().clone()
    }

    /// The focused target's layout in this frame, once laid out.
    pub(crate) fn focused_layout(&self) -> Option<LayoutId> {
        if !self.active {
            return None;
        }
        let id = self.recall.focused()?;
        self.layouts.borrow().get(&id).copied()
    }

    /// Registers one target in walk order.
    pub(crate) fn push(&self, target: Target) {
        if let Some(source) = &target.source {
            self.sources.borrow_mut().insert(target.id.clone(), source.clone());
        }
        let _ = self.list.with(|list| list.push(target));
    }

    /// Where the targets that stand for `symbol` were last painted (its
    /// twins in this region).
    pub(crate) fn twins_of(&self, symbol: &SymbolRef) -> Vec<Bounds<Pixels>> {
        let bounds = self.bounds.borrow();
        self.sources
            .borrow()
            .iter()
            .filter(|(_, source)| *source == symbol)
            .filter_map(|(id, _)| bounds.get(id).copied())
            .collect()
    }

    /// Wraps `child` so its bounds are recorded under `id` at prepaint (and
    /// published to the probe ledger as a clickable, focusable target).
    pub(crate) fn track(&self, id: impl Into<SharedString>, child: impl IntoElement) -> Tracked {
        let id = id.into();
        let focused = self.is_focused(&id);
        let source = self.sources.borrow().get(&id).cloned();
        Tracked {
            id,
            focused,
            source,
            target: true,
            bounds: Rc::clone(&self.bounds),
            layouts: Rc::clone(&self.layouts),
            child: child.into_any_element(),
        }
    }

    /// Wraps `child` so its bounds are recorded under `id` at prepaint, and
    /// nothing else: a part of a target (a row's name), not a target itself.
    pub(crate) fn measure(&self, id: impl Into<SharedString>, child: impl IntoElement) -> Tracked {
        Tracked {
            id: id.into(),
            focused: false,
            source: None,
            target: false,
            bounds: Rc::clone(&self.bounds),
            layouts: Rc::clone(&self.layouts),
            child: child.into_any_element(),
        }
    }

    /// The focused target's id.
    pub(crate) fn focused(&self) -> Option<SharedString> {
        self.recall.focused()
    }

    /// Whether `id` is focused in the active zone.
    pub(crate) fn is_focused(&self, id: &str) -> bool {
        self.active && self.recall.focused().as_deref() == Some(id)
    }

    /// Marks this zone active or not; returns whether that changed.
    pub(crate) fn set_active(&mut self, active: bool) -> bool {
        let changed = self.active != active;
        self.active = active;
        changed
    }

    /// Whether this zone has the keyboard.
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }


    /// Forgets the focused target (a new page starts unfocused).
    pub(crate) fn clear_focus(&self) {
        self.recall.clear_focus();
    }

    /// Focuses `id` (a pointer click keeps the keyboard where the pointer
    /// is). `&self`: a body only ever holds `&Targets`, so a click can call
    /// this directly, the same way it already calls `push`/`track`.
    pub(crate) fn focus(&self, id: impl Into<SharedString>) {
        self.recall.focus(id);
    }

    /// Remembers that activating `id` is what left `route` (a click, not a
    /// key walk): [`Self::left_by`] reads this back so Back can put the
    /// keyboard on the same row when it lands on `route` again.
    pub(crate) fn remember_leave(&self, route: Route, id: impl Into<SharedString>) {
        self.recall.remember_leave(route, id);
    }

    /// The two things a click writes when it leaves a page, without the
    /// list. An action stored in [`Self::push`] must capture this and never
    /// a clone of the `Targets`: the list holds the action, so an action
    /// that holds the list is a cycle that keeps everything the action
    /// captured (the shell's links, the store) alive past the window.
    pub(crate) fn recall(&self) -> Recall {
        self.recall.clone()
    }

    /// A probe on the list: dead once every holder of the list has let go.
    #[cfg(test)]
    pub(crate) fn list_probe(&self) -> Weak<RefCell<Vec<Target>>> {
        self.list.weak()
    }

    /// The target `route` was left by, when a click (not a key walk) is
    /// what left it.
    pub(crate) fn left_by(&self, route: &Route) -> Option<SharedString> {
        self.recall.left_by(route)
    }

    /// Moves focus `delta` targets along the last rendered list, clamping at
    /// the ends. With nothing focused, J lands on the first and K on the last.
    /// Returns whether focus moved.
    pub(crate) fn walk(&mut self, delta: isize) -> bool {
        let focused = self.recall.focused();
        let landed = self.list.with(|list| {
            let last = list.len().checked_sub(1)?;
            let next = match focused.as_ref().and_then(|id| list.iter().position(|target| &target.id == id)) {
                Some(index) => index.saturating_add_signed(delta).min(last),
                None if delta >= 0 => 0,
                None => last,
            };
            Some(list[next].id.clone())
        });
        let Some(Some(id)) = landed else { return false };
        let moved = focused.as_ref() != Some(&id);
        self.recall.focus(id);
        moved
    }

    /// The focused target, if it is still on screen.
    pub(crate) fn current(&self) -> Option<Target> {
        let id = self.recall.focused()?;
        self.list.with(|list| list.iter().find(|target| target.id == id).cloned()).flatten()
    }

    /// Every target with its last recorded bounds (hint mode).
    pub(crate) fn placed(&self) -> Vec<(Target, Bounds<Pixels>)> {
        let bounds = self.bounds.borrow();
        self.list
            .with(|list| list.iter().filter_map(|target| bounds.get(&target.id).map(|at| (target.clone(), *at))).collect())
            .unwrap_or_default()
    }

    /// Where target `id` was last painted.
    pub(crate) fn bounds_of(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.bounds.borrow().get(id).copied()
    }

    /// The focused target's bounds, when recorded.
    pub(crate) fn focused_bounds(&self) -> Option<Bounds<Pixels>> {
        let id = self.recall.focused()?;
        self.bounds.borrow().get(&id).copied()
    }

    /// The travelling focus bevel for this region: add it as the region's
    /// last child.
    pub(crate) fn glow(&self, measure: &Measure) -> FocusGlow {
        FocusGlow {
            keys: ["x", "y", "w", "h"].map(|axis| ElementId::Name(format!("{}.glow-{axis}", self.name).into())),
            bounds: Rc::clone(&self.bounds),
            focused: self.recall.focused().filter(|_| self.active),
            heading: Rc::clone(&self.heading),
            motion: self.motion.clone(),
            chamfer: f32::from(measure.space(facet::Space::Base)).max(4.0),
        }
    }
}

/// See [`Targets::track`].
pub(crate) struct Tracked {
    id: SharedString,
    focused: bool,
    /// The declaration it stands for: hovering it lights its twins.
    source: Option<SymbolRef>,
    /// Published to the probe as a target (`track`), or only measured
    /// (`measure`: a part of a target, such as a row's name).
    target: bool,
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    layouts: Rc<RefCell<HashMap<SharedString, LayoutId>>>,
    child: AnyElement,
}

impl IntoElement for Tracked {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Tracked {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let layout = self.child.request_layout(window, cx);
        self.layouts.borrow_mut().insert(self.id.clone(), layout);
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        self.bounds.borrow_mut().insert(self.id.clone(), bounds);
        if !self.target {
            self.child.prepaint(window, cx);
            return None;
        }
        facet::probe::record_target(
            cx,
            &ElementId::Name(self.id.clone()),
            bounds,
            facet::probe::Target {
                hovered: false,
                pressed: false,
                focused: self.focused,
                focusable: true,
                clickable: true,
            },
        );
        self.child.prepaint(window, cx);
        self.source.as_ref().map(|_| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        // Twins: the pointer on a declaration lights that declaration
        // everywhere it stands (`side::twin`), and leaving puts it out.
        if let (Some(source), Some(hitbox)) = (self.source.clone(), hitbox.clone()) {
            let moved_source = source.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble {
                    if hitbox.is_hovered(window) {
                        super::side::twin::light(Some(moved_source.clone()), cx);
                    } else {
                        super::side::twin::put_out(&moved_source, cx);
                    }
                }
            });
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble {
                    super::side::twin::put_out(&source, cx);
                }
            });
        }
    }
}

/// The focus bevel. It fills its parent absolutely, reads the focused
/// target's bounds recorded earlier in the same prepaint, springs its rect
/// towards them, and paints one doubled periwinkle bevel with no fill.
///
/// When its zone loses focus (Esc, Tab away) it stops painting at once but
/// keeps moving, unseen, to where it was heading until it rests: a track is
/// never left mid-flight, and focus returning springs on from there.
pub(crate) struct FocusGlow {
    keys: [ElementId; 4],
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    focused: Option<SharedString>,
    heading: Rc<Cell<Option<Bounds<Pixels>>>>,
    motion: Motion,
    chamfer: f32,
}

impl IntoElement for FocusGlow {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for FocusGlow {
    type RequestLayoutState = ();
    type PrepaintState = Option<Bounds<Pixels>>;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = gpui::Position::Absolute;
        style.inset.top = px(0.0).into();
        style.inset.left = px(0.0).into();
        style.size.width = px(0.0).into();
        style.size.height = px(0.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>> {
        let shown = self
            .focused
            .as_ref()
            .and_then(|id| self.bounds.borrow().get(id).copied());
        let target = match shown {
            Some(target) => {
                self.heading.set(Some(target));
                target
            }
            None => self.heading.get()?,
        };
        let [kx, ky, kw, kh] = self.keys.clone();
        let x = self.motion.animate(kx, f32::from(target.origin.x), spec::FOLLOW, window, cx);
        let y = self.motion.animate(ky, f32::from(target.origin.y), spec::FOLLOW, window, cx);
        let w = self.motion.animate(kw, f32::from(target.size.width), spec::FOLLOW, window, cx);
        let h = self.motion.animate(kh, f32::from(target.size.height), spec::FOLLOW, window, cx);
        let rect = Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0))));
        if shown.is_none() {
            // Unseen: once the spring itself is at rest (not merely drawn at
            // its target) there is nothing left to sample until focus returns.
            if !self.motion.is_live(cx) {
                self.heading.set(None);
            }
            return None;
        }
        Some(rect)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        rect: &mut Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(rect) = *rect else {
            return;
        };
        let palette = cx.facet().palette();
        let mut paint = CutPaint::new(palette);
        paint.chamfer = self.chamfer;
        paint.edge = Edge::of(Bevel::Focus, palette);
        paint.fill = Some(gpui::transparent_black());
        paint_cut(window, rect, &paint, palette);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(id: &'static str, act: Act) -> Target {
        Target { id: id.into(), label: id.into(), act, peek: None, source: None }
    }

    fn nothing() -> Act {
        Rc::new(|_, _| {})
    }

    /// The Library's project tile once stored an action in its region's list
    /// that captured a clone of the region's `Targets`: the list held the
    /// action and the action held the list, and with them the shell's links
    /// and the data store, past the window. A clone holds the list weakly,
    /// so that cycle cannot be written: dropping the region's own `Targets`
    /// frees the list even while an action inside it holds a clone.
    #[test]
    fn an_action_that_holds_a_clone_of_its_own_targets_does_not_keep_the_list_alive() {
        let targets = Targets::named("region");
        let held = targets.clone();
        targets.push(target("row", Rc::new(move |_, _| held.focus("row"))));
        let probe = targets.list_probe();
        assert!(probe.upgrade().is_some(), "the region's list is alive while the region is");
        drop(targets);
        assert!(probe.upgrade().is_none(), "the action's clone kept the region's list alive: a cycle");
    }

    #[test]
    fn a_clone_walks_and_pushes_the_list_of_the_targets_it_was_cloned_from() {
        let targets = Targets::named("region");
        let mut clone = targets.clone();
        clone.push(target("a", nothing()));
        targets.push(target("b", nothing()));
        assert!(clone.walk(1), "J from nothing lands on the first target");
        assert_eq!(targets.focused().as_deref(), Some("a"), "focus is shared between the clones");
        assert!(clone.walk(1));
        assert_eq!(targets.current().map(|found| found.id), Some("b".into()));
        assert!(!clone.walk(1), "the end of the list clamps");
        // Once the region is gone a clone has nothing to walk and says so.
        drop(targets);
        assert!(!clone.walk(1));
        assert!(clone.current().is_none());
    }

    #[test]
    fn what_a_click_leaves_behind_is_written_through_a_recall_and_read_from_the_targets() {
        let targets = Targets::named("region");
        let recall = targets.recall();
        recall.focus("row");
        assert_eq!(targets.focused().as_deref(), Some("row"));
        recall.remember_leave(Route::World, "row");
        assert_eq!(targets.left_by(&Route::World).as_deref(), Some("row"));
        targets.clear_focus();
        assert!(recall.focused().is_none(), "a new page starts unfocused, through either handle");
        assert_eq!(recall.left_by(&Route::World).as_deref(), Some("row"), "and the leave survives that clear");
    }
}
