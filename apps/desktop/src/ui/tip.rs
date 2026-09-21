//! Tooltips: one shape for every hover explanation in the window.
//! A tip states what a control does, then the key that does it, then the
//! exact value behind it. A view builds one value; the frame draws it once.
//!
//! The previous tooltips were a monospace box holding whatever string a view
//! had to hand — a coordinate, a label with a key spliced in with two spaces,
//! a sentence — and read as debug output. A tip is typed instead: a title in
//! the interface face, an optional sentence in dim ink, an optional key hint
//! drawn as the same key tag every button uses, and an optional exact value in
//! the specimen face, so a reader learns one shape and never has to work out
//! which part of a tooltip is the copyable thing.
//!
//! A [`Card`] is the larger form for a declaration: its kind mark, its name,
//! its signature line, and its first sentence. It is what a hover over any row
//! that names a declaration shows, so the tab tree, the context panel, and a
//! member list all explain a name the same way.

use super::glyph;
use super::surface;
use super::text;
use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Space, TypeScale, space, type_size};
use crate::views::keys::Chord;
use backend_library::DeclarationKind;
use gpui::{
    AnyView, App, AppContext as _, Div, FontWeight, IntoElement, ParentElement,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use std::time::Duration;

/// How long the pointer rests before a tip appears.
const DELAY: Duration = Duration::from_millis(320);

/// Widest a tip grows, in pixels.
const MAX_WIDTH: f32 = 380.0;

/// What one hover explains.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Tip {
    title: String,
    detail: Option<String>,
    key: Option<String>,
    value: Option<String>,
}

impl Tip {
    /// Starts a tip with what the control does, in a few words.
    pub(crate) fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    /// Adds one sentence of explanation.
    pub(crate) fn detail(mut self, detail: impl Into<String>) -> Self {
        let text = detail.into();
        self.detail = (!text.trim().is_empty()).then_some(text);
        self
    }

    /// Adds the chord that does the same thing from the keyboard.
    pub(crate) fn key(mut self, chord: Chord) -> Self {
        self.key = Some(chord.label());
        self
    }

    /// Adds the exact value the control is about, never abbreviated.
    pub(crate) fn value(mut self, value: impl Into<String>) -> Self {
        let text = value.into();
        self.value = (!text.trim().is_empty()).then_some(text);
        self
    }

    /// Returns the builder a GPUI `tooltip` call takes.
    pub(crate) fn build(self) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_window, cx| {
            let theme = crate::theme::theme(cx);
            let tip = self.clone();
            cx.new(|_| TipView { theme, tip }).into()
        }
    }
}

/// Attaches a tip to any stateful element with the window's one delay.
pub(crate) trait Tipped: Sized {
    /// Explains the element on hover.
    fn tip(self, tip: Tip) -> Self;

    /// Explains a declaration on hover.
    fn card(self, card: Card) -> Self;
}

impl<E> Tipped for gpui::Stateful<E>
where
    gpui::Stateful<E>: StatefulInteractiveElement,
{
    fn tip(self, tip: Tip) -> Self {
        self.tooltip_show_delay(DELAY).tooltip(tip.build())
    }

    fn card(self, card: Card) -> Self {
        self.tooltip_show_delay(DELAY).tooltip(card.build())
    }
}

/// GPUI CE buttons own their tooltip popup and delay. Keep the product's
/// `Tip` vocabulary at the call site while routing button affordances through
/// the component implementation instead of the legacy stateful `Div` path.
impl Tipped for gpui_component::button::Button {
    fn tip(self, tip: Tip) -> Self {
        self.tooltip(tip.title)
    }

    fn card(self, card: Card) -> Self {
        self.tooltip(card.name)
    }
}

struct TipView {
    theme: Theme,
    tip: Tip,
}

impl gpui::Render for TipView {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let tip = &self.tip;
        frame(theme)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(
                        text::text_at(theme, TypeScale::Small, Paint::Silver0)
                            .font_weight(FontWeight::MEDIUM)
                            .child(tip.title.clone()),
                    )
                    .children(
                        tip.key
                            .clone()
                            .map(|key| super::button::key_hint(theme, &key)),
                    ),
            )
            .children(
                tip.detail.clone().map(|detail| {
                    text::text_at(theme, TypeScale::Tiny, Paint::Silver2).child(detail)
                }),
            )
            .children(tip.value.clone().map(|value| mono(theme, value)))
    }
}

/// A declaration explained: mark, name, kind, signature, first sentence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Card {
    name: String,
    kind: Option<DeclarationKind>,
    signature: Option<String>,
    summary: Option<String>,
    site: Option<String>,
}

impl Card {
    /// Starts a card for one named declaration.
    pub(crate) fn new(name: impl Into<String>, kind: Option<DeclarationKind>) -> Self {
        Self {
            name: name.into(),
            kind,
            signature: None,
            summary: None,
            site: None,
        }
    }

    /// Adds the flattened signature line.
    pub(crate) fn signature(mut self, signature: impl Into<String>) -> Self {
        let text = signature.into();
        self.signature = (!text.trim().is_empty()).then_some(text);
        self
    }

    /// Adds the first sentence of documentation.
    pub(crate) fn summary(mut self, summary: impl Into<String>) -> Self {
        let text = summary.into();
        self.summary = (!text.trim().is_empty()).then_some(text);
        self
    }

    /// Adds where the declaration is: a path and line, or a project.
    pub(crate) fn site(mut self, site: impl Into<String>) -> Self {
        let text = site.into();
        self.site = (!text.trim().is_empty()).then_some(text);
        self
    }

    fn build(self) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        move |_window, cx| {
            let theme = crate::theme::theme(cx);
            let card = self.clone();
            cx.new(|_| CardView { theme, card }).into()
        }
    }
}

struct CardView {
    theme: Theme,
    card: Card,
}

impl gpui::Render for CardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let card = &self.card;
        frame(theme)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(space(Space::Snug))
                    .child(glyph::kind_tile(theme, card.kind, false))
                    .child(
                        text::text_at(theme, TypeScale::Interface, Paint::Silver0)
                            .font_weight(FontWeight::MEDIUM)
                            .child(card.name.clone()),
                    )
                    .child(text::faint(theme).child(glyph::kind_label(card.kind))),
            )
            .children(
                card.signature
                    .clone()
                    .map(|signature| mono(theme, signature)),
            )
            .children(card.summary.clone().map(|summary| {
                text::text_at(theme, TypeScale::Tiny, Paint::Silver2).child(summary)
            }))
            .children(card.site.clone().map(|site| {
                text::text_at(theme, TypeScale::Micro, Paint::Silver3)
                    .font_family(theme.specimen())
                    .child(site)
            }))
    }
}

fn frame(theme: &Theme) -> Div {
    surface::raised(theme)
        .max_w(px(MAX_WIDTH))
        .px(space(Space::Base))
        .py(space(Space::Snug))
        .flex()
        .flex_col()
        .gap(px(3.0))
}

fn mono(theme: &Theme, value: String) -> Div {
    div()
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::Silver2))
        .child(value)
}
