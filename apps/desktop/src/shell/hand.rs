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
use crate::runtime::fixture_world::Card;
use facet::motion::presence::{Act, Axis, Entry, Extent};
use facet::motion::{Keys, Pose, Presence};
use std::collections::HashMap;
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

/// The key a card's mark travels under when it is taken (Take → hand): the
/// shell remembers the hero stone's bounds under it, and the foot's new mark
/// starts over them. Distinct from the declaration's own shared key, so the
/// hero ↔ graph-node morph never starts from the foot.
pub(crate) fn take_key(coordinate: &str) -> gpui::ElementId {
    gpui::ElementId::Name(SharedString::from(format!("hand:{coordinate}")))
}

/// The digits ⌘ shows over the cards, in shown order (⌘1–⌘5 go to them).
const DIGITS: [&str; 5] = ["1", "2", "3", "4", "5"];

/// How long the stone takes to reach the hand.
const TAKE: std::time::Duration = std::time::Duration::from_millis(380);

/// Goes to card `n` and touches it.
fn go(links: &Links, view: &HandView, n: usize, cx: &mut gpui::App) {
    let Some(card) = view.cards.get(n) else { return };
    if let Some(route) = super::root::held_route(&card.held) {
        links.dispatch(Intent::Navigate(route), cx);
        links.dispatch(Intent::TouchHeld(card.held.clone(), super::root::now_ms()), cx);
    }
}

/// A card's identity on the Mark rung (its Presence key).
fn card_key(held: &crate::model::hand::Held) -> gpui::ElementId {
    let id = held.id.as_ref().map_or("", |id| id.as_str());
    gpui::ElementId::Name(SharedString::from(format!("{}|{id}", held.package.as_str())))
}

/// A new mark arrives still: Take → hand already moved it.
fn arrive() -> Act {
    let still = std::time::Duration::from_millis(1);
    Act {
        duration: still,
        pose: Keys::owned(still, vec![(0.0, Pose::REST), (1.0, Pose::REST)], facet::tokens::motion::GLIDE),
        room: Keys::owned(still, vec![(0.0, Extent::FULL), (1.0, Extent::FULL)], facet::tokens::motion::GLIDE),
    }
}

/// Let go: the stone sinks through the foot's floor (0–200 ms, clipped by
/// the window's edge; no fade), and from 160 ms the room it held closes, so
/// the rest travel from where they were painted.
fn let_go() -> Act {
    let whole = std::time::Duration::from_millis(360);
    let sunk = Pose { y: 18.0, ..Pose::REST };
    Act {
        duration: whole,
        pose: Keys::owned(whole, vec![(0.0, Pose::REST), (0.55, sunk), (1.0, sunk)], facet::tokens::motion::DROP),
        room: Keys::owned(whole, vec![(0.0, Extent::FULL), (0.45, Extent::FULL), (1.0, Extent::NONE)], facet::tokens::motion::GLIDE),
    }
}

/// The hand at rest in the foot (the Mark rung), and what it remembers of
/// the cards it drew (a card let go is drawn on its way out).
pub(crate) struct Marks {
    presence: Presence,
    drawn: HashMap<gpui::ElementId, (Card, bool)>,
}

impl Default for Marks {
    fn default() -> Self {
        Self { presence: Presence::new("hand.marks").axis(Axis::Horizontal), drawn: HashMap::new() }
    }
}

impl Marks {
    /// How many cards the rung draws (leaving ones included).
    #[cfg(test)]
    pub(crate) fn drawn(&self) -> usize {
        self.drawn.len()
    }

