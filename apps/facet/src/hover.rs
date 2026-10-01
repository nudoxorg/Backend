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
//! - Keyboard focus lights exactly like the pointer ([`focus`]).
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
    AnyElement, App, Bounds, ColorExt, ElementId, Global, GlobalElementId, Hitbox, HitboxBehavior,
    Hsla, InspectorElementId, IntoElement, LayoutId, MouseExitEvent, MouseMoveEvent, Pixels,
    SharedString, Window, WindowId, fill, point, px, size,
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

/// What made an element the target: the pointer over it, or keyboard focus
/// on it. The pointer's target follows the pointer (it lets go when the
/// element moves out from under a still pointer); a focus target stays
/// until focus moves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Source {
    /// The pointer is over it.
    Pointer,
    /// Keyboard focus is on it.
    Keyboard,
}

/// The element lit as the target, its subject and why.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Held {
    id: ElementId,
    subject: Subject,
    source: Source,
}

/// The per-window hover field: which element is the target, and its
/// subject.
#[derive(Default)]
struct Field {
    windows: HashMap<WindowId, Held>,
}

impl Global for Field {}

fn held(window: &Window, cx: &App) -> Option<Held> {
    cx.try_global::<Field>().and_then(|field| {
        field
            .windows
            .get(&window.window_handle().window_id())
            .cloned()
    })
}

fn target(window: &Window, cx: &App) -> Option<(ElementId, Subject)> {
    held(window, cx).map(|held| (held.id, held.subject))
}

fn set_target(value: Option<Held>, window: &mut Window, cx: &mut App) {
    let id = window.window_handle().window_id();
    let field = cx.default_global::<Field>();
    let changed = match &value {
        Some(value) => field.windows.get(&id) != Some(value),
        None => field.windows.contains_key(&id),
    };
    if !changed {
        return;
    }
    match value {
        Some(value) => field.windows.insert(id, value),
        None => field.windows.remove(&id),
    };
    // Answer in the same frame the input arrived in.
    window.refresh();
}

/// Lets go of the target, whatever holds it: the page under the pointer
/// changed (navigation), so nothing on the old page is hovered any more.
/// [`float::close_all`] calls this, and the shell calls that on every
/// navigation.
pub fn clear(window: &mut Window, cx: &mut App) {
    set_target(None, window, cx);
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

/// Keyboard focus lights like the pointer: `Some` makes `id` the target,
/// `None` clears it (only if `id` holds it).
pub fn focus(target: Option<(ElementId, Subject)>, window: &mut Window, cx: &mut App) {
    set_target(
        target.map(|(id, subject)| Held {
            id,
            subject,
            source: Source::Keyboard,
        }),
        window,
        cx,
    );
}

/// The ink a hoverable's text takes: one step up the ink ramp when lit
/// (ink3 → ink2 → ink1 → ink0).
#[must_use]
pub fn ink(rest: Tone, lit: Lit, palette: &Palette) -> Hsla {
    if !lit.is_lit() {
        return rest.hsla();
    }
    let ramp = [
        palette.ink4,
        palette.ink3,
        palette.ink2,
        palette.ink1,
        palette.ink0,
    ];
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
    Hoverable {
        id: id.into(),
        subject,
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
    id: ElementId,
    subject: Subject,
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
        self.lit = lit_as(&self.id, &self.subject, window, cx);
        let built = self
            .build
            .take()
            .map_or_else(|| gpui::Empty.into_any_element(), |build| build(self.lit));
        let mut child = match &self.peek {
            Some(request) => {
                let request = Rc::clone(request);
                float::trigger(self.id.clone(), move |bounds| request(bounds), built)
                    .into_any_element()
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
        let (id, subject) = (self.id.clone(), self.subject.clone());
        // The pointer is still but the content moved (a scroll, a reflow, a
        // page that changed): the target follows the layout, not the last move.
        if held(window, cx).is_some_and(|held| held.id == id && held.source == Source::Pointer)
            && !hitbox.is_hovered(window)
        {
            set_target(None, window, cx);
        }
        window.on_mouse_event({
            let (id, subject) = (id.clone(), subject.clone());
            move |_: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let hovered = hitbox.is_hovered(window);
                let holds = target(window, cx).is_some_and(|(held, _)| held == id);
                if hovered && !holds {
                    let held = Held {
                        id: id.clone(),
                        subject: subject.clone(),
                        source: Source::Pointer,
                    };
                    set_target(Some(held), window, cx);
                } else if !hovered && holds {
                    set_target(None, window, cx);
                }
            }
        });
        // The pointer can leave the window without a final move (GPUI's own
        // hover clears on the exit event for the same reason): a target the
        // pointer held stays lit for ever otherwise.
        window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
            if phase == gpui::DispatchPhase::Bubble
                && held(window, cx)
                    .is_some_and(|held| held.id == id && held.source == Source::Pointer)
            {
                set_target(None, window, cx);
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
