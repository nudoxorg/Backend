//! Peek (`v6/cohesion/COHESION.md`, "The sidebar: primitives", 5). Arrow keys
//! move a selection and a peek follows it beside the sidebar (Finder's
//! column view with Quick Look). Space toggles it. It shows the signature,
//! who uses it (per crate of yours, with bars) and what the release being
//! read does to it, and its foot names the keys: ↵ opens, → shows members,
//! H holds, G G shows the graph.
//!
//! It is one card of the float layer (`facet::overlay::float`): the same
//! peek card every other peek in the shell is (`shell::peeks`), with the
//! sidebar's sections under it, placed to the right of the row and morphing
//! from row to row as the selection moves. No card of its own is drawn here.

use super::state::{Move, StateBook, Usage, WorkspaceCrate};
use crate::model::pages::{PageKey, SymbolRef};
use crate::runtime::store::DataStore;
use crate::shell::kit::text;
use crate::shell::peeks;
use facet::overlay::float::{FloatRequest, Side};
use facet::tokens::ty;
use facet::{ActiveFacet as _, Measure, Palette, Space};
use gpui::{AnyElement, App, Bounds, ElementId, Entity, IntoElement, ParentElement, Pixels, SharedString, Styled, div, px};
use std::collections::BTreeMap;
use std::rc::Rc;

/// How many crates of yours a peek names before it stops.
const USERS_SHOWN: usize = 6;
/// The width of the column the crate names are set in, at 100 % text.
const NAME_COLUMN: f32 = 112.0;
/// The length of the longest bar, at 100 % text.
const BAR_LONGEST: f32 = 96.0;
/// The thickness of a bar, at 100 % text.
const BAR_THICKNESS: f32 = 4.0;

/// The float key the sidebar's peek opens under. One key for every row: the
/// card morphs from row to row instead of closing and opening.
const KEY: &str = "side-peek";

/// The peek's float key.
pub(super) fn key() -> ElementId {
    ElementId::Name(SharedString::new_static(KEY))
}

/// What the peek shows beyond the card every peek shares.
#[derive(Clone)]
pub(super) struct Extras {
    /// The declaration, when the row is one.
    pub symbol: Option<SymbolRef>,
    /// Its path in the release data.
    pub path: Option<SharedString>,
    /// What the release data knows.
    pub book: Rc<StateBook>,
    /// The store the page is read from each frame.
    pub store: Entity<DataStore>,
    /// Whether → scopes into the row (members).
    pub hoists: bool,
}

/// The float request for the peek of `page`, beside `anchor` (the row).
pub(super) fn request(page: PageKey, label: SharedString, anchor: Bounds<Pixels>, extras: Extras) -> FloatRequest {
    let mut request = peeks::request(page, label, anchor, extras.store.clone());
    request.key = key();
    request.side = Side::Right;
    let card = Rc::clone(&request.content);
    request.content = Rc::new(move |measure, window, cx| {
        let card = card(measure, window, cx);
        column(card, &extras, measure, cx)
    });
    request
}

fn column(card: AnyElement, extras: &Extras, measure: &Measure, cx: &App) -> AnyElement {
    let palette = cx.facet().palette();
    // The card pads its own content (the plate has no padding of its own), so
    // what is added under it carries the same side and bottom padding.
    let mut sections = div().flex().flex_col().gap(measure.space(Space::Roomy)).px(measure.space(Space::Gutter)).pb(measure.space(Space::Gutter));
    let users = users(extras, cx);
    if !users.is_empty() {
        sections = sections.child(users_block(&users, measure, palette));
    }
    if let Some(block) = change_block(extras, measure, palette) {
        sections = sections.child(block);
    }
    let sections = sections.child(foot(extras, measure, palette));
    div().flex().flex_col().child(card).child(sections).into_any_element()
}

