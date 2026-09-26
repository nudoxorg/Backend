//! What every mark remembers between frames, and the one set of pointer and
//! keyboard hooks they all share.
//!
//! A mark is one custom element. Its hover, its keyboard walk, its focus
//! handle and its motion tracks live in a small entity keyed by the mark's
//! element id (`Window::use_keyed_state`), so the caller never threads a
//! `hovered: Option<usize>` through its own model: a hover wave changes only
//! this entity, which notifies only the view that painted the mark; the
//! caller's data is never rebuilt for a hover.
//!
//! Parts are found by index arithmetic (each mark supplies `hit`, a pure
//! function of the pointer and its painted geometry), never by scanning.

use super::door::{self, Door, Side};
use crate::motion::Motion;
use gpui::{
    AnyElement, App, Bounds, InteractiveElement, IntoElement, LayoutId, Styled,
    div, DispatchPhase, ElementId, Entity, FocusHandle, Hitbox,
    HitboxBehavior, KeyDownEvent, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, Pixels, Point,
    Window,
};
use std::rc::Rc;

/// Finds the part under a window point.
pub(crate) type Hit = Rc<dyn Fn(Point<Pixels>) -> Option<usize>>;
/// A part's sub-rect in window coordinates.
pub(crate) type Anchor = Rc<dyn Fn(usize) -> Option<Bounds<Pixels>>>;
/// The keyboard walk: from the current part, a key, over `count` parts.
pub(crate) type Step = Rc<dyn Fn(Option<usize>, &str, usize) -> Option<usize>>;

/// One mark's memory.
pub struct Live {
    /// The part under the pointer.
    pub hover: Option<usize>,
    /// The part the keyboard walk sits on.
    pub walk: Option<usize>,
    /// Focus for the keyboard walk.
    pub handle: FocusHandle,
    /// The mark's motion tracks (wave, lift, rung cross-fade, arrival).
    pub motion: Motion,
    /// Where a hover wave last centred, px within the mark (a leaving wave
    /// decays there).
    pub wave_at: (f32, f32),
    /// A scalar scratch value a mark may keep between frames (gem progress
    /// remembers the fill it last flowed to).
    pub memo: f32,
    /// Which wave this is: a rest that starts from calm starts a new wave
    /// (a new track) instead of teleporting the old one.
    pub wave_gen: u64,
    /// Earlier waves still settling: `(generation, target)`. They are
    /// sampled until they come to rest, so no track is abandoned mid-flight.
    waves_settling: Vec<(u64, (f32, f32), std::time::Instant)>,
    anchor: Option<Anchor>,
    count: usize,
    wiring: Option<Wiring>,
}

/// What the key handler needs, refreshed every paint.
#[derive(Clone)]
struct Wiring {
    mark: ElementId,
    door: Option<Door>,
    side: Side,
    step: Step,
}

impl Live {
    /// The part to light: the pointer's, else the walk's.
    #[must_use]
    pub fn lit(&self) -> Option<usize> {
        self.hover.or(self.walk)
    }
}

/// The mark state for the element being drawn (its id must be on the stack:
/// call from `request_layout`, `prepaint` or `paint`).
pub(crate) fn live(window: &mut Window, cx: &mut App) -> Entity<Live> {
    window.use_keyed_state("live", cx, |_, cx| Live {
        hover: None,
        walk: None,
        handle: cx.focus_handle().tab_stop(true),
        motion: Motion::new(),
        wave_at: (-1.0e4, -1.0e4),
        memo: -1.0,
        wave_gen: 0,
        waves_settling: Vec::new(),
        anchor: None,
        count: 0,
        wiring: None,
    })
}

/// A motion key scoped to one mark (probe keys must not collide).
pub(crate) fn key(mark: &ElementId, name: &'static str) -> ElementId {
    ElementId::NamedChild(std::sync::Arc::new(mark.clone()), gpui::SharedString::new_static(name))
}

/// A motion key scoped to one mark and a number.
pub(crate) fn key_n(mark: &ElementId, name: &'static str, n: u64) -> ElementId {
    ElementId::NamedChild(std::sync::Arc::new(mark.clone()), gpui::SharedString::from(format!("{name}-{n}")))
}

