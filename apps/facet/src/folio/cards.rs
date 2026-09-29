//! The symbol card: one declaration as a cut plate. Its kind mark and name
//! lead, the kind word sits at the right, the author's first sentence sets
//! in the serif beneath, and the badges (never code: `pub unsafe fn` is
//! never printed) close the card. Rest on it and it lifts; click or Enter
//! and it opens the declaration's page.
//!
//! In the past ([`Change`]) a card says what happened to its name at the
//! release being read: gone (coral, struck), changed (amber), new (mint), or
//! not yet there (dashed and dimmed).

use super::state::{Pick, Use};
use super::text::{ellipsis, key, one, wrap};
use crate::Set;
use crate::controls::button::{Handler, wire};
use crate::controls::state::{Touch, hover_zone, track};
use crate::icons::{Kind, KindSize, Lang, kind_mark};
use crate::marks::badges::{Badge as BadgeFacts, Glyph, Ink, Item, Reading, Shape, badge, read};
use crate::measure::{Measure, Space};
use crate::motion::spec;
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{TypeRole, ty};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Styled, Window, div, px,
};
use std::rc::Rc;

/// What happened to a name between the release you pin and the one you read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Change {
    /// It did not exist yet in the release being read (or was added since).
    New,
    /// Its signature differs.
    Changed,
    /// It is gone from the release being read.
    Gone,
    /// It does not exist yet at the release being read.
    Absent,
}

impl Change {
    /// The badge word that names it, at release `at`.
    #[must_use]
    pub fn word(self, at: &str) -> String {
        match self {
            Self::New => format!("new at {at}"),
            Self::Changed => format!("changed at {at}"),
            Self::Gone => format!("gone at {at}"),
            Self::Absent => format!("not yet at {at}"),
        }
    }
}

/// Everything a card says.
#[derive(Clone, Debug, PartialEq)]
pub struct CardFacts {
    /// The declaration's name.
    pub name: SharedString,
    /// Its kind, for the mark.
    pub kind: Kind,
    /// The kind word (`fn`, `struct`, `trait`).
    pub word: SharedString,
    /// The author's first sentence, when it has one.
    pub doc: Option<SharedString>,
    /// What the signature reads to.
    pub badges: Vec<BadgeFacts>,
    /// Your code reaches it.
    pub yours: Use,
    /// What the release being read did to it.
    pub change: Option<Change>,
}

impl CardFacts {
    /// A card read from an outline declaration.
    #[must_use]
    pub fn of(item: &Item<'_>, doc: Option<&str>) -> Self {
        let reading = read(item);
        Self::from_reading(item.name, item.kind, item.lang, &reading, doc)
    }

    /// A card from a reading already made.
    #[must_use]
    pub fn from_reading(name: &str, kind: Option<Kind>, lang: Lang, reading: &Reading, doc: Option<&str>) -> Self {
        let kind = kind.unwrap_or(match reading.shape {
            Shape::Function => Kind::Function,
            Shape::Macro => Kind::Macro,
            Shape::Contract => Kind::Trait,
            Shape::Alias => Kind::Type,
            Shape::Struct => Kind::Struct,
            Shape::Enum => Kind::Enum,
            Shape::Union => Kind::Union,
            Shape::Constant | Shape::Static => Kind::Constant,
            Shape::Module => Kind::Module,
            Shape::Item => Kind::Unknown,
        });
        Self {
            name: name.to_owned().into(),
            kind,
            word: reading.shape.word(lang).into(),
            doc: doc.map(str::trim).filter(|doc| !doc.is_empty()).map(|doc| SharedString::from(doc.to_owned())),
            badges: reading.badges.clone(),
            yours: Use::Elsewhere,
            change: None,
        }
    }

    /// Your code reaches it.
    #[must_use]
    pub const fn yours(mut self, yours: Use) -> Self {
        self.yours = yours;
        self
    }

    /// What the release being read did to it.
    #[must_use]
    pub const fn change(mut self, change: Option<Change>) -> Self {
        self.change = change;
        self
    }
}

/// The card's name.
const NAME: TypeRole = TypeRole { weight: 520.0, size: 14.0, line: 20.0, ..ty::MONO_ROW };
/// The kind word.
const KIND: TypeRole = TypeRole { size: 11.5, line: 16.0, ..ty::SMALL };
/// The author's sentence.
const DOC: TypeRole = TypeRole { size: 13.5, line: 18.5, ..ty::CAPTION };
/// The chip that says yours.
const YOURS: TypeRole = TypeRole { weight: 560.0, size: 11.0, line: 14.0, ..ty::SMALL };

/// One symbol card (see [`symbol_card`]).
#[derive(IntoElement)]
pub struct SymbolCard {
    id: ElementId,
    facts: Rc<CardFacts>,
    measure: Measure,
    at: SharedString,
    width: Option<Pixels>,
    lit: Pick,
    on_open: Option<Handler>,
}

/// A card for `facts` in a column of `measure`, remembering its hover under `id`.
#[must_use]
pub fn symbol_card(id: impl Into<ElementId>, facts: Rc<CardFacts>, measure: &Measure) -> SymbolCard {
    SymbolCard {
        id: id.into(),
        facts,
        measure: *measure,
        at: SharedString::default(),
        width: None,
        lit: Pick::Rest,
        on_open: None,
    }
}

