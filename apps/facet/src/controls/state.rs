//! The one piece of per-instance memory every control shares: hover, press,
//! the keyboard press, the hover-enter count (for the one-shot sweep), the
//! layout frame, and a stable [`FocusHandle`]. It is persisted across
//! renders by element id through [`Window::use_keyed_state`], so a fresh
//! builder every render finds the same identity; its animated values live in
//! a [`Motion`] store scoped to that state entity, so a control that is
//! unmounted and mounted again starts fresh instead of replaying.
//!
//! Hover is *reconciled*, not just event-driven: [`HoverZone`] checks the
//! pointer against the plate's cut outline in every painted frame and
//! corrects the stored flag when layout moved under a still pointer (a
//! resize, a text-scale change, a list making room). That is what keeps a
//! storm from leaving a control stuck lit.

use crate::motion::Motion;
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, Entity, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseMoveEvent, Pixels, Point, Window,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// An appearance pinned from outside, merged (OR) with the live state: a
/// control whose key was just pressed shows its press, a keyboard-active
/// menu row shows its hover, and a design-system sheet can show every state
/// side by side.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct Look {
    /// Shown as hovered.
    pub hover: bool,
    /// Shown as pressed.
    pub press: bool,
    /// Shown with the keyboard focus bevel.
    pub focus: bool,
}

impl Look {
    /// Nothing pinned: the live state alone.
    pub const LIVE: Self = Self {
        hover: false,
        press: false,
        focus: false,
    };
    /// Pinned hovered.
    pub const HOVER: Self = Self {
        hover: true,
        press: false,
        focus: false,
    };
    /// Pinned pressed (and hovered: a press happens under the pointer).
    pub const PRESS: Self = Self {
        hover: true,
        press: true,
        focus: false,
    };
    /// Pinned keyboard-focused.
    pub const FOCUS: Self = Self {
        hover: false,
        press: false,
        focus: true,
    };
}

/// Hover, press, the focus handle and the layout frame of one control.
pub(crate) struct Interact {
    pub focus: FocusHandle,
    pub hovered: bool,
    pub pressed: bool,
    /// Held by Enter/Space: pressed until the key comes up.
    pub key_pressed: bool,
    /// How many times the pointer has entered (drives the one-shot sweep).
    pub enters: u32,
    /// The hovered sub-item (segment, tick, stone), if any.
    pub hot_item: Option<usize>,
    /// Where the control was laid out last frame (window px): menus anchor
    /// to it. A cell: writing it never notifies.
    pub frame: Rc<Cell<Bounds<Pixels>>>,
}

/// The persistent state for `id`, created the first time it is asked for.
pub(crate) fn interact(id: &ElementId, window: &mut Window, cx: &mut App) -> Entity<Interact> {
    window.use_keyed_state(id.clone(), cx, |_, cx| Interact {
        // An explicitly tracked handle carries its own tab-stop flag: the
        // element's `tab_index` does not reach it.
        focus: cx.focus_handle().tab_stop(true),
        hovered: false,
        pressed: false,
        key_pressed: false,
        enters: 0,
        hot_item: None,
        frame: Rc::new(Cell::new(Bounds::default())),
    })
}

/// Sets `hovered`, counting enters, notifying only on change. Leaving also
/// forgets the hovered sub-item.
pub(crate) fn set_hovered(entity: &Entity<Interact>, value: bool, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.hovered != value {
            state.hovered = value;
            if value {
                state.enters = state.enters.wrapping_add(1);
            } else {
                state.hot_item = None;
            }
            cx.notify();
        }
    });
}

/// Sets the pointer press, notifying only on change.
pub(crate) fn set_pressed(entity: &Entity<Interact>, value: bool, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.pressed != value {
            state.pressed = value;
            cx.notify();
        }
    });
}

/// Sets the keyboard press, notifying only on change.
pub(crate) fn set_key_pressed(entity: &Entity<Interact>, value: bool, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.key_pressed != value {
            state.key_pressed = value;
            cx.notify();
        }
    });
}

/// Sets the hovered sub-item, notifying only on change.
pub(crate) fn set_hot_item(entity: &Entity<Interact>, value: Option<usize>, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.hot_item != value {
            state.hot_item = value;
            cx.notify();
        }
    });
}

