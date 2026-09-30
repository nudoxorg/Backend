//! The one hover grammar (DIRECTION v5 §3). Every hoverable thing (rows,
//! links, marks, chips, graph nodes, comb teeth) hovers this way, and ad hoc
//! hover styles go.
//!
//! - **At 0 ms** (the frame that answers the pointer):
//!   - the target's ink rises one step ([`ink`]);
//!   - its hit shape draws its bevel: 1 px, the kind's hue at 45 %;
//!   - its own relation stroke rises from 1.2 to 1.5 px ([`stroke`]).
//! - **At 0 ms, everything related lights:** every other occurrence of the
//!   same [`Subject`] on screen gets a 1.5 px underline in the kind's hue.
//!   The relation strokes a painter draws to it light at 1.5 px in full hue:
//!   painters ask [`lit`].
//! - **At 350 ms** a peek unfurls from the target (the float layer's hover
//!   intent) and leaves the way it came.
//! - Nothing tweens: colour and stroke steps are instant.
//! - Keyboard focus lights exactly like the pointer ([`focus`]). Each window
//!   retains both source targets: the latest input source takes the light,
//!   and the other resumes when it leaves.
//! - The pointer's target lets go when the pointer leaves the window, when
//!   the content moves out from under a still pointer, and on navigation
//!   ([`clear`]); a focus target stays until focus moves.
//!
//! ```ignore
//! let subject = hover::Subject::new("present::glyph::SemanticLinkKind");
//! hover::hoverable("rel-0", subject, kind_hue, move |lit| {
//!     div().text_color(hover::ink(palette.ink1, lit, palette)).child("SemanticLinkKind").into_any_element()
//! })
//! .peek(|anchor| peek::request("rel-0", anchor, semantic_link_kind()))
//! ```

use crate::overlay::float::{self, FloatRequest};
use crate::paint::geom::{Poly, fill_poly};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, Tone};
use gpui::{
    AnyElement, App, Bounds, ColorExt, ElementId, Global, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseExitEvent, MouseMoveEvent, Pixels, SharedString, Window,
    WindowId, fill, point, px, size,
};
use std::collections::HashMap;
use std::rc::Rc;

/// What lights together: every hoverable with the same subject (a symbol's
/// address, a package id, a release). "Light every other occurrence of X on
/// screen" is this key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Subject(pub SharedString);

impl Subject {
    /// A subject named by `address`.
    pub fn new(address: impl Into<SharedString>) -> Self {
        Self(address.into())
    }
}

/// The identity shared by a pointer hoverable and its semantic keyboard
/// target. Build the hoverable from this value and pass the same value to the
/// shell's target registration; Facet never guesses keyboard focus from the
/// pointer or layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusTarget {
    id: ElementId,
    subject: Subject,
}

impl FocusTarget {
    /// The identity for a custom hoverable whose pointer and keyboard target
    /// are maintained outside [`Hoverable`]. Prefer [`hoverable_target`] when
    /// both inputs use the standard hover element.
    #[must_use]
    pub fn new(id: impl Into<ElementId>, subject: Subject) -> Self {
        Self { id: id.into(), subject }
    }

    /// The stable element id shared with the pointer hit target.
    #[must_use]
    pub fn id(&self) -> &ElementId {
        &self.id
    }

    /// The semantic identity shared with related hoverables.
    #[must_use]
    pub fn subject(&self) -> &Subject {
        &self.subject
    }
}

/// How a hoverable is lit this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lit {
    /// Nothing about it is hovered.
    #[default]
    Rest,
    /// It is the thing under the pointer (or keyboard focus).
    Target,
    /// Another occurrence of the target's subject.
    Related,
}

impl Lit {
    /// Whether it is lit at all.
    #[must_use]
    pub const fn is_lit(self) -> bool {
        !matches!(self, Self::Rest)
    }
}

/// Which input source currently owns a hover treatment. A semantic focus
/// change wins over the pointer position remembered before it; the pointer
/// takes over again after its next actual move.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum InteractionMode {
    /// The remembered pointer hit may light its target.
    Pointer,
    /// The current semantic target overrides a stale pointer hit.
    Keyboard,
}