    /// The Mark rung: each card's kind mark in shown order, the ones on one
    /// road joined by a hairline, a `›` that opens the Row. Nothing once
    /// the hand is empty and the last card has gone.
    pub(crate) fn render(
        &mut self,
        view: &std::rc::Rc<HandView>,
        links: &Links,
        measure: &Measure,
        palette: &'static Palette,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Option<AnyElement> {
        let scale = measure.scale();
        // The card each shown card follows on its road (for the hairline).
        let mut joined: HashMap<usize, bool> = HashMap::new();
        for road in &view.roads {
            for (k, &n) in road.cards.iter().enumerate() {
                joined.insert(n, k > 0);
            }
        }
        let order: Vec<usize> = view.roads.iter().flat_map(|road| road.cards.iter().copied()).chain(view.apart.iter().copied()).collect();
        for &n in &order {
            let card = &view.cards[n];
            self.drawn.insert(card_key(&card.held), (card.clone(), joined.get(&n).copied().unwrap_or(false)));
        }
        let items = self.presence.sync_entries(
            order.iter().map(|&n| Entry::new(card_key(&view.cards[n].held)).enter(arrive()).exit(let_go())),
            window,
            cx,
        );
        self.drawn.retain(|key, _| items.iter().any(|item| &item.key == key));
        if items.is_empty() {
            return None;
        }
        let open_links = links.clone();
        // Not clipped: the digits rise above the foot while ⌘ is held; the
        // window's own floor clips a stone that sinks.
        let mut row = div()
            .flex()
            .items_center()
            .child(
                div()
                    .id("hand-open")
                    .cursor_pointer()
                    .pr(px(6.0 * scale))
                    .child(text(ty::SMALL, measure, palette.ink3).keyed("hand-open").child("›"))
                    .on_click(move |_: &ClickEvent, _, cx| open_links.shell(cx, |shell, cx| shell.toggle_hand(cx))),
            );
        let keys = facet::ActiveFacet::facet(cx).reveal.keys;
        for item in &items {
            let Some((card, follows)) = self.drawn.get(&item.key).cloned() else { continue };
            let shown = view.cards.iter().position(|shown| shown.held.same(&card.held));
            // ⌘ held: each card's digit (⌘1–⌘5 go to the cards in shown order).
            let digit = shown.filter(|_| keys && !item.is_leaving()).and_then(|n| DIGITS.get(n).copied());
            let mark = kind_mark(card.kind, KindSize::Sm, measure, palette);
            // Only the text-free stone travels (the no-overlap law).
            let mark: AnyElement = match (&card.held.id, item.is_leaving()) {
                (Some(id), false) => facet::motion::shared::shared(take_key(id.as_str()), mark)
                    .timing(TAKE, facet::tokens::motion::GLIDE)
                    .into_any_element(),
                _ => mark,
            };
            let links = links.clone();
            let view = std::rc::Rc::clone(view);
            let mut cell = div().flex().items_center();
            cell = if follows {
                cell.child(div().w(px(8.0 * scale)).h(px(1.0)).bg(palette.ink4))
            } else {
                cell.pl(px(6.0 * scale))
            };
            cell = cell.child(
                div()
                    .id(SharedString::from(format!("hand-mark-{}", item.index)))
                    .relative()
                    .cursor_pointer()
                    .child(mark)
                    // The digit sits centred above its own mark, so the caps
                    // never crowd one another and nothing moves.
                    .children(digit.map(|label| {
                        let cap = facet::controls::kbd(label, measure).size(facet::controls::KbdSize::Small).hot();
                        div()
                            .absolute()
                            .left(px(-6.0 * scale))
                            .right(px(-6.0 * scale))
                            .bottom(px(16.0 * scale))
                            .flex()
                            .justify_center()
                            .child(facet::probe::text(
                                gpui::ElementId::Name(SharedString::from(format!("hand-digit:{label}"))),
                                label,
                                measure.role(ty::MONO_SMALL),
                                1.0,
                                facet::probe::TextOverflow::Clip,
                                cap,
                            ))
                    }))
                    .on_click(move |_: &ClickEvent, _, cx| {
                        if let Some(n) = shown {
                            go(&links, &view, n, cx);
                        }
                    }),
            );
            row = row.child(item.slot(cell));
        }
        Some(row.into_any_element())
    }
}

/// The Row rung: the hand opened, over the foot.
pub(crate) fn row(view: &std::rc::Rc<HandView>, at: usize, links: &Links, measure: &Measure, palette: &'static Palette) -> AnyElement {
    let scale = measure.scale();
    let card = |n: usize| {
        let held = &view.cards[n];
        let links = links.clone();
        let view = std::rc::Rc::clone(view);
        cut()
            .chamfer(Chamfer::Sm)
            // The card the keyboard stands on wears the focus bevel.
            .bevel(if n == at { Bevel::Focus } else { Bevel::Rest })
            .fill(palette.plate)
            .id(SharedString::from(format!("hand-card-{n}")))
            .cursor_pointer()
            .flex()
            .items_center()
            .gap(px(6.0 * scale))
            .px(px(8.0 * scale))
            .h(px(26.0 * scale))
            .child(kind_mark(held.kind, KindSize::Sm, measure, palette))
            .child(text(ty::MONO_ROW, measure, palette.ink0).keyed(SharedString::from(format!("hand-row:card:{n}"))).child(held.name.clone()))
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
                        .child(text(ty::SMALL, measure, palette.ink3).keyed(SharedString::from(format!("hand-row:verb:{n}"))).child(verb))
                        .child(div().w(px(10.0 * scale)).h(px(1.0)).bg(palette.ink4))
                };
                line = line.child(join);
            }
            line = line.child(card(n));
        }
        column = column
            .child(line)
            .child(
                text(ty::CAPTION, measure, palette.ink3)
                    .keyed(SharedString::from(format!("hand-row:sentence:{}", road.cards.first().copied().unwrap_or_default())))
                    .child(SharedString::from(road.sentence.clone())),
            );
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
