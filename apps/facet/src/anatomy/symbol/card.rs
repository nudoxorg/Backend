//! Progressive disclosure: one card, anchored under what you rest on, that
//! stays while the pointer is on it. A generic parameter's pill says its
//! role in words, what it must be, and what your workspace chooses for it (a
//! click filters the list to those places); an error type says which kinds
//! it tells you.
//!
//! The float layer owns the card's hover intent (it rises after 120 ms, stays
//! while hovered, leaves after a 160 ms grace), its placement and its exit.

use super::host::{Act, Change};
use super::ink::{G, mark};
use super::key::{Key, Part, Sec};
use super::kit::{Env, ink, roles, wrapped_in};
use super::view::{Fails, Fill, Uses, View};
use crate::measure::{Measure, Set};
use crate::overlay::float::{self, FloatKind, FloatRequest};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole};
use gpui::{AnyElement, App, Bounds, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Window, div, px};
use std::collections::BTreeMap;
use std::rc::Rc;

/// A card's request for the trigger `key` at `anchor`: each trigger is its own
/// float key, so two words that open the same card do not cancel each other.
pub(super) type Request = Rc<dyn Fn(&Key, Bounds<Pixels>) -> FloatRequest>;

/// The cards a page can open, by what opens them.
#[derive(Default)]
pub(super) struct Cards {
    generics: BTreeMap<String, Request>,
    error: Option<Request>,
}

impl Cards {
    /// The card a generic's pill opens.
    pub fn generic(&self, name: &str) -> Option<Request> {
        self.generics.get(name).cloned()
    }

    /// The card an error type opens.
    pub fn error(&self) -> Option<Request> {
        self.error.clone()
    }
}

/// Everything a generic's card says, owned so the layer can rebuild it every
/// frame.
struct GenericCard {
    name: String,
    role: &'static str,
    says: String,
    bounds: Vec<(String, String)>,
    fills: Vec<(Fill, Act)>,
    elsewhere: Vec<(String, usize)>,
}

/// The page's cards, prepared from the view and the workspace's places.
pub(super) fn prepare(env: &Env<'_>, view: &View, uses: &Uses) -> Cards {
    let mut cards = Cards::default();
    let tests = env.host.ui().tests;
    for generic in &view.generics {
        let fills: Vec<(Fill, Act)> = uses
            .fills(tests)
            .into_iter()
            .take(7)
            .map(|fill| {
                // Choosing a type filters the list to those places and takes you there.
                let (filter, reveal) = (env.host.change(Change::Fill(Some(fill.ty.clone()))), env.host.reveal(Sec::Uses));
                let act: Act = Rc::new(move |window, cx| {
                    filter(window, cx);
                    reveal(window, cx);
                });
                (fill, act)
            })
            .collect();
        let data = Rc::new(GenericCard {
            name: generic.name.clone(),
            role: generic.role.says(),
            says: generic.says.clone(),
            bounds: generic.bounds.iter().map(|b| (b.name.clone(), b.means.clone())).collect(),
            fills,
            elsewhere: Vec::new(),
        });
        cards.generics.insert(generic.name.clone(), request(move |m, window, cx| generic_card(&data, m, window, cx)));
    }
    if let Some(fails) = &view.fails {
        let data = Rc::new(fails.clone());
        cards.error = Some(request(move |m, window, cx| error_card(&data, m, window, cx)));
    } else if let Some(call) = &view.call
        && let Some(fail) = &call.fails
    {
        // No section: the card still names the error and says when.
        let data = Rc::new(Fails { ty: fail.ty.clone(), when: fail.when.clone(), kinds: Vec::new(), tells: None });
        cards.error = Some(request(move |m, window, cx| error_card(&data, m, window, cx)));
    }
    cards
}

fn request(content: impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static) -> Request {
    let content = Rc::new(content);
    Rc::new(move |trigger, anchor| {
        let content = Rc::clone(&content);
        FloatRequest::new(trigger.id(), anchor, FloatKind::Peek, move |m, window, cx| content(m, window, cx)).hang_from_start().quick()
    })
}

fn k(m: &Measure, value: f32) -> Pixels {
    px(value * m.scale() * m.density().space())
}

fn text(m: &Measure, key: &Key, role: TypeRole, color: Hsla, content: impl Into<SharedString>) -> AnyElement {
    wrapped_in(m, key, content, role, color)
}

fn label(m: &Measure, key: &Key, color: Hsla, content: &str) -> AnyElement {
    div().mt(k(m, 12.0)).mb(k(m, 6.0)).child(text(m, key, roles::CARD_LABEL, color, content.to_uppercase())).into_any_element()
}

fn pill(m: &Measure, p: &Palette, name: &str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(20.0 * m.scale()))
        .h(px(20.0 * m.scale()))
        .px(k(m, 6.0))
        .bg(p.f_con.hue.hsla())
        .set(roles::PILL, m)
        .text_color(p.g0.hsla())
        .child(SharedString::from(name.to_owned()))
        .into_any_element()
}

fn frame(m: &Measure, body: Vec<AnyElement>) -> AnyElement {
    let width = k(m, 360.0).min(m.width());
    div().w(width).flex().flex_col().pt(k(m, 12.0)).px(k(m, 14.0)).pb(k(m, 12.0)).children(body).into_any_element()
}

