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
use facet::motion::{Motion, spec};
use facet::paint::{Bevel, CutPaint, Edge, paint_cut};
use facet::{ActiveFacet as _, Measure};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Style, Window, point, px, size,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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

/// A region's targets, rebuilt every render, and its focused one.
#[derive(Clone, Default)]
pub(crate) struct Targets {
    list: Rc<RefCell<Vec<Target>>>,
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    focused: Option<SharedString>,
    /// The zone is the active one: the glow shows only there.
    active: bool,
}

impl Targets {
    /// Starts a render: forgets last frame's list (bounds are kept until the
    /// same ids record again, so a reused prepaint keeps them valid).
    pub(crate) fn begin(&self) {
        self.list.borrow_mut().clear();
    }

    /// Registers one target in walk order.
    pub(crate) fn push(&self, target: Target) {
        self.list.borrow_mut().push(target);
    }

    /// Wraps `child` so its bounds are recorded under `id` at prepaint.
    pub(crate) fn track(&self, id: impl Into<SharedString>, child: impl IntoElement) -> Tracked {
        Tracked {
            id: id.into(),
            bounds: Rc::clone(&self.bounds),
            child: child.into_any_element(),
        }
    }

    /// The focused target's id.
    pub(crate) fn focused(&self) -> Option<&SharedString> {
        self.focused.as_ref()
    }

    /// Whether `id` is focused in the active zone.
    pub(crate) fn is_focused(&self, id: &str) -> bool {
        self.active && self.focused.as_deref() == Some(id)
    }

    /// Marks this zone active or not; returns whether that changed.
    pub(crate) fn set_active(&mut self, active: bool) -> bool {
        let changed = self.active != active;
        self.active = active;
        changed
    }

    /// Whether this zone is the active one.
    pub(crate) const fn is_active(&self) -> bool {
        self.active
    }

    /// Forgets the focused target (a new page starts unfocused).
    pub(crate) fn clear_focus(&mut self) {
        self.focused = None;
    }

    /// Focuses `id` (a pointer click keeps the keyboard where the pointer is).
    pub(crate) fn focus(&mut self, id: impl Into<SharedString>) {
        self.focused = Some(id.into());
    }

    /// Moves focus `delta` targets along the last rendered list, clamping at
    /// the ends. With nothing focused, J lands on the first and K on the last.
    /// Returns whether focus moved.
    pub(crate) fn walk(&mut self, delta: isize) -> bool {
        let list = self.list.borrow();
        if list.is_empty() {
            return false;
        }
        let last = list.len() - 1;
        let next = match self
            .focused
            .as_ref()
            .and_then(|id| list.iter().position(|target| &target.id == id))
        {
            Some(index) => index.saturating_add_signed(delta).min(last),
            None if delta >= 0 => 0,
            None => last,
        };
        let id = list[next].id.clone();
        drop(list);
        let moved = self.focused.as_ref() != Some(&id);
        self.focused = Some(id);
        moved
    }

    /// The focused target, if it is still on screen.
    pub(crate) fn current(&self) -> Option<Target> {
        let id = self.focused.as_ref()?;
        self.list.borrow().iter().find(|target| &target.id == id).cloned()
    }

    /// Every target with its last recorded bounds (hint mode).
    pub(crate) fn placed(&self) -> Vec<(Target, Bounds<Pixels>)> {
        let bounds = self.bounds.borrow();
        self.list
            .borrow()
            .iter()
            .filter_map(|target| bounds.get(&target.id).map(|at| (target.clone(), *at)))
            .collect()
    }

    /// The focused target's bounds, when recorded.
    pub(crate) fn focused_bounds(&self) -> Option<Bounds<Pixels>> {
        let id = self.focused.as_ref()?;
        self.bounds.borrow().get(id).copied()
    }

    /// The travelling focus bevel for this region: add it as the region's
    /// last child.
    pub(crate) fn glow(&self, motion: &Motion, measure: &Measure) -> FocusGlow {
        FocusGlow {
            bounds: Rc::clone(&self.bounds),
            focused: self.focused.clone().filter(|_| self.active),
            motion: motion.clone(),
            chamfer: f32::from(measure.space(facet::Space::Base)).max(4.0),
        }
    }
}

/// See [`Targets::track`].
pub(crate) struct Tracked {
    id: SharedString,
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
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
    type PrepaintState = ();

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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.bounds.borrow_mut().insert(self.id.clone(), bounds);
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// The focus bevel. It fills its parent absolutely, reads the focused
/// target's bounds recorded earlier in the same prepaint, springs its rect
/// towards them, and paints one doubled periwinkle bevel with no fill.
pub(crate) struct FocusGlow {
    bounds: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    focused: Option<SharedString>,
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
        let target = self
            .focused
            .as_ref()
            .and_then(|id| self.bounds.borrow().get(id).copied());
        let target = target?;
        let x = self.motion.animate("glow-x", f32::from(target.origin.x), spec::FOLLOW, window, cx);
        let y = self.motion.animate("glow-y", f32::from(target.origin.y), spec::FOLLOW, window, cx);
        let w = self.motion.animate("glow-w", f32::from(target.size.width), spec::FOLLOW, window, cx);
        let h = self.motion.animate("glow-h", f32::from(target.size.height), spec::FOLLOW, window, cx);
        Some(Bounds::new(point(px(x), px(y)), size(px(w.max(0.0)), px(h.max(0.0)))))
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