/// Whether `focus` is focused *and* the focus arrived via the keyboard: the
/// only time FACET shows the doubled periwinkle bevel (GPUI's own
/// `:focus-visible` signal).
pub(crate) fn focus_visible(focus: &FocusHandle, window: &Window) -> bool {
    focus.is_focused(window) && window.last_input_was_keyboard()
}

/// Everything a control reads from its state this frame, with the pinned
/// [`Look`] merged in and inactive (disabled, busy) controls quietened.
pub(crate) struct Touch {
    pub entity: Entity<Interact>,
    pub focus: FocusHandle,
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub enters: u32,
    pub hot_item: Option<usize>,
    /// Where the control's plate was laid out last frame (window px).
    pub frame: Rc<Cell<Bounds<Pixels>>>,
    /// The raw (unpinned) hover, which [`HoverZone`] reconciles.
    pub live_hover: bool,
    pub motion: Motion,
}

impl Touch {
    /// Reads the state for `id`.
    pub(crate) fn read(
        id: &ElementId,
        look: Look,
        active: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let entity = interact(id, window, cx);
        let motion = Motion::scoped(ElementId::View(entity.entity_id()), cx);
        let state = entity.read(cx);
        let focus = state.focus.clone();
        let live_hover = state.hovered;
        let focused = active && (look.focus || focus_visible(&focus, window));
        Self {
            hovered: active && (look.hover || state.hovered),
            pressed: active && (look.press || state.pressed || state.key_pressed),
            focused,
            enters: state.enters,
            hot_item: if active { state.hot_item } else { None },
            frame: state.frame.clone(),
            live_hover,
            focus,
            motion,
            entity,
        }
    }
}

/// A motion track key: the control id and a channel name, so every track
/// in a motion report says whose it is (`save-lift`, `wrap-x`).
pub(crate) fn track(id: &ElementId, channel: &'static str) -> ElementId {
    ElementId::NamedChild(Arc::new(id.clone()), channel.into())
}

/// A motion track key for a numbered sub-item (`seg-item-2`).
pub(crate) fn track_n(id: &ElementId, channel: &'static str, index: usize) -> ElementId {
    ElementId::NamedChild(
        Arc::new(ElementId::NamedChild(Arc::new(id.clone()), channel.into())),
        index.to_string().into(),
    )
}

/// Wraps a control's plate and keeps its hover flag true to the pointer:
/// the zone is the laid-out box (not the lifted plate, so a hover lift never
/// flickers the hover away), cut to the chamfer, and reconciled every frame.
pub(crate) struct HoverZone {
    child: AnyElement,
    entity: Entity<Interact>,
    frame: Rc<Cell<Bounds<Pixels>>>,
    chamfer: f32,
    stored: bool,
    active: bool,
}

/// A hover zone around `child` for the control whose state is `touch`.
pub(crate) fn hover_zone(
    child: impl IntoElement,
    touch: &Touch,
    chamfer: f32,
    active: bool,
) -> HoverZone {
    HoverZone {
        child: child.into_any_element(),
        entity: touch.entity.clone(),
        frame: touch.frame.clone(),
        chamfer,
        stored: touch.live_hover,
        active,
    }
}

fn inside(bounds: Bounds<Pixels>, chamfer: f32, at: Point<Pixels>) -> bool {
    crate::paint::cut::contains(bounds, chamfer, at)
}

impl IntoElement for HoverZone {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for HoverZone {
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
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Hitbox> {
        self.frame.set(bounds);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        self.child.prepaint(window, cx);
        Some(hitbox)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        hitbox: &mut Option<Hitbox>,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        let Some(hitbox) = hitbox.clone() else {
            return;
        };
        let chamfer = self.chamfer;
        let truth = self.active
            && hitbox.is_hovered(window)
            && inside(bounds, chamfer, window.mouse_position());
        if truth != self.stored {
            // Layout moved under a still pointer (or keyboard input
            // suppressed hover): correct the flag after this frame.
            let entity = self.entity.clone();
            window.defer(cx, move |_window, cx| set_hovered(&entity, truth, cx));
        }
        let entity = self.entity.clone();
        let active = self.active;
        let painted = Cell::new(truth);
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let now =
                active && hitbox.is_hovered(window) && inside(bounds, chamfer, event.position);
            if now != painted.get() {
                painted.set(now);
                set_hovered(&entity, now, cx);
            }
        });
    }
}
