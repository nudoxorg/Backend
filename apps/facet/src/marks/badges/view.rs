//! The badge element: a small cut plate wearing a glyph and one word. Rest
//! on it and its sentence floats beside it on the window's float layer (the
//! house tip: a rest, then a warm sweep between badges), so a hover never
//! moves a neighbour and the meaning is never cut to the badge's width.

use super::{Badge as Facts, Ink};
use crate::Set;
use crate::controls::state::{Touch, hover_zone, track};
use crate::measure::{Measure, Space};
use crate::motion::spec;
use crate::overlay::tooltip::Tipped;
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::probe::{self, TextOverflow};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    px,
};

/// The words on a badge: the role it is set in.
const WORD: TypeRole = TypeRole { weight: 520.0, ..ty::BUTTON };

/// One badge as an element (see [`badge`]).
#[derive(IntoElement)]
pub struct Badge {
    id: ElementId,
    facts: Facts,
    measure: Measure,
    held: bool,
}

/// A badge for `facts`, sized for `measure`, remembering its hover under `id`.
#[must_use]
pub fn badge(id: impl Into<ElementId>, facts: &Facts, measure: &Measure) -> Badge {
    Badge {
        id: id.into(),
        facts: facts.clone(),
        measure: *measure,
        held: false,
    }
}

impl Badge {
    /// Shows the plate as a pointer on it would (scenes, tests).
    #[must_use]
    pub const fn open(mut self) -> Self {
        self.held = true;
        self
    }
}

/// The colour a badge's glyph and word take.
#[must_use]
pub fn ink_of(ink: Ink, palette: &Palette) -> Hsla {
    match ink {
        Ink::Plain => palette.ink1.into(),
        Ink::Peri => palette.peri_hi.into(),
        Ink::Amber => palette.amber.base.into(),
        Ink::Coral => palette.coral.base.into(),
        Ink::Teal => palette.f_type.hue.into(),
        Ink::Mint => palette.mint.base.into(),
    }
}

/// The edge a badge's plate wears: its voice on the lit side.
#[must_use]
pub fn edge_of(ink: Ink, palette: &Palette, hover: f32) -> Edge {
    let rest = match ink {
        Ink::Plain => {
            let mut edge = Edge::of(Bevel::Rest, palette);
            edge.hi = palette.line3.into();
            edge.lo = palette.line2.into();
            edge
        }
        Ink::Peri => Edge::of(Bevel::Peri, palette),
        Ink::Amber => Edge::of(Bevel::Amber, palette),
        Ink::Coral => Edge::of(Bevel::Coral, palette),
        Ink::Mint => Edge::of(Bevel::Hot, palette),
        Ink::Teal => {
            let mut edge = Edge::of(Bevel::Rest, palette);
            edge.hi = palette.f_type.hue.into();
            edge.lo = palette.f_type.hue.alpha(0.25).into();
            edge
        }
    };
    rest.mix(Edge::of(Bevel::Peri, palette), hover * 0.5)
}

impl RenderOnce for Badge {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let open_target = if touch.hovered || self.held { 1.0 } else { 0.0 };
        let open = motion.animate(track(&self.id, "open"), open_target, spec::REVEAL, window, cx);
        let ink = ink_of(self.facts.ink, palette);
        let scale = measure.scale();
        let word_role = measure.role(WORD);

        let word = probe::text(
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "word".into()),
            self.facts.word.clone(),
            word_role,
            1.0,
            TextOverflow::Clip,
            div().set(WORD, &measure).text_color(ink).whitespace_nowrap().child(self.facts.word.clone()),
        );
        let fill = mix(palette.plate.into(), palette.plate2.into(), open);
        let body = cut()
            .chamfer(Chamfer::Px(3.0 * scale))
            .edge(edge_of(self.facts.ink, palette, open))
            .plate(Plate::Flat)
            .fill(fill)
            .flex()
            .flex_none()
            .items_center()
            .gap(measure.space(Space::Snug))
            .h(px(21.0 * scale))
            .pl(measure.space(Space::Snug))
            .pr(measure.space(Space::Snug) + px(1.0))
            .child(super::glyph(self.facts.glyph, 12.0 * scale, ink))
            .child(word);
        let (title, sentence): (SharedString, SharedString) = (self.facts.word.clone(), self.facts.tip.clone());
        div().id(self.id.clone()).flex_none().child(hover_zone(body, &touch, 3.0 * scale, true)).tip_rich(title, sentence, &[])
    }
}