fn generic_card(data: &GenericCard, m: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    float::title(SharedString::from(data.name.clone()), window, cx);
    let p = cx.facet().palette();
    let i = ink(p);
    let key = Key::of(Part::Card).field("gen");
    let mut body: Vec<AnyElement> = Vec::new();
    body.push(
        div()
            .flex()
            .items_center()
            .gap(k(m, 9.0))
            .child(pill(m, p, &data.name))
            .child(text(m, &key.field("title"), roles::CARD_TITLE, i.ink0, data.role))
            .into_any_element(),
    );
    body.push(div().mt(k(m, 6.0)).child(text(m, &key.field("says"), roles::BODY, i.ink1, data.says.clone())).into_any_element());
    if !data.bounds.is_empty() {
        body.push(label(m, &key.field("must"), i.ink3, "It must be"));
        for (n, (name, means)) in data.bounds.iter().enumerate() {
            let bound = key.field("bound").at(n);
            body.push(
                div()
                    .id(bound.id())
                    .flex()
                    .items_center()
                    .gap(k(m, 8.0))
                    .min_h(k(m, 24.0))
                    .child(mark(G::Verb(super::view::Verb::Implements), p, 14.0 * m.scale()))
                    .child(text(m, &bound.field("name"), roles::PACKAGE, i.ink0, name.clone()))
                    .child(text(m, &bound.field("means"), roles::DOC, i.ink2, means.clone()))
                    .into_any_element(),
            );
        }
    }
    if !data.fills.is_empty() {
        body.push(label(m, &key.field("chooses"), i.ink3, "Your workspace chooses"));
        let most = data.fills.iter().map(|(fill, _)| fill.places).max().unwrap_or(1).max(1);
        for (n, (fill, act)) in data.fills.iter().enumerate() {
            let shown = fill.packages.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
            let shown = if fill.packages.len() > 3 { format!("{shown}…") } else { shown };
            let act = Rc::clone(act);
            let bar = (40.0 * m.scale() * fill.places as f32 / most as f32).max(2.0);
            let row = key.field("fill").at(n);
            body.push(
                div()
                    .id(row.id())
                    .flex()
                    .items_center()
                    .gap(k(m, 8.0))
                    .min_h(k(m, 24.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(i.g2))
                    .on_click(move |_, window, cx| act(window, cx))
                    .child(text(m, &row.field("type"), roles::PACKAGE, i.ink0, fill.ty.clone()))
                    .child(div().min_w_0().flex_1().child(text(m, &row.field("packages"), roles::WRITTEN, i.mint, shown)))
                    .child(div().h(px(5.0)).w(px(bar)).bg(i.violet))
                    .child(text(m, &row.field("places"), roles::WRITTEN, i.ink1, fill.places.to_string()))
                    .into_any_element(),
            );
        }
        body.push(div().mt(k(m, 8.0)).child(text(m, &key.field("click"), roles::ASIDE_SMALL, i.ink3, "Click a type to see those places.")).into_any_element());
    }
    if !data.elsewhere.is_empty() {
        body.push(label(m, &key.field("elsewhere-head"), i.ink3, "Elsewhere in the registry"));
        let line = data.elsewhere.iter().take(6).map(|(ty, n)| format!("{ty} {n}")).collect::<Vec<_>>().join(" · ");
        body.push(text(m, &key.field("elsewhere"), roles::ASIDE_SMALL, i.ink3, line));
    }
    frame(m, body)
}

fn error_card(data: &Fails, m: &Measure, window: &mut Window, cx: &mut App) -> AnyElement {
    float::title(SharedString::from(data.ty.word.clone()), window, cx);
    let p = cx.facet().palette();
    let i = ink(p);
    let key = Key::of(Part::Card).field("err");
    let mut body: Vec<AnyElement> = Vec::new();
    body.push(
        div()
            .flex()
            .items_center()
            .gap(k(m, 9.0))
            .child(mark(G::Fail, p, 14.0 * m.scale()))
            .child(text(m, &key.field("title"), roles::CARD_TITLE, i.coral, data.ty.word.clone()))
            .into_any_element(),
    );
    if !data.when.trim().is_empty() {
        let plain = crate::anatomy::symbol::derive::plain_text(&data.when);
        body.push(div().mt(k(m, 6.0)).child(text(m, &key.field("when"), roles::BODY, i.ink1, plain)).into_any_element());
    }
    if !data.kinds.is_empty() {
        body.push(label(m, &key.field("which"), i.ink3, "It tells you which"));
        for (n, kind) in data.kinds.iter().enumerate() {
            let dim = kind.impossible.is_some();
            let row = key.field("kind").at(n);
            body.push(
                div()
                    .id(row.id())
                    .flex()
                    .items_center()
                    .gap(k(m, 8.0))
                    .min_h(k(m, 24.0))
                    .child(mark(if dim { G::Impossible } else { G::Fail }, p, 11.0 * m.scale()))
                    .child(text(m, &row.field("name"), roles::PACKAGE, if dim { i.ink3 } else { i.ink0 }, kind.name.clone()))
                    .child(text(m, &row.field("doc"), roles::DOC, if dim { i.ink3 } else { i.ink2 }, kind.doc.clone()))
                    .into_any_element(),
            );
        }
    }
    frame(m, body)
}