/// The wave's centre for a mark, in px within the mark: while something is
/// lit it springs after the lit part; a rest that begins from calm starts a
/// new wave (new tracks, first seen at their target) so the centre never
/// teleports; a leaving wave decays where it stood; earlier waves are
/// sampled until they rest, so no track is abandoned mid-flight. Returns
/// `(x, y, strength)`.
pub(crate) fn wave(
    mark: &ElementId,
    live: &Entity<Live>,
    active: Option<(f32, f32)>,
    window: &mut Window,
    cx: &mut App,
) -> (f32, f32, f32) {
    let motion = live.read(cx).motion.clone();
    let strength = motion.animate(
        key(mark, "strength"),
        if active.is_some() { 1.0 } else { 0.0 },
        crate::motion::spec::HOVER,
        window,
        cx,
    );
    let (target, generation, settling) = live.update(cx, |state, cx| {
        if let Some(at) = active {
            let far = (state.wave_at.0 - at.0).abs() + (state.wave_at.1 - at.1).abs() > 0.5;
            if strength < 0.02 && far {
                state.waves_settling.push((state.wave_gen, state.wave_at, crate::motion::now(cx)));
                state.wave_gen += 1;
            }
            state.wave_at = at;
        }
        (state.wave_at, state.wave_gen, state.waves_settling.clone())
    });
    // A glide, not a spring: it follows the pointer smoothly, retargets
    // from where it is, and lands exactly on its part (a spring's last
    // sub-pixel step to rest reads as a jump to the alignment checks).
    let follow = crate::motion::spec::REVEAL;
    let x = motion.animate(key_n(mark, "wave-x", generation), target.0, follow, window, cx);
    let y = motion.animate(key_n(mark, "wave-y", generation), target.1, follow, window, cx);
    // An earlier wave keeps being sampled until its glide has surely
    // ended (its whole budget, plus a frame's slack): its last sample is
    // then an at-rest one, and the track can go.
    let now = crate::motion::now(cx);
    let done_after = crate::tokens::motion::QUICK + std::time::Duration::from_millis(40);
    let mut rested = Vec::new();
    for (g, t, since) in settling {
        motion.animate(key_n(mark, "wave-x", g), t.0, follow, window, cx);
        motion.animate(key_n(mark, "wave-y", g), t.1, follow, window, cx);
        if now.saturating_duration_since(since) > done_after {
            rested.push(g);
        }
    }
    if !rested.is_empty() {
        let keys: Vec<ElementId> = rested
            .iter()
            .flat_map(|g| [key_n(mark, "wave-x", *g), key_n(mark, "wave-y", *g)])
            .collect();
        motion.retain(|k| !keys.contains(k));
        live.update(cx, |state, _| state.waves_settling.retain(|(g, _, _)| !rested.contains(g)));
    }
    (x, y, strength)
}

/// The walk along a row of parts: ←/→ (and ↑/↓) step, Home/End jump.
pub(crate) fn linear(current: Option<usize>, key: &str, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let last = count - 1;
    match key {
        "left" | "up" => Some(current.map_or(last, |i| i.saturating_sub(1))),
        "right" | "down" => Some(current.map_or(0, |i| (i + 1).min(last))),
        "home" => Some(0),
        "end" => Some(last),
        _ => current,
    }
}

/// The walk across a grid of `columns`: ←/→ step, ↑/↓ move a row.
pub(crate) fn grid(columns: usize) -> Step {
    Rc::new(move |current, key, count| {
        if count == 0 {
            return None;
        }
        let cols = columns.max(1);
        let last = count - 1;
        match (current, key) {
            (None, _) => Some(0),
            (Some(i), "left") => Some(i.saturating_sub(1)),
            (Some(i), "right") => Some((i + 1).min(last)),
            (Some(i), "up") => Some(i.saturating_sub(cols)),
            (Some(i), "down") => Some((i + cols).min(last)),
            (Some(_), "home") => Some(0),
            (Some(_), "end") => Some(last),
            (Some(i), _) => Some(i),
        }
    })
}

/// Everything a mark hands the shared hooks.
#[derive(Clone)]
pub(crate) struct Hooks {
    pub mark: ElementId,
    pub live: Entity<Live>,
    pub door: Option<Door>,
    pub side: Side,
    pub count: usize,
    pub hit: Hit,
    pub anchor: Anchor,
    pub step: Step,
}

/// The mark's focus child: an invisible `div` over the mark that is a tab
/// stop and owns the key handler (GPUI registers tab stops for divs only).
/// Call from `request_layout`; lay the child out as one of the mark's
/// children, prepaint and paint it with the mark.
pub(crate) fn keys(live: &Entity<Live>, window: &mut Window, cx: &mut App) -> (AnyElement, LayoutId) {
    let handle = live.read(cx).handle.clone();
    let state = live.clone();
    let mut child = div()
        .id("keys")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .track_focus(&handle)
        .on_key_down(move |event: &KeyDownEvent, window, cx| on_key(&state, event, window, cx))
        .into_any_element();
    let id = child.request_layout(window, cx);
    (child, id)
}

/// [`keys`] for a mark that may be static (no id): the child and the
/// layout ids to hand `request_layout`.
pub(crate) fn keys_for(
    live: Option<&Entity<Live>>,
    window: &mut Window,
    cx: &mut App,
) -> (Option<AnyElement>, Vec<LayoutId>) {
    match live {
        Some(live) => {
            let (child, id) = keys(live, window, cx);
            (Some(child), vec![id])
        }
        None => (None, Vec::new()),
    }
}

/// Prepaint: the focus child, then the hitbox.
pub(crate) fn prepaint(
    bounds: Bounds<Pixels>,
    keys: &mut AnyElement,
    window: &mut Window,
    cx: &mut App,
) -> Hitbox {
    keys.prepaint(window, cx);
    window.insert_hitbox(bounds, HitboxBehavior::Normal)
}