/// The current interaction mode for a component whose keyboard target is
/// `rest` and whose last pointer event observed `observed_rest`.
#[must_use]
pub fn interaction_mode<T: PartialEq>(observed_rest: Option<T>, rest: Option<T>) -> InteractionMode {
    if observed_rest == rest { InteractionMode::Pointer } else { InteractionMode::Keyboard }
}

/// Chooses the target lit by pointer or semantic keyboard focus using the
/// shared modality rule. Components store `observed_rest` with pointer state
/// and refresh it on a genuine pointer move.
#[must_use]
pub fn visible_target<T: Copy + PartialEq>(pointer: Option<T>, observed_rest: Option<T>, rest: Option<T>) -> Option<T> {
    match interaction_mode(observed_rest, rest) {
        InteractionMode::Pointer => pointer.or(rest),
        InteractionMode::Keyboard => rest,
    }
}

/// The hit shape the target's bevel is drawn on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Shape {
    /// A plain box (words, rows).
    #[default]
    Rect,
    /// A cut plate with this chamfer (chips, cards, steps).
    Chamfer(f32),
    /// A diamond (contracts, graph nodes).
    Diamond,
}

/// The element lit as the target, its subject and why.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: ElementId,
    subject: Subject,
}

/// The active target and both input sources belong to one window. Keeping the
/// inactive source lets keyboard focus and the pointer yield to one another
/// without losing the target that should resume when the other leaves.
#[derive(Default)]
struct WindowField {
    pointer: Option<Held>,
    keyboard: Option<Held>,
    active: Option<InteractionMode>,
}

impl WindowField {
    fn held(&self) -> Option<Held> {
        let value = match self.active {
            Some(InteractionMode::Pointer) => self.pointer.as_ref().or(self.keyboard.as_ref()),
            Some(InteractionMode::Keyboard) => self.keyboard.as_ref().or(self.pointer.as_ref()),
            None => self.keyboard.as_ref().or(self.pointer.as_ref()),
        };
        value.cloned()
    }

    fn is_empty(&self) -> bool {
        self.pointer.is_none() && self.keyboard.is_none()
    }
}

/// The per-window hover field. Each window retains its own pointer and
/// keyboard targets; no window can light another window's target.
#[derive(Default)]
struct Field {
    windows: HashMap<WindowId, WindowField>,
}

impl Global for Field {}

fn held(window: &Window, cx: &App) -> Option<Held> {
    cx.try_global::<Field>()
        .and_then(|field| field.windows.get(&window.window_handle().window_id()).and_then(WindowField::held))
}

fn pointer_held(window: &Window, cx: &App) -> Option<Held> {
    cx.try_global::<Field>()
        .and_then(|field| field.windows.get(&window.window_handle().window_id()).and_then(|state| state.pointer.clone()))
}

fn target(window: &Window, cx: &App) -> Option<(ElementId, Subject)> {
    held(window, cx).map(|held| (held.id, held.subject))
}

fn set_pointer(value: Option<Held>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let field = cx.default_global::<Field>();
    let before = field.windows.get(&id).and_then(WindowField::held);
    let (after, empty) = {
        let state = field.windows.entry(id).or_default();
        match value {
            Some(value) => {
                state.pointer = Some(value);
                state.active = Some(InteractionMode::Pointer);
            }
            None => {
                state.pointer = None;
                if state.active == Some(InteractionMode::Pointer) {
                    state.active = state.keyboard.as_ref().map(|_| InteractionMode::Keyboard);
                }
            }
        }
        (state.held(), state.is_empty())
    };
    if empty { field.windows.remove(&id); }
    if before != after {
        // Answer in the same frame the input arrived in.
        window.refresh();
    }
}

/// Lets go of the target, whatever holds it: the page under the pointer
/// changed (navigation), so nothing on the old page is hovered any more.
/// [`float::close_all`] calls this, and the shell calls that on every
/// navigation.
pub fn clear(window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    if cx.default_global::<Field>().windows.remove(&id).is_some() {
        window.refresh();
    }
}

/// The subject of whatever is the target now (the pointer's or keyboard
/// focus's), for painters that answer a scrub across the page (W-Glyph's
/// reach bar: the decks bring the scrubbed member's lines forward).
#[must_use]
pub fn hovered(window: &Window, cx: &App) -> Option<Subject> {
    target(window, cx).map(|(_, subject)| subject)
}

