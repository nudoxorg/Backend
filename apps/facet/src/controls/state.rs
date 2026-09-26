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
use crate::probe::{self, Target};
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, Entity, FocusHandle, Global,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseExitEvent, MouseMoveEvent, Pixels, Point, Window, WindowId,
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
    /// Armed by Enter/Space until the key comes up (Space activates on
    /// its release).
    pub key_pressed: bool,
    /// The keyboard press as shown: a beat that the key's release ends early
    /// and that ends by itself if the release never comes (focus moved, the
    /// window lost the key), so a key can never leave a control sunk.
    pub key_beat: bool,
    key_beats: u64,
    /// How many facet sweeps have started: one per hover-enter that finds
    /// no crossing in flight (an enter mid-crossing lets it finish).
    pub sweeps: u32,
    /// A crossing is in flight (the control's render clears it at the end).
    pub sweeping: bool,
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
        key_beat: false,
        key_beats: 0,
        sweeps: 0,
        sweeping: false,
        hot_item: None,
        frame: Rc::new(Cell::new(Bounds::default())),
    })
}

/// Sets `hovered`, notifying only on change. Entering starts a sweep
/// unless one is still crossing; leaving forgets the hovered sub-item.
pub(crate) fn set_hovered(entity: &Entity<Interact>, value: bool, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.hovered != value {
            state.hovered = value;
            if value {
                if !state.sweeping {
                    state.sweeps = state.sweeps.wrapping_add(1);
                    state.sweeping = true;
                }
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

/// Sets the keyboard press, notifying only on change. Releasing also ends
/// the shown beat; pressing goes through [`key_press`].
pub(crate) fn set_key_pressed(entity: &Entity<Interact>, value: bool, cx: &mut App) {
    entity.update(cx, |state, cx| {
        if state.key_pressed != value || (!value && state.key_beat) {
            state.key_pressed = value;
            if !value {
                state.key_beat = false;
            }
            cx.notify();
        }
    });
}

/// The longest a keyboard press shows without its release: past the
/// platform's key-repeat delay, so a held key reads as held.
const KEY_BEAT: std::time::Duration = std::time::Duration::from_millis(600);

/// Enter/Space went down on the control: armed, and shown pressed for a
/// beat (the release ends it sooner; a repeat re-arms it).
pub(crate) fn key_press(entity: &Entity<Interact>, window: &mut Window, cx: &mut App) {
    let beat = entity.update(cx, |state, cx| {
        state.key_pressed = true;
        state.key_beat = true;
        state.key_beats = state.key_beats.wrapping_add(1);
        cx.notify();
        state.key_beats
    });
    let timer = cx.background_executor().timer(KEY_BEAT);
    let entity = entity.downgrade();
    window
        .spawn(cx, async move |cx| {
            timer.await;
            let _ = cx.update(|_window, cx| {
                if let Some(entity) = entity.upgrade() {
                    entity.update(cx, |state, cx| {
                        if state.key_beats == beat && state.key_beat {
                            state.key_beat = false;
                            cx.notify();
                        }
                    });
                }
            });
        })
        .detach();
}

/// The render saw the current sweep reach its end.
pub(crate) fn sweep_done(entity: &Entity<Interact>, window: &mut Window, cx: &mut App) {
    let entity = entity.clone();
    window.defer(cx, move |_window, cx| {
        entity.update(cx, |state, _| state.sweeping = false);
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
    /// The current sweep's number (0: none yet) and whether it is crossing.
    pub sweeps: u32,
    pub sweeping: bool,
    pub hot_item: Option<usize>,
    /// Where the control's plate was laid out last frame (window px).
    pub frame: Rc<Cell<Bounds<Pixels>>>,
    /// The raw (unpinned) hover, which [`HoverZone`] reconciles.
    pub live_hover: bool,
    pub motion: Motion,
    /// What the control shows, for the probe: only a live control (no
    /// pinned [`Look`]) claims its hover, press and focus are real.
    pub claim: Option<(ElementId, Target)>,
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
        let hovered = active && (look.hover || state.hovered);
        // A keyboard press shows only on the control that holds focus.
        let key_beat = state.key_beat && focus.is_focused(window);
        let pressed = active && (look.press || state.pressed || key_beat);
        let claim = (look == Look::LIVE).then(|| {
            (
                id.clone(),
                Target {
                    hovered,
                    // The probe's press is the pointer's (the storm checks
                    // it against the buttons); a key press ends by itself.
                    pressed: active && state.pressed,
                    focused,
                    focusable: active,
                    clickable: active,
                },
            )
        });
        Self {
            hovered,
            pressed,
            focused,
            sweeps: state.sweeps,
            sweeping: state.sweeping,
            hot_item: if active { state.hot_item } else { None },
            frame: state.frame.clone(),
            live_hover,
            focus,
            motion,
            entity,
            claim,
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

/// Windows the pointer has left. GPUI keeps the last position (and its hit
/// test) after the pointer exits, so without this whatever sat under the
/// exit point would stay lit until the pointer came back.
#[derive(Default)]
struct PointerAway(Vec<WindowId>);

impl Global for PointerAway {}

/// Whether the pointer has left `window`.
pub(crate) fn pointer_away(window: &Window, cx: &App) -> bool {
    let id = window.window_handle().window_id();
    cx.try_global::<PointerAway>().is_some_and(|away| away.0.contains(&id))
}

/// Keeps [`pointer_away`] true to the pointer (every live control's paint
/// calls it; the capture phase runs before any control's own listener).
pub(crate) fn watch_pointer(window: &mut Window) {
    window.on_mouse_event(|_: &MouseExitEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture {
            let id = window.window_handle().window_id();
            let away = cx.default_global::<PointerAway>();
            if !away.0.contains(&id) {
                away.0.push(id);
            }
        }
    });
    window.on_mouse_event(|_: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture && pointer_away(window, cx) {
            let id = window.window_handle().window_id();
            cx.default_global::<PointerAway>().0.retain(|away| *away != id);
        }
    });
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
    claim: Option<(ElementId, Target)>,
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
        claim: touch.claim.clone(),
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
        if let Some((key, target)) = &self.claim {
            probe::record_target(cx, key, bounds, *target);
        }
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
        watch_pointer(window);
        let truth = self.active
            && !pointer_away(window, cx)
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
        let painted = Rc::new(Cell::new(truth));
        {
            let entity = entity.clone();
            let painted = painted.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
                if phase == DispatchPhase::Bubble && painted.replace(false) {
                    set_hovered(&entity, false, cx);
                }
            });
        }
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
