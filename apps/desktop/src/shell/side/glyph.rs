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
    AnyElement, App, Bounds, HighlightStyle, Hsla, IntoElement, ParentElement, PathBuilder, Pixels,
    RenderOnce, SharedString, Styled, StyledText, UnderlineStyle, Window, canvas, div, point, px,
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
    let mut run = div()
        .ml_auto()
        .flex_none()
        .flex()
        .items_center()
        .gap(measure.space(Space::Base));
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
            .child(
                text(ty::MONO_SMALL, measure, palette.mint.base)
                    .flex_none()
                    .whitespace_nowrap()
                    .child(uses.to_string()),
            )
            .into_any_element(),
        Glyph::Changed(count) => {
            let mut run = div()
                .flex()
                .items_center()
                .gap(gap)
                .child(diamond(palette.amber.base.into(), measure));
            if count > 1 {
                run = run.child(
                    text(ty::MONO_SMALL, measure, palette.amber.base)
                        .flex_none()
                        .whitespace_nowrap()
                        .child(count.to_string()),
                );
            }
            run.into_any_element()
        }
        Glyph::Gone(gone) => text(ty::SMALL, measure, palette.coral.base)
            .flex_none()
            .whitespace_nowrap()
            .child(gone.word())
            .into_any_element(),
        Glyph::Members(members) => text(ty::MONO_SMALL, measure, palette.ink3)
            .flex_none()
            .whitespace_nowrap()
            .child(members.to_string())
            .into_any_element(),
    }
}

/// A square with its bottom-right corner cut: how often your code uses it.
fn notch(color: Hsla, measure: &Measure) -> impl IntoElement {
    let side = px(NOTCH * measure.scale());
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let (w, h) = (bounds.size.width, bounds.size.height);
            let mut path = PathBuilder::fill();
            path.move_to(bounds.origin);
            path.line_to(point(bounds.origin.x + w, bounds.origin.y));
            path.line_to(point(bounds.origin.x + w, bounds.origin.y + h * 0.6));
            path.line_to(point(bounds.origin.x + w * 0.6, bounds.origin.y + h));
            path.line_to(point(bounds.origin.x, bounds.origin.y + h));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(side)
}

/// A diamond: it changes in the release being read.
fn diamond(color: Hsla, measure: &Measure) -> impl IntoElement {
    let side = px(DIAMOND * measure.scale());
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let c = bounds.center();
            let r = bounds.size.width * 0.5;
            let mut path = PathBuilder::fill();
            path.move_to(point(c.x, c.y - r));
            path.line_to(point(c.x + r, c.y));
            path.line_to(point(c.x, c.y + r));
            path.line_to(point(c.x - r, c.y));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(side)
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
        Self {
            name,
            hit,
            ink,
            underline,
        }
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
            underline: Some(UnderlineStyle {
                thickness: px(1.5),
                color: Some(self.underline),
                wavy: false,
            }),
            ..HighlightStyle::default()
        };
        StyledText::new(self.name).with_highlights([(self.hit, style)])
    }
}