/// How the hoverable `id` with `subject` is lit.
#[must_use]
pub fn lit_as(id: &ElementId, subject: &Subject, window: &Window, cx: &App) -> Lit {
    match target(window, cx) {
        Some((target, _)) if &target == id => Lit::Target,
        Some((_, hovered)) if &hovered == subject => Lit::Related,
        _ => Lit::Rest,
    }
}

/// Whether anything with `subject` is the target: for painters of
/// relation strokes (rails, stubs, spines, graph edges) that light with it.
#[must_use]
pub fn lit(subject: &Subject, window: &Window, cx: &App) -> Lit {
    match target(window, cx) {
        Some((_, hovered)) if &hovered == subject => Lit::Target,
        _ => Lit::Rest,
    }
}

/// Keyboard focus lights like the pointer. Each window retains its last
/// pointer hit independently, so clearing focus restores that target when it
/// still lies under the pointer. A repeated synchronization of the same focus
/// target does not steal the light from a more recent pointer move.
pub fn focus(target: Option<FocusTarget>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let field = cx.default_global::<Field>();
    let before = field.windows.get(&id).and_then(WindowField::held);
    let (after, empty) = {
        let state = field.windows.entry(id).or_default();
        match target {
            Some(target) => {
                let target = Held { id: target.id, subject: target.subject };
                // The shell mirrors its focus target every layout pass. Only
                // a real focus change takes precedence over a recent pointer.
                if state.keyboard.as_ref() != Some(&target) {
                    state.keyboard = Some(target);
                    state.active = Some(InteractionMode::Keyboard);
                }
            }
            None => {
                state.keyboard = None;
                if state.active == Some(InteractionMode::Keyboard) {
                    state.active = state.pointer.as_ref().map(|_| InteractionMode::Pointer);
                }
            }
        }
        (state.held(), state.is_empty())
    };
    if empty { field.windows.remove(&id); }
    if before != after { window.refresh(); }
}

/// The ink a hoverable's text takes: one step up the ink ramp when lit
/// (ink3 → ink2 → ink1 → ink0).
#[must_use]
pub fn ink(rest: Tone, lit: Lit, palette: &Palette) -> Hsla {
    if !lit.is_lit() {
        return rest.hsla();
    }
    let ramp = [palette.ink4, palette.ink3, palette.ink2, palette.ink1, palette.ink0];
    let step = ramp
        .iter()
        .position(|tone| *tone == rest)
        .map_or(rest, |index| ramp[(index + 1).min(ramp.len() - 1)]);
    step.hsla()
}

/// A relation stroke's width: its rest width, raised to 1.5 px (the
/// relation stroke) while its subject is lit.
#[must_use]
pub fn stroke(rest: f32, lit: Lit) -> f32 {
    if lit.is_lit() {
        rest.max(crate::tokens::stroke::RELATION)
    } else {
        rest
    }
}

/// Wraps what `build` draws in the grammar. `build` is called during layout
/// with this frame's [`Lit`], so the element can raise its own ink.
pub fn hoverable(
    id: impl Into<ElementId>,
    subject: Subject,
    hue: Hsla,
    build: impl FnOnce(Lit) -> AnyElement + 'static,
) -> Hoverable {
    hoverable_target(FocusTarget::new(id, subject), hue, build)
}

/// A hoverable built from the exact identity the shell can register as its
/// semantic keyboard target. Keeping the identity in one value prevents the
/// pointer id or subject from drifting away from the shell's focus mirror.
pub fn hoverable_target(
    target: FocusTarget,
    hue: Hsla,
    build: impl FnOnce(Lit) -> AnyElement + 'static,
) -> Hoverable {
    Hoverable {
        target,
        hue,
        shape: Shape::Rect,
        build: Some(Box::new(build)),
        peek: None,
        child: None,
        lit: Lit::Rest,
    }
}

/// See [`hoverable`].
pub struct Hoverable {
    target: FocusTarget,
    hue: Hsla,
    shape: Shape,
    build: Option<Box<dyn FnOnce(Lit) -> AnyElement>>,
    peek: Option<Rc<dyn Fn(Bounds<Pixels>) -> FloatRequest>>,
    child: Option<AnyElement>,
    lit: Lit,
}

