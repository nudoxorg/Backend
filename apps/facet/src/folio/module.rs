//! A module opened: its names as cards. A small module unfurls inline (just
//! the grid); a big one is a page of its own: the module's path at the top
//! in display type, its own words beneath, a line of what it holds, then the
//! cards grouped by what they are (types, functions, traits, values).

use super::cards::{CardFacts, symbol_card};
use super::flight::Marks;
use super::state::{Extent, Pick};
use super::text::{key, one, wrap};
use crate::fluid::Modes;
use crate::measure::{Measure, Space};
use crate::motion::Flow;
use crate::tokens::fluid::FOLIO_CARDS;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Family, TypeRole, ty};
use gpui::{AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div};
use std::rc::Rc;

const PATH: TypeRole = TypeRole { face: Face::Display, weight: 700.0, size: 34.0, line: 40.0, tracking: -0.02, italic: false };
const DOC: TypeRole = TypeRole { size: 18.0, line: 26.0, ..ty::LEDE };
const STAT: TypeRole = TypeRole { size: 12.5, line: 18.0, ..ty::SMALL };
const STAT_NUMBER: TypeRole = TypeRole { weight: 600.0, size: 13.0, line: 18.0, ..ty::MONO_SMALL };
const GROUP: TypeRole = TypeRole { weight: 620.0, size: 11.0, line: 14.0, tracking: 0.08, ..ty::LABEL };

/// What a card does to its host: open item `index`.
pub type Open = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// The host's hook on each card (keyboard targets, prefetch).
pub type Wrap = Rc<dyn Fn(usize, AnyElement) -> AnyElement>;

/// An opened module (see [`module`]).
#[derive(IntoElement)]
pub struct ModuleView {
    id: ElementId,
    package: SharedString,
    name: SharedString,
    doc: Option<SharedString>,
    cards: Vec<Rc<CardFacts>>,
    at: SharedString,
    extent: Extent,
    lit: Option<usize>,
    measure: Measure,
    on_open: Option<Open>,
    wrap: Option<Wrap>,
    marks: Option<Marks>,
    carried: f32,
}

/// The module `name` of `package`, its `cards` set in `measure`.
#[must_use]
pub fn module(id: impl Into<ElementId>, package: impl Into<SharedString>, name: impl Into<SharedString>, cards: Vec<Rc<CardFacts>>, measure: &Measure) -> ModuleView {
    ModuleView {
        id: id.into(),
        package: package.into(),
        name: name.into(),
        doc: None,
        cards,
        at: SharedString::default(),
        extent: Extent::Inline,
        lit: None,
        measure: *measure,
        on_open: None,
        wrap: None,
        marks: None,
        carried: 1.0,
    }
}

impl ModuleView {
    /// The module's own first sentence.
    #[must_use]
    pub fn doc(mut self, doc: Option<SharedString>) -> Self {
        self.doc = doc;
        self
    }

    /// A page of its own (head and groups), not an inline unfurl.
    #[must_use]
    pub const fn extent(mut self, extent: Extent) -> Self {
        self.extent = extent;
        self
    }

    /// The release being read, for the cards' change badges.
    #[must_use]
    pub fn at(mut self, at: impl Into<SharedString>) -> Self {
        self.at = at.into();
        self
    }

    /// The card a click on a shingle asked for, lit for a moment.
    #[must_use]
    pub const fn lit(mut self, lit: Option<usize>) -> Self {
        self.lit = lit;
        self
    }

    /// Where the cards report their marks, for shingles flying to them.
    #[must_use]
    pub fn marks(mut self, marks: &Marks) -> Self {
        self.marks = Some(marks.clone());
        self
    }

    /// How far the module's shingles have come (`0..=1`).
    #[must_use]
    pub const fn carried(mut self, progress: f32) -> Self {
        self.carried = progress;
        self
    }

    /// Called with the card opened.
    #[must_use]
    pub fn on_open(mut self, open: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(open));
        self
    }

    /// The host's hook on each card.
    #[must_use]
    pub fn wrap(mut self, wrap: impl Fn(usize, AnyElement) -> AnyElement + 'static) -> Self {
        self.wrap = Some(Rc::new(wrap));
        self
    }
}

