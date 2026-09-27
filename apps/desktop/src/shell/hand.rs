//! The hand (D-Hand), one component at its rungs:
//!
//! - **Mark**: at rest, in the foot — each card's kind mark, the ones that
//!   feed each other joined by a hairline, the rest set apart by space, and
//!   a `›` that opens the Row. An empty hand shows nothing.
//! - **Row**: opened (H, or the `›`): each road as cards (mark + name) with
//!   the verb between them and its one sentence beneath ("from text to
//!   Table, in two steps"), then what stands apart.
//!
//! The order is the arrangement's (what feeds what), recomputed from the
//! held set; it is never stored. A card goes to its page and touches it.

use super::kit::{kind_mark, text};
use super::region::Links;
use crate::navigation::Intent;
use crate::runtime::fixture_world::HandView;
use facet::icons::KindSize;
use facet::paint::{Bevel, Chamfer, cut};
use facet::tokens::ty;
use facet::{Measure, Palette, Space};
use gpui::{
    AnyElement, ClickEvent, InteractiveElement, IntoElement, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    div, px,
};

/// Goes to card `n` and touches it.
fn go(links: &Links, view: &HandView, n: usize, cx: &mut gpui::App) {
    let Some(card) = view.cards.get(n) else { return };
    if let Some(route) = super::root::held_route(&card.held) {
        links.dispatch(Intent::Navigate(route), cx);
        links.dispatch(Intent::TouchHeld(card.held.clone(), super::root::now_ms()), cx);
    }
}

/// The Mark rung: the hand in the foot. Nothing when the hand is empty.
pub(crate) fn marks(view: &std::rc::Rc<HandView>, links: &Links, measure: &Measure, palette: &'static Palette) -> Option<AnyElement> {
    if view.cards.is_empty() {
        return None;
    }
    let scale = measure.scale();
    let open_links = links.clone();
    let mut row = div()
        .flex()
        .items_center()
        .gap(px(6.0 * scale))
        .child(
            div()
                .id("hand-open")
                .cursor_pointer()
                .child(text(ty::SMALL, measure, palette.ink3).keyed("hand-open").child("›"))
                .on_click(move |_: &ClickEvent, _, cx| open_links.shell(cx, |shell, cx| shell.toggle_hand(cx))),
        );
    let mark = |n: usize| {
        let card = &view.cards[n];
        let links = links.clone();
        let view = std::rc::Rc::clone(view);
        div()
            .id(SharedString::from(format!("hand-mark-{n}")))
            .cursor_pointer()
            .child(kind_mark(card.kind, KindSize::Sm, measure, palette))
            .on_click(move |_: &ClickEvent, _, cx| go(&links, &view, n, cx))
            .into_any_element()
    };
    for road in &view.roads {
        let mut joined = div().flex().items_center();
        for (k, &n) in road.cards.iter().enumerate() {
            if k > 0 {
                joined = joined.child(div().w(px(8.0 * scale)).h(px(1.0)).bg(palette.ink4));
            }
            joined = joined.child(mark(n));
        }
        row = row.child(joined);
    }
    for &n in &view.apart {
        row = row.child(div().pl(px(4.0 * scale)).child(mark(n)));
    }
    Some(row.into_any_element())
}

/// The Row rung: the hand opened, over the foot.
pub(crate) fn row(view: &std::rc::Rc<HandView>, links: &Links, measure: &Measure, palette: &'static Palette) -> AnyElement {
    let scale = measure.scale();
    let card = |n: usize| {
        let held = &view.cards[n];
        let links = links.clone();
        let view = std::rc::Rc::clone(view);
        cut()
            .chamfer(Chamfer::Sm)
            .bevel(Bevel::Rest)
            .fill(palette.plate)
            .id(SharedString::from(format!("hand-card-{n}")))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.0 * scale))
            .px(px(8.0 * scale))
            .h(px(26.0 * scale))
            .child(kind_mark(held.kind, KindSize::Sm, measure, palette))
            .child(text(ty::MONO_ROW, measure, palette.ink0).child(held.name.clone()))
            .on_click(move |_: &ClickEvent, _, cx| go(&links, &view, n, cx))
    };
    let mut column = cut()
        .chamfer(Chamfer::Float)
        .bevel(Bevel::Rest)
        .fill(palette.plate2)
        .id("hand-row")
        .flex()
        .flex_col()
        .gap(measure.space(Space::Base))
        .p(measure.space(Space::Roomy));
    for road in &view.roads {
        let mut line = div().flex().flex_wrap().items_center().gap(px(6.0 * scale));
        for (k, &n) in road.cards.iter().enumerate() {
            if k > 0 {
                // A direct feed is one line; a verb sits on the line.
                let verb = road.verbs.get(k - 1).cloned().unwrap_or_default();
                let join = if verb.is_empty() {
                    div().w(px(20.0 * scale)).h(px(1.0)).bg(palette.ink4)
                } else {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0 * scale))
                        .child(div().w(px(10.0 * scale)).h(px(1.0)).bg(palette.ink4))
                        .child(text(ty::SMALL, measure, palette.ink3).child(verb))
                        .child(div().w(px(10.0 * scale)).h(px(1.0)).bg(palette.ink4))
                };
                line = line.child(join);
            }
            line = line.child(card(n));
        }
        column = column
            .child(line)
            .child(text(ty::CAPTION, measure, palette.ink3).child(SharedString::from(road.sentence.clone())));
    }
    if !view.apart.is_empty() {
        let mut apart = div().flex().flex_wrap().items_center().gap(measure.space(Space::Roomy));
        for &n in &view.apart {
            apart = apart.child(card(n));
        }
        column = column.child(apart);
    }
    column.into_any_element()
}