impl Hoverable {
    /// The hit shape its bevel draws on (default [`Shape::Rect`]).
    #[must_use]
    pub fn shape(mut self, shape: Shape) -> Self {
        self.shape = shape;
        self
    }

    /// The card that unfurls from it after the hover delay (350 ms for a
    /// peek), built from its bounds.
    #[must_use]
    pub fn peek(mut self, request: impl Fn(Bounds<Pixels>) -> FloatRequest + 'static) -> Self {
        self.peek = Some(Rc::new(request));
        self
    }
}

impl IntoElement for Hoverable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The bevel stroke's alpha on the kind's hue.
const BEVEL_ALPHA: f32 = 0.45;
/// The related underline's width, px.
const UNDERLINE: f32 = 1.5;

impl gpui::Element for Hoverable {
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
        self.lit = lit_as(&self.target.id, &self.target.subject, window, cx);
        let built = self.build.take().map_or_else(|| gpui::Empty.into_any_element(), |build| build(self.lit));
        let mut child = match &self.peek {
            Some(request) => {
                let request = Rc::clone(request);
                float::trigger(self.target.id.clone(), move |bounds| request(bounds), built).into_any_element()
            }
            None => built,
        };
        let layout = child.request_layout(window, cx);
        self.child = Some(child);
        (layout, ())
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
        if let Some(child) = self.child.as_mut() {
            child.prepaint(window, cx);
        }
        Some(window.insert_hitbox(bounds, HitboxBehavior::Normal))
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
        if let Some(child) = self.child.as_mut() {
            child.paint(window, cx);
        }
        match self.lit {
            Lit::Target => paint_bevel(bounds, self.shape, self.hue.opacity(BEVEL_ALPHA), window),
            Lit::Related => window.paint_quad(fill(
                gpui::Bounds::new(
                    point(bounds.origin.x, bounds.bottom() - px(UNDERLINE)),
                    size(bounds.size.width, px(UNDERLINE)),
                ),
                self.hue,
            )),
            Lit::Rest => {}
        }
        let Some(hitbox) = hitbox.clone() else {
            return;
        };
        let (id, subject) = (self.target.id.clone(), self.target.subject.clone());
        // The pointer is still but the content moved (a scroll, a reflow, a
        // page that changed): the target follows the layout, not the last move.
        if pointer_held(window, cx).is_some_and(|held| held.id == id)
            && !hitbox.is_hovered(window)
        {
            set_pointer(None, window, cx);
        }
        window.on_mouse_event({
            let (id, subject) = (id.clone(), subject.clone());
            move |_: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let hovered = hitbox.is_hovered(window);
                if hovered {
                    let held = Held { id: id.clone(), subject: subject.clone() };
                    set_pointer(Some(held), window, cx);
                } else if pointer_held(window, cx).is_some_and(|held| held.id == id) {
                    set_pointer(None, window, cx);
                }
            }
        });
        // The pointer can leave the window without a final move (GPUI's own
        // hover clears on the exit event for the same reason): a target the
        // pointer held stays lit for ever otherwise.
        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Bubble
                && pointer_held(window, cx).is_some_and(|held| held.id == id)
            {
                set_pointer(None, window, cx);
            }
        });
        let _ = cx.facet();
    }
}

/// The target's bevel: a 1 px line along its hit shape.
fn paint_bevel(bounds: Bounds<Pixels>, shape: Shape, color: Hsla, window: &mut Window) {
    let (x, y, w, h) = (
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y),
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    );
    let poly = match shape {
        Shape::Rect => Poly::chamfer(x, y, w, h, 0.0),
        Shape::Chamfer(chamfer) => Poly::chamfer(x, y, w, h, chamfer),
        Shape::Diamond => Poly::new([
            crate::paint::geom::pt(x + w / 2.0, y),
            crate::paint::geom::pt(x + w, y + h / 2.0),
            crate::paint::geom::pt(x + w / 2.0, y + h),
            crate::paint::geom::pt(x, y + h / 2.0),
        ]),
    };
    for edge in poly.stroke_ring(1.0) {
        fill_poly(window, &edge, color);
    }
}

#[cfg(test)]
mod tests;
