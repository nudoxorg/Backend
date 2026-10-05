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
    AnyElement, App, Bounds, ClickEvent, DispatchPhase, Element, ElementId, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement, IntoElement,
    LayoutId, MouseExitEvent, MouseMoveEvent, Pixels, SharedString, StatefulInteractiveElement,
    Style, Window, div, point, px, size,
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

/// One target's activation and the admission that owns both native focus and
/// the action. A hint checks the same receipt before moving focus and again
/// when it invokes the callback; producers supply their existing guard.
#[derive(Clone)]
pub(crate) struct TargetAction {
    admit: Rc<dyn Fn(&mut App) -> bool>,
    act: Act,
}

impl TargetAction {
    pub(crate) fn new(admit: Rc<dyn Fn(&mut App) -> bool>, act: Act) -> Self {
        Self { admit, act }
    }

    pub(crate) fn admits(&self, cx: &mut App) -> bool {
        (self.admit)(cx)
    }

    pub(crate) fn run(&self, window: &mut Window, cx: &mut App) {
        if self.admits(cx) { (self.act)(window, cx); }
    }

    pub(crate) fn callback(&self) -> Act {
        let binding = self.clone();
        Rc::new(move |window, cx| binding.run(window, cx))
    }
}

/// A native leaf reports itself. Only a logical selection represented by
/// its focused ancestor may advertise an active descendant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FocusRepresentative {
    Native,
    Descendant,
    None,
}
impl FocusRepresentative {
    pub(crate) fn for_leaf(selected: bool, handle: &FocusHandle, window: &Window) -> Self {
        if handle.is_focused(window) {
            Self::Native
        } else if selected {
            Self::Descendant
        } else {
            Self::None
        }
    }
}

/// A raw Reader target's native semantics and activation. The caller owns
/// visual styling and the existing target list owns order and hints. Facet
/// buttons keep their own native control implementation.
pub(crate) fn native_control(
    id: SharedString,
    label: impl Into<SharedString>,
    role: gpui::Role,
    handle: Option<FocusHandle>,
    act: Act,
) -> gpui::Stateful<gpui::Div> {
    let control = div()
        .id(id)
        .role(role)
        .aria_label(label.into())
        .key_context(crate::shell::keys::NATIVE_CONTROL);
    let control = if let Some(handle) = handle {
        facet::controls::button::native_button(control, &handle, move |window, cx| act(window, cx))
    } else {
        control.on_click(move |_: &ClickEvent, window, cx| act(window, cx))
    };
    control
}

/// One thing the keyboard can stand on.
#[derive(Clone)]
pub(crate) struct Target {
    /// Stable id within its region.
    pub id: SharedString,
    /// What it reads as (hint mode's announcement, the peek's title).
    pub label: SharedString,
    /// Enter / click / hint.
    pub action: TargetAction,
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
    /// One live departing target, consumed into the current visit before
    /// navigation. This is transition evidence, never a second route history.
    left_by: Rc<RefCell<Option<(Route, SharedString)>>>,
    /// Pure current-visit focus projection; history lives only in SessionState.
    reading_return: Rc<RefCell<Option<(Route, Option<crate::navigation::presentation::ReadingFocus>)>>>,
    reading_visit: Rc<Cell<Option<crate::navigation::presentation::VisitId>>>,
}

impl Recall {
    /// The focused target's id.
    pub(crate) fn focused(&self) -> Option<SharedString> {
        self.focused.borrow().clone()
    }

    /// Focuses `id` (as [`Targets::focus`]).
    pub(crate) fn focus(&self, id: impl Into<SharedString>) {
        let id = id.into();
        if self.left_by.borrow().as_ref().is_some_and(|(_, leaving)| leaving != &id) { self.left_by.borrow_mut().take(); }
        *self.focused.borrow_mut() = Some(id);
    }

    /// Forgets the focused target (a new page starts unfocused).
    pub(crate) fn clear_focus(&self) {
        *self.focused.borrow_mut() = None;
    }