impl SymbolCard {
    /// The release being read, for the change badge.
    #[must_use]
    pub fn at(mut self, at: impl Into<SharedString>) -> Self {
        self.at = at.into();
        self
    }

    /// A fixed width (a grid column).
    #[must_use]
    pub const fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }

    /// Shown lit (the card a click on a shingle opened).
    #[must_use]
    pub const fn lit(mut self, lit: Pick) -> Self {
        self.lit = lit;
        self
    }

    /// Called on click, Enter or Space.
    #[must_use]
    pub fn on_open(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for SymbolCard {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let facts = self.facts;
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let hover = motion.animate(track(&self.id, "hover"), if touch.hovered || self.lit == Pick::Lit { 1.0 } else { 0.0 }, spec::LIFT, window, cx);
        let focus = motion.animate(track(&self.id, "focus"), if touch.focused { 1.0 } else { 0.0 }, spec::HOVER, window, cx);

        let rest = match facts.change {
            Some(Change::Gone) => Edge::of(Bevel::Coral, palette),
            Some(Change::Changed) => Edge::of(Bevel::Amber, palette),
            Some(Change::New) => Edge::of(Bevel::Hot, palette),
            Some(Change::Absent) | None => {
                let mut edge = Edge::of(Bevel::Rest, palette);
                edge.hi = palette.line3.into();
                edge.lo = palette.line2.into();
                edge
            }
        };
        let edge = rest.mix(Edge::of(Bevel::Peri, palette), hover * 0.8).mix(Edge::of(Bevel::Focus, palette), focus);
        let fill = mix(palette.plate.into(), palette.plate2.into(), hover);
        let name_ink: Hsla = match facts.change {
            Some(Change::Gone) => palette.coral.base.into(),
            Some(Change::Absent) => palette.ink3.into(),
            _ => palette.ink0.into(),
        };

        let name = ellipsis(key(&self.id, "name"), facts.name.clone(), NAME, name_ink, &measure);
        let mut head = div()
            .flex()
            .items_center()
            .gap(measure.space(Space::Base))
            .min_w_0()
            .child(kind_mark(facts.kind, KindSize::Sm, palette))
            .child(div().flex_1().min_w_0().flex().child(name));
        if facts.yours.is_yours() {
            head = head.child(one(key(&self.id, "yours"), "you use it", YOURS, palette.mint.base, &measure));
        }
        head = head.child(one(key(&self.id, "kind"), facts.word.clone(), KIND, palette.ink3, &measure));

        let doc = match &facts.doc {
            Some(doc) => wrap(key(&self.id, "doc"), doc.clone(), DOC, palette.ink2, &measure, Some(2)).into_any_element(),
            None => div()
                .flex()
                .items_center()
                .gap(measure.space(Space::Snug))
                .child(crate::marks::badges::glyph(Glyph::Undoc, 12.0 * scale, palette.ink3))
                .child(one(key(&self.id, "doc"), "undocumented", DOC, palette.ink3, &measure))
                .into_any_element(),
        };

        let mut badges = div().mt_auto().flex().flex_wrap().gap(measure.space(Space::Tight));
        for (index, facts_badge) in facts.badges.iter().enumerate() {
            badges = badges.child(badge(key(&self.id, format!("badge-{index}")), facts_badge, &measure));
        }
        if let Some(change) = facts.change.filter(|_| !self.at.is_empty()) {
            let (glyph, ink) = match change {
                Change::Gone => (Glyph::Error, Ink::Coral),
                Change::Changed => (Glyph::Changes, Ink::Amber),
                Change::New => (Glyph::Makes, Ink::Mint),
                Change::Absent => (Glyph::Undoc, Ink::Plain),
            };
            let word = change.word(&self.at);
            let tip = match change {
                Change::Gone => "It is not in the release you are reading.",
                Change::Changed => "Its signature is different in the release you are reading.",
                Change::New => "It is not in your pin: this release added it.",
                Change::Absent => "The release you are reading came before it.",
            };
            badges = badges.child(badge(key(&self.id, "change"), &BadgeFacts { glyph, word: word.into(), tip: tip.into(), ink }, &measure));
        }

        let body = cut()
            .chamfer(Chamfer::Px(9.0 * scale))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .lift(hover * 0.6)
            .flex()
            .flex_col()
            .gap(measure.space(Space::Snug))
            .min_h(px(112.0 * scale))
            .px(measure.space(Space::Roomy))
            .pt(measure.space(Space::Roomy))
            .pb(measure.space(Space::Base))
            .when_some(self.width, |card, width| card.w(width))
            .child(head)
            .child(doc)
            .child(badges)
            .id(self.id.clone());
        let body = wire(body, &touch, self.on_open);
        let shown = if facts.change == Some(Change::Absent) { 0.55 } else { 1.0 };
        hover_zone(body.opacity(shown), &touch, 9.0 * scale, true)
    }
}

/// How many columns of cards of at least `min` px fit in `measure`, and the
/// width each takes.
#[must_use]
pub fn columns(measure: &Measure, min: f32) -> (usize, Pixels) {
    let (count, one) = measure.columns(min, Space::Roomy, 6);
    (count, one.width())
}