/// The group a card belongs to.
#[must_use]
pub fn group_of(facts: &CardFacts) -> &'static str {
    match facts.kind.family() {
        Family::Callable => "Functions",
        Family::Contract => "Traits",
        Family::Value => "Values",
        Family::Type | Family::Namespace => "Types",
    }
}

const ORDER: [&str; 4] = ["Types", "Functions", "Traits", "Values"];

impl RenderOnce for ModuleView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        // As many columns as fit; a change of count carries the cards to their
        // new places instead of jumping them.
        let columns = Modes::keyed(key(&self.id, "modes"), window, cx).columns(&FOLIO_CARDS, measure.fluid_room(), measure.space(Space::Roomy));
        let width = columns.column.width();
        let flow = Flow::scoped("folio-cards", cx);
        flow.epoch((columns.epoch, columns.count));
        let card = |index: usize| {
            let facts = self.cards[index].clone();
            let mut card = symbol_card(key(&self.id, format!("card-{index}")), facts, &measure).width(width).at(self.at.clone()).lit(if self.lit == Some(index) { Pick::Lit } else { Pick::Rest }).carried(self.carried);
            if let Some(marks) = &self.marks {
                card = card.mark(marks, index);
            }
            if let Some(open) = self.on_open.clone() {
                card = card.on_open(move |window, cx| open(index, window, cx));
            }
            let element = card.into_any_element();
            let element = match &self.wrap {
                Some(wrap) => wrap(index, element),
                None => element,
            };
            flow.item(key(&self.id, format!("flow-{index}")), element).into_any_element()
        };
        let grid = |indices: &[usize]| {
            div().flex().flex_wrap().gap(measure.space(Space::Roomy)).children(indices.iter().map(|i| card(*i)))
        };
        if self.extent == Extent::Inline {
            let all: Vec<usize> = (0..self.cards.len()).collect();
            return div().flex().flex_col().child(grid(&all)).into_any_element();
        }
        let mut groups: Vec<(&str, Vec<usize>)> = ORDER.iter().map(|g| (*g, Vec::new())).collect();
        for (i, facts) in self.cards.iter().enumerate() {
            let group = group_of(facts);
            if let Some(slot) = groups.iter_mut().find(|(name, _)| *name == group) {
                slot.1.push(i);
            }
        }
        let mut stats = div().flex().flex_wrap().items_center().gap_x(measure.space(Space::Gutter));
        for (name, indices) in groups.iter().filter(|(_, v)| !v.is_empty()) {
            stats = stats.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(measure.space(Space::Snug))
                    .child(one(key(&self.id, format!("stat-n-{name}")), indices.len().to_string(), STAT_NUMBER, palette.ink0, &measure))
                    .child(one(key(&self.id, format!("stat-{name}")), name.to_lowercase(), STAT, palette.ink3, &measure)),
            );
        }
        let head = div()
            .flex()
            .flex_col()
            .min_w_0()
            .w(measure.width())
            .gap(measure.space(Space::Snug))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_baseline()
                    .child(wrap(key(&self.id, "package"), self.package.clone(), PATH, palette.ink3, &measure, None))
                    .child(one(key(&self.id, "sep"), "::", PATH, palette.ink4, &measure))
                    .child(wrap(key(&self.id, "title"), self.name.clone(), PATH, palette.ink0, &measure, None)),
            )
            .child(match &self.doc {
                Some(doc) => wrap(key(&self.id, "doc"), doc.clone(), DOC, palette.ink2, &measure, None).into_any_element(),
                None => wrap(key(&self.id, "nodoc"), "This module has no description of its own.", ty::CAPTION, palette.ink3, &measure, None).into_any_element(),
            })
            .child(stats);
        let mut page = div().flex().flex_col().gap(measure.space(Space::Wide)).child(head);
        for (name, indices) in groups.iter().filter(|(_, v)| !v.is_empty()) {
            page = page.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(measure.space(Space::Roomy))
                    .child(one(key(&self.id, format!("group-{name}")), name.to_uppercase(), GROUP, palette.ink3, &measure))
                    .child(grid(indices)),
            );
        }
        page.into_any_element()
    }
}