    /// Remembers that activating `id` left `route` (as [`Targets::remember_leave`]).
    pub(crate) fn remember_leave(&self, route: Route, id: impl Into<SharedString>) {
        let id = id.into();
        if crate::navigation::presentation::ReadingText::new(id.to_string()).is_some() {
            self.focus(id.clone());
            *self.left_by.borrow_mut() = Some((route, id));
        }
    }

    /// The target `route` was left by, when a click (not a key walk) is what
    /// left it.
    pub(crate) fn left_by(&self, route: &Route) -> Option<SharedString> {
        if let Some((bound, focus)) = self.reading_return.borrow().as_ref()
            && bound == route {
            return match focus {
                Some(crate::navigation::presentation::ReadingFocus::Reader(key)) => Some(key.as_str().to_owned().into()),
                _ => None,
            };
        }
        self.left_by.borrow().as_ref().filter(|(left, _)| left == route).map(|(_, id)| id.clone())
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
    /// A new page arrived ([`Targets::new_page`]): the bevel is born on the
    /// next target focused, not flown there from the last page's.
    fresh: Rc<Cell<bool>>,
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
    /// Native focus for controls in this same target list. Only handles
    /// registered by the current mounted frame can receive keyboard focus.
    native: Rc<RefCell<NativeTargets>>,
}

#[derive(Default)]
struct NativeTargets {
    frame: u64,
    handles: HashMap<SharedString, (FocusHandle, u64)>,
}

impl NativeTargets {
    fn target_for(&self, origin: &FocusHandle) -> Option<SharedString> {
        self.handles.iter().find_map(|(id, (handle, seen))| {
            (*seen == self.frame && handle == origin).then(|| id.clone())
        })
    }
}

/// Identity-only receipt for controls a covering page retired. It owns no
/// callbacks and can only name an origin; returning resolves that name against
/// the destination's newly registered handles.
pub(crate) struct NativeFocusDeparture(NativeTargets);

impl NativeFocusDeparture {
    pub(crate) fn target_for(&self, origin: &FocusHandle) -> Option<SharedString> {
        self.0.target_for(origin)
    }
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
        let mut native = self.native.borrow_mut();
        native.frame = native.frame.wrapping_add(1);
        drop(native);
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
            self.sources
                .borrow_mut()
                .insert(target.id.clone(), source.clone());
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