/// Which of your crates use the row, and how often, the busiest first: from
/// the release data when it knows the item, else from the lines the page
/// read for the declaration.
fn users(extras: &Extras, cx: &App) -> Vec<Usage> {
    if let Some(path) = &extras.path {
        let known = extras.book.users_of(path);
        if !known.is_empty() {
            return known;
        }
    }
    let Some(symbol) = &extras.symbol else { return Vec::new() };
    let resource = extras.store.read(cx).symbol(symbol);
    let Some(page) = resource.loaded_value() else { return Vec::new() };
    let mut by: BTreeMap<WorkspaceCrate, u32> = BTreeMap::new();
    for line in page.workspace.iter() {
        *by.entry(WorkspaceCrate::new(line.package.to_string())).or_insert(0) += 1;
    }
    let mut users: Vec<Usage> = by.into_iter().map(|(by, uses)| Usage { by, uses }).collect();
    users.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.by.cmp(&b.by)));
    users
}

fn users_block(users: &[Usage], measure: &Measure, palette: &Palette) -> AnyElement {
    let small = |uses: u32| f32::from(u16::try_from(uses).unwrap_or(u16::MAX));
    let most = small(users.iter().map(|usage| usage.uses).max().unwrap_or(1)).max(1.0);
    let bar = BAR_LONGEST * measure.scale();
    let mut block = div().flex().flex_col().gap(measure.space(Space::Tight)).child(text(ty::LABEL, measure, palette.ink3).child("YOUR CODE"));
    for usage in users.iter().take(USERS_SHOWN) {
        let share = small(usage.uses) / most;
        block = block.child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Base))
                .child(text(ty::MONO_SMALL, measure, palette.ink2).w(px(NAME_COLUMN * measure.scale())).flex_none().overflow_hidden().whitespace_nowrap().text_ellipsis().child(usage.by.shared()))
                .child(div().flex_none().w(px(bar * share)).h(px(BAR_THICKNESS * measure.scale())).bg(palette.mint.base).opacity(0.8))
                .child(text(ty::MONO_SMALL, measure, palette.mint.base).ml_auto().child(usage.uses.to_string())),
        );
    }
    block.into_any_element()
}

/// What the release being read does to the row: the two signatures.
fn change_block(extras: &Extras, measure: &Measure, palette: &Palette) -> Option<AnyElement> {
    let movement = extras.book.movement(extras.path.as_deref()?)?;
    let to = extras.book.compared().map(|compared| compared.to.clone()).unwrap_or_default();
    let (label, ink) = match movement.kind {
        Move::Changed => (format!("CHANGES IN {to}"), palette.amber.base),
        Move::Gone(gone) => (format!("{} IN {to}", gone.word().to_uppercase()), palette.coral.base),
    };
    let mut block = div().flex().flex_col().gap(measure.space(Space::Hair)).child(text(ty::LABEL, measure, ink).child(label));
    if let Some(before) = &movement.before {
        block = block.child(text(ty::MONO_SMALL, measure, palette.ink3).line_through().child(before.clone()));
    }
    if let Some(after) = movement.after.as_ref().filter(|_| movement.kind == Move::Changed) {
        block = block.child(text(ty::MONO_SMALL, measure, palette.ink1).child(after.clone()));
    }
    Some(block.into_any_element())
}

/// The keys the peek answers to.
fn foot(extras: &Extras, measure: &Measure, palette: &Palette) -> AnyElement {
    let mut keys: Vec<(&'static str, &'static str)> = vec![("↵", "open")];
    if extras.hoists {
        keys.push(("→", "members"));
    }
    keys.push(("H", "hold"));
    keys.push(("G G", "graph"));
    let mut foot = div().flex().flex_wrap().gap(measure.space(Space::Roomy));
    for (cap, words) in keys {
        foot = foot.child(
            div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Snug))
                .child(facet::controls::kbd(cap, measure))
                .child(text(ty::SMALL, measure, palette.ink3).child(words)),
        );
    }
    foot.into_any_element()
}
