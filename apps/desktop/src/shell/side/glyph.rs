//! What is painted at a row's edge and at its left: the state glyphs (a mint
//! notch for your uses, an amber diamond for a change, coral words for gone,
//! quiet ink for a member count), and the marks that are not kind marks
//! (a release's tick, the highlighted name of a narrowed row).

use super::row::ReleaseMark;
use super::state::{Glyph, RowState};
use crate::shell::kit::{SaidChild, text};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, HighlightStyle,
    InspectorElementId, IntoElement, LayoutId, ParentElement, PathBuilder, Pixels, Refineable,
    RenderOnce, SharedString, Style, StyleRefinement, Styled, StyledText, UnderlineStyle, Window,
    div, point, px,
};
use std::ops::Range;

/// The side of the mint notch, at 100 % text.
const NOTCH: f32 = 6.0;
/// The side of the amber diamond, at 100 % text.
const DIAMOND: f32 = 7.0;
/// The side of a release's tick, at 100 % text.
const TICK: f32 = 8.0;

/// The state glyphs of `state`, in the order they are drawn, as one run
/// pushed to the row's right edge.
pub(super) fn trailing(state: &RowState, measure: &Measure, palette: &Palette) -> AnyElement {
    let mut run = div().ml_auto().flex_none().flex().items_center().gap(measure.space(Space::Base));
    for glyph in state.glyphs() {
        run = run.child(one(glyph, measure, palette));
    }
    run.into_any_element()
}

fn one(glyph: Glyph, measure: &Measure, palette: &Palette) -> AnyElement {
    let gap = measure.space(Space::Tight);
    match glyph {
        Glyph::Used(uses) => div()
            .flex()
            .items_center()
            .gap(gap)
            .child(notch(palette.mint.base.into(), measure))
            .child(text(ty::MONO_SMALL, measure, palette.mint.base).flex_none().whitespace_nowrap().child(uses.to_string()))
            .into_any_element(),
        Glyph::Changed(count) => {
            let mut run = div().flex().items_center().gap(gap).child(diamond(palette.amber.base.into(), measure));
            if count > 1 {
                run = run.child(text(ty::MONO_SMALL, measure, palette.amber.base).flex_none().whitespace_nowrap().child(count.to_string()));
            }
            run.into_any_element()
        }
        Glyph::Gone(gone) => text(ty::SMALL, measure, palette.coral.base).flex_none().whitespace_nowrap().child(gone.word()).into_any_element(),
        Glyph::Members(members) => text(ty::MONO_SMALL, measure, palette.ink3).flex_none().whitespace_nowrap().child(members.to_string()).into_any_element(),
    }
}

/// A square with its bottom-right corner cut: how often your code uses it.
fn notch(color: Hsla, measure: &Measure) -> impl IntoElement {
    let side = px(NOTCH * measure.scale());
    SideGlyph { shape: SideShape::Notch, color, side, style: StyleRefinement::default() }
        .flex_none()
        .size(side)
}

/// A diamond: it changes in the release being read.
fn diamond(color: Hsla, measure: &Measure) -> impl IntoElement {
    let side = px(DIAMOND * measure.scale());
    SideGlyph { shape: SideShape::Diamond, color, side, style: StyleRefinement::default() }
        .flex_none()
        .size(side)
}

#[derive(Clone, Copy)]
enum SideShape { Notch, Diamond }

struct SideGlyph {
    shape: SideShape,
    color: Hsla,
    side: Pixels,
    style: StyleRefinement,
}

impl Styled for SideGlyph {
    fn style(&mut self) -> &mut StyleRefinement { &mut self.style }
}

impl IntoElement for SideGlyph {
    type Element = Self;
    fn into_element(self) -> Self { self }
}

impl Element for SideGlyph {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = self.side.into();
        style.size.height = self.side.into();
        style.flex_shrink = 0.0;
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {}

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) {
        let mut path = PathBuilder::fill();
        match self.shape {
            SideShape::Notch => {
                let (w, h) = (bounds.size.width, bounds.size.height);
                path.move_to(bounds.origin);
                path.line_to(point(bounds.origin.x + w, bounds.origin.y));
                path.line_to(point(bounds.origin.x + w, bounds.origin.y + h * 0.6));
                path.line_to(point(bounds.origin.x + w * 0.6, bounds.origin.y + h));
                path.line_to(point(bounds.origin.x, bounds.origin.y + h));
            }
            SideShape::Diamond => {
                let center = bounds.center();
                let radius = bounds.size.width * 0.5;
                path.move_to(point(center.x, center.y - radius));
                path.line_to(point(center.x + radius, center.y));
                path.line_to(point(center.x, center.y + radius));
                path.line_to(point(center.x - radius, center.y));
            }
        }
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, self.color);
        }
    }
}

/// A release's mark: the pin in mint, the release being read in periwinkle,
/// every other one quiet. The FACET cut, small.
pub(super) fn release(mark: ReleaseMark, measure: &Measure, palette: &Palette) -> AnyElement {
    let ink: Hsla = match mark {
        ReleaseMark::Reading => palette.peri_hi.into(),
        ReleaseMark::Pin => palette.mint.base.into(),
        ReleaseMark::Other => palette.ink4.into(),
    };
    let side = px(TICK * measure.scale());
    div().flex_none().size(side).bg(ink).into_any_element()
}

/// A name with the words that matched underlined in periwinkle. It still
/// publishes the whole name to the probe (it is a [`SaidChild`]).
#[derive(IntoElement)]
pub(super) struct Marked {
    name: SharedString,
    hit: Range<usize>,
    ink: Hsla,
    underline: Hsla,
}

impl Marked {
    /// `name` with `hit` (bytes) underlined.
    pub(super) fn new(name: SharedString, hit: Range<usize>, ink: Hsla, underline: Hsla) -> Self {
        Self { name, hit, ink, underline }
    }
}

impl SaidChild for Marked {
    fn words(&self) -> Option<&str> {
        Some(&self.name)
    }
}

impl RenderOnce for Marked {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let style = HighlightStyle {
            color: Some(self.ink),
            underline: Some(UnderlineStyle { thickness: px(1.5), color: Some(self.underline), wavy: false }),
            ..HighlightStyle::default()
        };
        StyledText::new(self.name).with_highlights([(self.hit, style)])
    }
}