fn on_key(live: &Entity<Live>, event: &KeyDownEvent, window: &mut Window, cx: &mut App) {
    let (current, count, anchor, wiring) = {
        let state = live.read(cx);
        (state.walk, state.count, state.anchor.clone(), state.wiring.clone())
    };
    let (Some(anchor), Some(Wiring { mark, door, side, step })) = (anchor, wiring) else {
        return;
    };
    let key = event.keystroke.key.as_str();
    match key {
        "left" | "right" | "up" | "down" | "home" | "end" => {
            let next = step(current, key, count);
            if next != current {
                live.update(cx, |state, cx| {
                    state.walk = next;
                    cx.notify();
                });
                // A part opened from the keyboard follows the walk.
                if let (Some(door), Some(i)) = (&door, next)
                    && door::rested_part(&mark, cx).is_some_and(|(_, keyboard)| keyboard)
                    && let Some(rect) = anchor(i)
                {
                    door::open(&mark, i, rect, door.preferred(side), door, window, cx);
                }
            }
            cx.stop_propagation();
        }
        "space" => {
            let target = current.or(if count > 0 { Some(0) } else { None });
            if let (Some(door), Some(i)) = (&door, target)
                && let Some(rect) = anchor(i)
            {
                if current.is_none() {
                    live.update(cx, |state, cx| {
                        state.walk = Some(i);
                        cx.notify();
                    });
                }
                door::open(&mark, i, rect, door.preferred(side), door, window, cx);
                cx.stop_propagation();
            }
        }
        "enter" => {
            if let (Some(door), Some(i)) = (&door, current) {
                door::activate(door, i, window, cx);
                cx.stop_propagation();
            }
        }
        "escape" => {
            if door::close(&mark, window, cx) {
                cx.stop_propagation();
            }
        }
        _ => {}
    }
}

/// Whether the walk light should show (focused by the keyboard).
pub(crate) fn walking(live: &Entity<Live>, window: &Window, cx: &App) -> Option<usize> {
    let state = live.read(cx);
    (state.handle.is_focused(window) && window.last_input_was_keyboard())
        .then_some(state.walk)
        .flatten()
}

/// Paint: pointer hover, click, and the keyboard walk.
///
/// The pointer is read in the capture phase, before the float layer's own
/// listener, so every move re-reports the rested part (the layer drops
/// triggers that stop reporting) and a change of part leaves the old one
/// and rests on the new one in the same event.
pub(crate) fn paint(
    hooks: Hooks,
    keys: &mut AnyElement,
    hitbox: &Hitbox,
    window: &mut Window,
    cx: &mut App,
) {
    keys.paint(window, cx);
    let Hooks {
        mark,
        live,
        door,
        side,
        count,
        hit,
        anchor,
        step,
    } = hooks;
    live.update(cx, |state, _| {
        state.anchor = Some(anchor.clone());
        state.count = count;
        state.wiring = Some(Wiring {
            mark: mark.clone(),
            door: door.clone(),
            side,
            step: step.clone(),
        });
        if state.hover.is_some_and(|i| i >= count) {
            state.hover = None;
        }
        if state.walk.is_some_and(|i| i >= count) {
            state.walk = None;
        }
    });

    // The open part follows the mark as it moves (scroll, reflow, FLIP).
    if door.is_some()
        && let Some((part, _)) = door::rested_part(&mark, cx)
        && let Some(rect) = anchor(part)
    {
        door::anchor(&mark, part, rect, window, cx);
    }

    // The pointer leaving the window leaves every mark: no move follows to
    // say so, so a hover would otherwise stay lit.
    {
        let (live, mark, door, anchor) = (live.clone(), mark.clone(), door.clone(), anchor.clone());
        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let Some(prev) = live.read(cx).hover else { return };
            live.update(cx, |state, cx| {
                state.hover = None;
                cx.notify();
            });
            if let Some(door) = &door
                && let Some(rect) = anchor(prev)
            {
                door::report(&mark, prev, rect, door.preferred(side), door, false, window, cx);
            }
        });
    }

    {
        let (live, mark, door, hit, anchor, hitbox) = (
            live.clone(),
            mark.clone(),
            door.clone(),
            hit.clone(),
            anchor.clone(),
            hitbox.clone(),
        );
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let next = if hitbox.is_hovered(window) {
                hit(event.position)
            } else {
                None
            };
            let prev = live.read(cx).hover;
            if prev != next {
                live.update(cx, |state, cx| {
                    state.hover = next;
                    cx.notify();
                });
            }
            let Some(door) = &door else { return };
            let side = door.preferred(side);
            if prev != next
                && let Some(p) = prev
                && let Some(rect) = anchor(p)
            {
                door::report(&mark, p, rect, side, door, false, window, cx);
            }
            if let Some(n) = next
                && let Some(rect) = anchor(n)
            {
                door::report(&mark, n, rect, side, door, true, window, cx);
            }
        });
    }

    if let Some(door) = door.clone() {
        let (hit, hitbox) = (hit.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            if let Some(i) = hit(event.position) {
                door::activate(&door, i, window, cx);
            }
        });
    }
    let _ = step;
}