    pub(crate) fn bind_reading(&self, route: Route, visit: crate::navigation::presentation::VisitId, focus: Option<crate::navigation::presentation::ReadingFocus>) {
        if self.recall.reading_visit.replace(Some(visit)) != Some(visit) { self.recall.left_by.borrow_mut().take(); }
        *self.recall.reading_return.borrow_mut() = Some((route, focus));
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

    /// A new page arrived: it starts unfocused, and the bevel it shows next
    /// is a new one, born on its target (Back restoring the row a page was
    /// left by must not fly the bevel in from the other page).
    pub(crate) fn new_page(&self) {
        self.recall.clear_focus();
        self.native.borrow_mut().handles.clear();
        self.fresh.set(true);
    }

    pub(crate) fn take_native_departure(&self) -> NativeFocusDeparture {
        let mut native = self.native.borrow_mut();
        NativeFocusDeparture(NativeTargets {
            frame: native.frame,
            handles: std::mem::take(&mut native.handles),
        })
    }

    pub(crate) fn target_for_native_handle(&self, origin: &FocusHandle) -> Option<SharedString> {
        self.native.borrow().target_for(origin)
    }

    /// Attach a real GPUI focus handle to a target already in this frame's
    /// list. Reorders preserve the handle; unmounted rows are discarded at
    /// the end of the frame so old inventory pages cannot own input.
    /// An actual control declared during render may build its native leaf in
    /// RenderOnce later. Mark its existing owner now, before finish_native,
    /// so the same mounted key does not lose focus on every parent repaint.
    pub(crate) fn reuse_native_handle(&self, id: &SharedString) -> Option<FocusHandle> {
        let mut native = self.native.borrow_mut();
        let frame = native.frame;
        let (handle, seen) = native.handles.get_mut(id)?;
        *seen = frame;
        Some(handle.clone())
    }

    pub(crate) fn native_handle(&self, id: &SharedString, cx: &mut App) -> FocusHandle {
        let mut native = self.native.borrow_mut();
        let frame = native.frame;
        let (handle, seen) = native
            .handles
            .entry(id.clone())
            .or_insert_with(|| (cx.focus_handle().tab_stop(true), frame));
        *seen = frame;
        handle.clone()
    }

    /// A native editor already owns its GPUI handle. Bind that same handle
    /// to the Reader target so Tab, target walking, and text input agree on
    /// one focus owner; do not create a second handle for its outer plate.
    pub(crate) fn bind_native_handle(&self, id: &SharedString, handle: FocusHandle) {
        let mut native = self.native.borrow_mut();
        let frame = native.frame;
        native.handles.insert(id.clone(), (handle, frame));
    }

    /// A hint can focus only the exact target list frame that supplied it.
    /// Reader redraws may reuse ids while replacing actions and evidence.
    pub(crate) fn hint_frame(&self) -> u64 {
        self.native.borrow().frame
    }

    pub(crate) fn admits_hint(&self, id: &str, frame: u64) -> bool {
        self.hint_frame() == frame
            && self.list.with(|list| list.iter().any(|target| target.id == id)).unwrap_or(false)
    }

    /// Virtualized regions retire offscreen native owners before constructing
    /// the next visible range. Handles remain alive in GPUI for that paint,
    /// but cannot become a later return/input claim through this registry.
    pub(crate) fn retain_native_handles(&self, keep: impl Fn(&str) -> bool) {
        self.native.borrow_mut().handles.retain(|key, _| keep(key));
    }

    pub(crate) fn finish_native(&self) {
        let mut native = self.native.borrow_mut();
        let frame = native.frame;
        native.handles.retain(|_, (_, seen)| *seen == frame);
    }

    fn native_order(&self) -> Vec<(SharedString, FocusHandle)> {
        let native = self.native.borrow();
        self.list
            .with(|list| {
                list.iter()
                    .filter_map(|target| {
                        native
                            .handles
                            .get(&target.id)
                            .filter(|(_, seen)| *seen == native.frame)
                            .map(|(handle, _)| (target.id.clone(), handle.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The next mounted native control in the reader. The list itself owns
    /// walk order and actions; this only gives that target native focus.
    pub(crate) fn native_step(&self, forward: bool, window: &mut Window, cx: &mut App) -> bool {
        let order = self.native_order();
        // AccessKit and input controls can change GPUI focus without walking
        // our logical target list. The mounted handle is the input origin;
        // Recall follows it, never the other way around.
        let current = order
            .iter()
            .position(|(_, handle)| handle.is_focused(window));
        if let Some(at) = current {
            self.focus(order[at].0.clone());
        }
        let next = match current {
            Some(at) if forward => at.checked_add(1).filter(|at| *at < order.len()),
            Some(at) => at.checked_sub(1),
            None if forward => (!order.is_empty()).then_some(0),
            None => order.len().checked_sub(1),
        };
        let Some((id, handle)) = next.and_then(|at| order.get(at)) else {
            return false;
        };
        self.focus(id.clone());
        handle.focus(window, cx);
        true
    }

    pub(crate) fn focus_native(&self, id: &str, window: &mut Window, cx: &mut App) -> bool {
        let Some((_, handle)) = self.native_order().into_iter().find(|(key, _)| key == id) else {
            return false;
        };
        handle.focus(window, cx);
        true
    }

    pub(crate) fn contains_native_handle(&self, handle: &FocusHandle) -> bool {
        self.native_order()
            .iter()
            .any(|(_, mounted)| mounted == handle)
    }

    pub(crate) fn native_focused(&self, window: &Window) -> Option<SharedString> {
        self.native_order()
            .into_iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(id, _)| id)
    }

    #[cfg(test)]
    pub(crate) fn native_keys(&self) -> Vec<SharedString> {
        self.native_order().into_iter().map(|(id, _)| id).collect()
    }

    #[cfg(test)]
    pub(crate) fn focused_native_is_live(&self, window: &Window) -> bool {
        self.focused().is_some_and(|id| {
            self.native_order()
                .into_iter()
                .any(|(key, handle)| key == id && handle.is_focused(window))
        })
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

    /// Only the live leaving gesture, never a route-keyed history lookup.
    pub(crate) fn leaving_focus(&self, route: &Route) -> Option<SharedString> {
        self.recall.left_by.borrow().as_ref().filter(|(left, _)| left == route).map(|(_, id)| id.clone())
    }


    /// Moves focus `delta` targets along the last rendered list, clamping at
    /// the ends. With nothing focused, J lands on the first and K on the last.
    /// Returns whether focus moved.
    pub(crate) fn walk(&mut self, delta: isize) -> bool {
        let focused = self.recall.focused();
        let landed = self.list.with(|list| {
            let last = list.len().checked_sub(1)?;
            let next = match focused
                .as_ref()
                .and_then(|id| list.iter().position(|target| &target.id == id))
            {
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
        self.list
            .with(|list| list.iter().find(|target| target.id == id).cloned())
            .flatten()
    }

    /// Every target with its last recorded bounds (hint mode).
    pub(crate) fn placed(&self) -> Vec<(Target, Bounds<Pixels>)> {
        let bounds = self.bounds.borrow();
        self.list
            .with(|list| {
                list.iter()
                    .filter_map(|target| bounds.get(&target.id).map(|at| (target.clone(), *at)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where target `id` was last painted.
    pub(crate) fn bounds_of(&self, id: &str) -> Option<Bounds<Pixels>> {
        self.bounds.borrow().get(id).copied()
    }

    /// The focused target's bounds, when recorded.
    pub(crate) fn focused_bounds(&self) -> Option<Bounds<Pixels>> {
        let target = self.current()?;
        self.bounds.borrow().get(&target.id).copied()
    }

    /// The travelling focus bevel for this region: add it as the region's
    /// last child.
    pub(crate) fn glow(&self, measure: &Measure) -> FocusGlow {
        FocusGlow {
            keys: ["x", "y", "w", "h"]
                .map(|axis| ElementId::Name(format!("{}.glow-{axis}", self.name).into())),
            bounds: Rc::clone(&self.bounds),
            targets: self.list.clone(),
            focused: self.recall.focused().filter(|_| self.active),
            heading: Rc::clone(&self.heading),
            fresh: Rc::clone(&self.fresh),
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
        // `bounds` is the child's layout-space rectangle. Reader transitions
        // and shared rows may paint under a layer transform; the glow, hint
        // labels, and peek anchors live in window space alongside hitboxes.
        // GPUI applies this same transform inside `insert_hitbox` below.
        let painted = window.layer_transform().apply_bounds(bounds);
        self.bounds.borrow_mut().insert(self.id.clone(), painted);
        if !self.target {
            self.child.prepaint(window, cx);
            return None;
        }
        facet::probe::record_target_in(
            cx,
            &ElementId::Name(self.id.clone()),
            painted,
            facet::probe::Target {
                hovered: false,
                pressed: false,
                focused: self.focused,
                focusable: true,
                clickable: true,
            },
            window,
        );
        self.child.prepaint(window, cx);
        self.source
            .as_ref()
            .map(|_| window.insert_hitbox(bounds, HitboxBehavior::Normal))
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
    targets: List,
    heading: Rc<Cell<Option<Bounds<Pixels>>>>,
    fresh: Rc<Cell<bool>>,
    motion: Motion,
    chamfer: f32,
}

impl FocusGlow {
    /// Recalled bounds are geometry, not proof that a target is still mounted.
    /// The weak list also revokes a cached glow when its region is dropped.
    fn shown_bounds(&self) -> Option<Bounds<Pixels>> {
        let id = self.focused.as_ref()?;
        self.targets
            .with(|targets| targets.iter().any(|target| &target.id == id))?
            .then(|| self.bounds.borrow().get(id).copied())
            .flatten()
    }

    /// The bevel was put away (unseen, and at rest), or a new page arrived:
    /// it comes back on its target as a new bevel, never flying in from where
    /// it was last seen (on another page, before a Back). The probe is told
    /// it is a designed start, not a step from that old place.
    fn born(&self, target: Bounds<Pixels>, cx: &mut App) {
        let values = [
            target.origin.x,
            target.origin.y,
            target.size.width,
            target.size.height,
        ]
        .map(f32::from);
        for (key, value) in self.keys.iter().zip(values) {
            self.motion.set(key.clone(), value);
            if facet::probe::enabled(cx) {
                let at_ms = facet::motion::now(cx)
                    .saturating_duration_since(facet::motion::epoch(cx))
                    .as_secs_f64()
                    * 1000.0;
                facet::probe::record_track(cx, || facet::probe::TrackSample {
                    key: key.to_string(),
                    kind: facet::probe::TrackKind::Snap,
                    value,
                    target: value,
                    velocity: 0.0,
                    started_ms: at_ms,
                    budget_ms: 0.0,
                    at_ms,
                    live: false,
                    overshoot_ratio: 0.0,
                    overshoot_absolute: 0.0,
                    group: None,
                });
            }
        }
    }
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
        let shown = self.shown_bounds();
        let target = match shown {
            Some(target) => {
                if self.heading.get().is_none() || self.fresh.get() {
                    self.fresh.set(false);
                    self.born(target, cx);
                }
                self.heading.set(Some(target));
                target
            }
            None => self.heading.get()?,
        };
        let [kx, ky, kw, kh] = self.keys.clone();
        let x = self
            .motion
            .animate(kx, f32::from(target.origin.x), spec::FOLLOW, window, cx);
        let y = self
            .motion
            .animate(ky, f32::from(target.origin.y), spec::FOLLOW, window, cx);
        let w = self
            .motion
            .animate(kw, f32::from(target.size.width), spec::FOLLOW, window, cx);
        let h = self
            .motion
            .animate(kh, f32::from(target.size.height), spec::FOLLOW, window, cx);
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
        Target {
            id: id.into(),
            label: id.into(),
            action: TargetAction::new(Rc::new(|_| true), act),
            peek: None,
            source: None,
        }
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
        assert!(
            probe.upgrade().is_some(),
            "the region's list is alive while the region is"
        );
        drop(targets);
        assert!(
            probe.upgrade().is_none(),
            "the action's clone kept the region's list alive: a cycle"
        );
    }

    #[test]
    fn a_clone_walks_and_pushes_the_list_of_the_targets_it_was_cloned_from() {
        let targets = Targets::named("region");
        let mut clone = targets.clone();
        clone.push(target("a", nothing()));
        targets.push(target("b", nothing()));
        assert!(clone.walk(1), "J from nothing lands on the first target");
        assert_eq!(
            targets.focused().as_deref(),
            Some("a"),
            "focus is shared between the clones"
        );
        assert!(clone.walk(1));
        assert_eq!(targets.current().map(|found| found.id), Some("b".into()));
        assert!(!clone.walk(1), "the end of the list clamps");
        // Once the region is gone a clone has nothing to walk and says so.
        drop(targets);
        assert!(!clone.walk(1));
        assert!(clone.current().is_none());
    }

    #[test]
    fn current_visit_focus_projection_does_not_borrow_an_older_same_route_departure() {
        use crate::navigation::presentation::{ReadingFocus, ReadingText, VisitId};
        let targets = Targets::named("reader");
        targets.remember_leave(Route::World, "old-row");
        targets.bind_reading(Route::World, VisitId::default(), None);
        assert!(targets.left_by(&Route::World).is_none());
        targets.bind_reading(Route::World, VisitId::default(), Some(ReadingFocus::Reader(ReadingText::new("saved-row").expect("bounded key"))));
        assert_eq!(targets.left_by(&Route::World).as_deref(), Some("saved-row"));
        targets.bind_reading(Route::World, VisitId::default(), Some(ReadingFocus::Shelf(ReadingText::new("shelf-row").expect("bounded key"))));
        assert!(targets.left_by(&Route::World).is_none(), "a Shelf claim cannot regain Reader focus");
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
        assert!(
            recall.focused().is_none(),
            "a new page starts unfocused, through either handle"
        );
        assert_eq!(
            recall.left_by(&Route::World).as_deref(),
            Some("row"),
            "and the leave survives that clear"
        );
    }

    #[test]
    fn cached_focus_geometry_requires_a_current_target_and_live_region() {
        let mut targets = Targets::named("shelf");
        targets.set_active(true);
        targets.push(target("row", nothing()));
        targets.focus("row");
        let at = Bounds::new(point(px(123.0), px(479.0)), size(px(217.0), px(58.0)));
        targets.bounds.borrow_mut().insert("row".into(), at);
        let measure = Measure::new(px(528.0), &facet::Facet::default());
        let cached = targets.glow(&measure);
        assert_eq!(cached.shown_bounds(), Some(at));
        assert_eq!(targets.focused_bounds(), Some(at));
        targets.begin();
        assert!(
            cached.shown_bounds().is_none(),
            "recalled row geometry cannot paint in the collapsed shelf"
        );
        assert!(targets.focused_bounds().is_none());
        targets.push(target("different-row", nothing()));
        assert!(
            cached.shown_bounds().is_none(),
            "the new current row cannot inherit another row's rectangle"
        );
        targets.push(target("row", nothing()));
        assert_eq!(
            cached.shown_bounds(),
            Some(at),
            "a reused prepaint keeps the admitted row's translated geometry"
        );
        drop(targets);
        assert!(
            cached.shown_bounds().is_none(),
            "the glow's weak list cannot retain a dead region"
        );
    }

    #[gpui::test]
    fn mounted_reader_target_geometry_does_not_survive_target_retirement(
        cx: &mut gpui::TestAppContext,
    ) {
        let mut rig = crate::shell::tests::rig(cx, None, 1440.0, 900.0);
        rig.settle();
        rig.repaint();
        let mut targets = rig
            .shell
            .read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        let (target, painted) = targets
            .placed()
            .into_iter()
            .find(|(target, bounds)| {
                target.id.starts_with("orbit-package-")
                    && bounds.size.width > px(0.0)
                    && bounds.size.height > px(0.0)
            })
            .expect("real mounted Library package target");
        targets.set_active(true);
        targets.focus(target.id.clone());
        let measure = Measure::new(px(1440.0), &facet::Facet::default());
        let cached = targets.glow(&measure);
        assert_eq!(targets.focused_bounds(), Some(painted));
        assert_eq!(
            cached.shown_bounds(),
            Some(painted),
            "glow uses the actual translated prepaint rectangle"
        );
        targets.begin();
        assert!(
            cached.shown_bounds().is_none(),
            "cached geometry cannot admit a retired painted target"
        );
    }

    #[gpui::test]
    fn lazy_native_control_reuses_its_owner_before_frame_retirement(cx: &mut gpui::TestAppContext) {
        let mut rig = crate::shell::tests::rig(cx, None, 1440.0, 900.0);
        rig.settle();
        let targets = Targets::named("lazy-native-control");
        let id: SharedString = "owned-control".into();
        targets.begin();
        targets.push(target("owned-control", nothing()));
        let original = rig.cx.update(|window, cx| {
            let handle = targets.native_handle(&id, cx);
            window.focus(&handle, cx);
            handle
        });
        targets.finish_native();
        targets.begin();
        targets.push(target("owned-control", nothing()));
        // The host declares the control before Reader retires unseen handles;
        // the child RenderOnce then receives this very same native owner.
        let reused = targets
            .reuse_native_handle(&id)
            .expect("same current control");
        targets.finish_native();
        assert_eq!(reused, original);
        assert!(
            rig.cx.update(|window, _| reused.is_focused(window)),
            "a parent repaint does not churn the child's native focus generation"
        );
        targets.begin();
        targets.finish_native();
        assert!(
            targets.reuse_native_handle(&id).is_none(),
            "an undeclared control does not retain native input ownership"
        );
    }
}
